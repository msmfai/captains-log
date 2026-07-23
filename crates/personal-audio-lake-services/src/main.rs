use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use clap::Parser;
use personal_audio_lake_contracts::{
    ActionProposal, DocumentKind, Envelope, Failure, HealthState, Message, SegmentEvidence,
    WorkerHealth, WorkerRole, WorkflowAction, WorkflowAdapter, WorkflowDefinition,
    WorkflowEffectUndo, WorkflowInput, WorkflowOutputField, WorkflowOutputKind, WorkflowRunRecord,
    WorkflowRunRequest, WorkflowRunRequester, WorkflowSelection, WorkflowSelector, WorkflowState,
    WorkflowStepRecord, is_required_local_model_size, ports::Oracle,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader as AsyncBufReader};
use tokio::time::{Duration, interval};
use uuid::Uuid;

#[derive(Parser)]
struct Cli {
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_STATE_ROOT")]
    state_root: PathBuf,
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_ROOT")]
    lake_root: PathBuf,
    #[arg(long, default_value_t = 1_000)]
    poll_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Checkpoint {
    cursor: usize,
}

#[derive(Serialize)]
struct EffectPayload<'a> {
    workflow_id: &'a str,
    run_id: Uuid,
    recording_id: Uuid,
    transition_id: &'a str,
    extended_state: &'a BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct SystemActionLink {
    run_id: Uuid,
    transition_id: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DocumentReceipt {
    kind: DocumentKind,
    external_id: String,
    destination: String,
    summary: String,
}

struct Runner {
    state_root: PathBuf,
    manifest: PathBuf,
    http: reqwest::Client,
}

struct ModelOracleContext {
    state: WorkflowState,
    instruction: String,
    extended: BTreeMap<String, String>,
}

struct LocalModelOracle<'a> {
    runner: &'a Runner,
}

impl Oracle<ModelOracleContext, WorkflowSelection> for LocalModelOracle<'_> {
    type Error = anyhow::Error;

    async fn select(&mut self, context: ModelOracleContext) -> Result<WorkflowSelection> {
        self.runner
            .select_with_model(&context.state, &context.instruction, &context.extended)
            .await
    }
}

struct HumanSelectionOracle {
    selection: Option<WorkflowSelection>,
}

impl Oracle<(), WorkflowSelection> for HumanSelectionOracle {
    type Error = anyhow::Error;

    async fn select(&mut self, (): ()) -> Result<WorkflowSelection> {
        self.selection
            .take()
            .context("human oracle slot has no supplied selection")
    }
}

async fn select_from_oracle<Context, Adapter>(
    adapter: &mut Adapter,
    context: Context,
) -> Result<WorkflowSelection>
where
    Adapter: Oracle<Context, WorkflowSelection, Error = anyhow::Error>,
{
    adapter.select(context).await
}

impl Runner {
    fn new(state_root: PathBuf, lake_root: PathBuf) -> Self {
        Self {
            state_root,
            manifest: lake_root.join("manifests/events.ndjson"),
            http: reqwest::Client::new(),
        }
    }

    async fn cycle(&self) -> Result<()> {
        for workflow in self.workflows()?.into_iter().filter(|item| item.enabled) {
            match self.run_subscriptions(&workflow).await {
                Ok(()) => {
                    let _ = fs::remove_file(self.workflow_failure_marker(&workflow.id));
                }
                Err(error) => {
                    let detail = format!("{} ({}): {error:#}", workflow.id, workflow.name);
                    let marker = self.workflow_failure_marker(&workflow.id);
                    if fs::read_to_string(&marker).ok().as_deref() != Some(detail.as_str()) {
                        fs::create_dir_all(marker.parent().expect("failure marker has a parent"))?;
                        fs::write(&marker, detail.as_bytes())?;
                        self.failure("workflow_subscription_failed", detail)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn workflow_failure_marker(&self, workflow_id: &str) -> PathBuf {
        self.state_root
            .join("workflow-state")
            .join(workflow_id)
            .join("subscription-failure")
    }

    fn workflows(&self) -> Result<Vec<WorkflowDefinition>> {
        let directory = self.state_root.join("services");
        fs::create_dir_all(&directory)?;
        fs::read_dir(directory)?
            .filter_map(|entry| match entry {
                Ok(entry)
                    if entry.path().extension().and_then(|value| value.to_str())
                        == Some("json") =>
                {
                    Some(
                        fs::read(entry.path())
                            .context("read workflow definition")
                            .and_then(|bytes| {
                                serde_json::from_slice(&bytes).context("parse workflow definition")
                            }),
                    )
                }
                Ok(_) => None,
                Err(error) => Some(Err(error.into())),
            })
            .collect()
    }

    fn workflow(&self, id: &str) -> Result<WorkflowDefinition> {
        self.workflows()?
            .into_iter()
            .find(|workflow| workflow.id == id)
            .ok_or_else(|| anyhow!("workflow {id} is not configured"))
    }

    async fn run_subscriptions(&self, workflow: &WorkflowDefinition) -> Result<()> {
        if !self.manifest.exists() {
            return Ok(());
        }
        let state = self.state_root.join("workflow-state").join(&workflow.id);
        let checkpoint_path = state.join("checkpoint.json");
        if !checkpoint_path.exists() {
            let sequence_path = self.manifest.with_file_name("sequence");
            let cursor = fs::read_to_string(sequence_path)
                .ok()
                .and_then(|value| value.trim().parse::<usize>().ok())
                .and_then(|sequence| sequence.checked_add(1));
            let cursor = match cursor {
                Some(cursor) => cursor,
                None => BufReader::new(File::open(&self.manifest)?).lines().count(),
            };
            atomic_json(&checkpoint_path, &Checkpoint { cursor })?;
            return Ok(());
        }
        let mut checkpoint: Checkpoint = fs::read(&checkpoint_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        for line in BufReader::new(File::open(&self.manifest)?)
            .lines()
            .skip(checkpoint.cursor)
        {
            let envelope = Envelope::from_json(&line?)?;
            let subscribed = workflow.subscriptions.is_empty()
                || workflow
                    .subscriptions
                    .iter()
                    .any(|wanted| wanted == message_type(&envelope.message));
            if subscribed && let Some(input) = workflow_input(&envelope.message) {
                let request = WorkflowRunRequest {
                    run_id: Uuid::new_v4(),
                    workflow_id: workflow.id.clone(),
                    input,
                    resume_state_id: None,
                    forced_selection: None,
                    requested_by: WorkflowRunRequester::Automatic,
                    requested_at_ms: Utc::now().timestamp_millis(),
                };
                self.execute(workflow, &request, BTreeMap::new(), Vec::new())
                    .await?;
            }
            checkpoint.cursor += 1;
            atomic_json(&checkpoint_path, &checkpoint)?;
        }
        Ok(())
    }

    async fn command(&self, envelope: Envelope) -> Result<()> {
        match &envelope.message {
            Message::WorkflowRunRequested(request) => {
                let workflow = self.workflow(&request.workflow_id)?;
                self.execute(&workflow, request, BTreeMap::new(), Vec::new())
                    .await?;
            }
            Message::WorkflowRoutingRequested(request) => {
                let workflows = self
                    .workflows()?
                    .into_iter()
                    .filter(|workflow| workflow.enabled)
                    .collect::<Vec<_>>();
                let selected = self.select_workflows(&workflows, &request.input).await?;
                for workflow in workflows
                    .iter()
                    .filter(|workflow| selected.contains(workflow.id.as_str()))
                {
                    let run = WorkflowRunRequest {
                        run_id: Uuid::new_v4(),
                        workflow_id: workflow.id.clone(),
                        input: request.input.clone(),
                        resume_state_id: None,
                        forced_selection: None,
                        requested_by: WorkflowRunRequester::LocalModel,
                        requested_at_ms: request.requested_at_ms,
                    };
                    self.execute(workflow, &run, BTreeMap::new(), Vec::new())
                        .await?;
                }
            }
            Message::WorkflowTransitionAdjusted(adjustment) => {
                self.adjust(
                    adjustment.run_id,
                    &adjustment.state_id,
                    adjustment.selection.clone(),
                )
                .await?;
            }
            Message::WorkflowEffectUndoRequested(request) => {
                self.undo(request.run_id, &request.transition_id).await?;
            }
            Message::ActionUndone(undo) => self.finish_mac_undo(undo)?,
            _ => bail!("services worker received unsupported command"),
        }
        Ok(())
    }

    async fn execute(
        &self,
        workflow: &WorkflowDefinition,
        request: &WorkflowRunRequest,
        mut extended: BTreeMap<String, String>,
        mut steps: Vec<WorkflowStepRecord>,
    ) -> Result<()> {
        validate_workflow(workflow)?;
        extended
            .entry("transcript".into())
            .or_insert_with(|| request.input.transcript.clone());
        extended
            .entry("recording_id".into())
            .or_insert_with(|| request.input.recording_id.to_string());
        extended
            .entry("transcript_run_id".into())
            .or_insert_with(|| request.input.transcript_run_id.to_string());
        extended
            .entry("recorded_at_ms".into())
            .or_insert_with(|| request.input.recorded_at_ms.to_string());
        let directory = self.run_directory(request.run_id);
        atomic_json(&directory.join("request.json"), request)?;
        let started_at_ms = Utc::now().timestamp_millis();
        let states = workflow
            .states
            .iter()
            .map(|state| (state.id.as_str(), state))
            .collect::<HashMap<_, _>>();
        let mut current = request
            .resume_state_id
            .as_deref()
            .unwrap_or(&workflow.initial_state_id)
            .to_owned();
        let mut forced = request.forced_selection.clone();
        let mut visited = HashSet::new();
        let status = loop {
            if !visited.insert(current.clone()) || steps.len() >= 64 {
                bail!("workflow {} contains a control-state cycle", workflow.id);
            }
            let state = states
                .get(current.as_str())
                .ok_or_else(|| anyhow!("workflow state {current} does not exist"))?;
            let available = state
                .transitions
                .iter()
                .map(|transition| transition.id.clone())
                .collect::<Vec<_>>();
            if let WorkflowSelector::Terminal { outcome } = &state.selector {
                steps.push(WorkflowStepRecord {
                    state_id: state.id.clone(),
                    available_transition_ids: Vec::new(),
                    selection: None,
                    selected_by: "terminal".into(),
                    destination_state_id: None,
                    receipt: None,
                    undoable: false,
                });
                break outcome.clone();
            }
            let human_oracle_slot = human_can_select(&state.selector, request.requested_by);
            if human_oracle_slot && forced.is_none() {
                steps.push(WorkflowStepRecord {
                    state_id: state.id.clone(),
                    available_transition_ids: available,
                    selection: None,
                    selected_by: "waiting_for_human_oracle".into(),
                    destination_state_id: None,
                    receipt: None,
                    undoable: false,
                });
                break "waiting_for_user".into();
            }
            let selected = if human_oracle_slot && let Some(selection) = forced.take() {
                let mut oracle = HumanSelectionOracle {
                    selection: Some(selection),
                };
                select_from_oracle(&mut oracle, ())
                    .await
                    .map(|selection| (selection, "user".to_owned()))
            } else {
                self.select(state, &extended).await
            };
            let (selection, selected_by) = match selected {
                Ok(value) => value,
                Err(error) => {
                    steps.push(WorkflowStepRecord {
                        state_id: state.id.clone(),
                        available_transition_ids: available,
                        selection: None,
                        selected_by: "invalid_oracle_output".into(),
                        destination_state_id: None,
                        receipt: Some(format!("{error:#}")),
                        undoable: false,
                    });
                    break "oracle_output_invalid".into();
                }
            };
            let transition = match validate_selection(state, &selection) {
                Ok(transition) => transition,
                Err(error) => {
                    steps.push(WorkflowStepRecord {
                        state_id: state.id.clone(),
                        available_transition_ids: available,
                        selection: Some(selection),
                        selected_by,
                        destination_state_id: None,
                        receipt: Some(format!("{error:#}")),
                        undoable: false,
                    });
                    break "selection_invalid".into();
                }
            };
            extended.extend(selection.outputs.clone());
            let (receipt, undoable) = match &transition.action {
                WorkflowAction::None => (None, false),
                WorkflowAction::Effect {
                    adapter,
                    undo_adapter,
                } => {
                    let payload = EffectPayload {
                        workflow_id: &workflow.id,
                        run_id: request.run_id,
                        recording_id: request.input.recording_id,
                        transition_id: &transition.id,
                        extended_state: &extended,
                    };
                    match self.deliver_effect(adapter, &payload, request).await {
                        Ok(receipt) => (Some(receipt), undo_adapter.is_some()),
                        Err(error) => {
                            steps.push(WorkflowStepRecord {
                                state_id: state.id.clone(),
                                available_transition_ids: available,
                                selection: Some(selection),
                                selected_by,
                                destination_state_id: None,
                                receipt: Some(format!("effect failed: {error:#}")),
                                undoable: false,
                            });
                            break "effect_failed".into();
                        }
                    }
                }
            };
            steps.push(WorkflowStepRecord {
                state_id: state.id.clone(),
                available_transition_ids: available,
                selection: Some(selection),
                selected_by,
                destination_state_id: Some(transition.destination_state_id.clone()),
                receipt,
                undoable,
            });
            current.clone_from(&transition.destination_state_id);
        };
        let record = WorkflowRunRecord {
            run_id: request.run_id,
            workflow_id: workflow.id.clone(),
            recording_id: request.input.recording_id,
            requested_by: request.requested_by,
            started_at_ms,
            completed_at_ms: Utc::now().timestamp_millis(),
            status,
            steps,
        };
        atomic_json(&directory.join("record.json"), &record)?;
        emit(&Envelope::root(Message::WorkflowRunRecorded(record)))?;
        Ok(())
    }

    async fn select(
        &self,
        state: &WorkflowState,
        extended: &BTreeMap<String, String>,
    ) -> Result<(WorkflowSelection, String)> {
        let selection = match &state.selector {
            WorkflowSelector::Deterministic { transition_id } => WorkflowSelection {
                transition_id: transition_id.clone(),
                outputs: BTreeMap::new(),
            },
            WorkflowSelector::TextLength {
                field,
                maximum_chars,
                within_transition_id,
                overflow_transition_id,
            } => WorkflowSelection {
                transition_id: text_length_transition(
                    extended.get(field).map_or("", String::as_str),
                    *maximum_chars,
                    within_transition_id,
                    overflow_transition_id,
                ),
                outputs: BTreeMap::new(),
            },
            WorkflowSelector::ActionEvidence {
                transcript_field,
                evidence_field,
                prior_evidence_fields,
                due_at_ms_field,
                time_phrase_field,
                accepted_evidence_field,
                valid_transition_id,
                invalid_transition_id,
            } => {
                let valid = action_evidence_is_valid(
                    extended,
                    transcript_field,
                    evidence_field,
                    prior_evidence_fields,
                    due_at_ms_field.as_deref(),
                    time_phrase_field.as_deref(),
                );
                WorkflowSelection {
                    transition_id: if valid {
                        valid_transition_id.clone()
                    } else {
                        invalid_transition_id.clone()
                    },
                    outputs: if valid {
                        accepted_evidence_field
                            .as_ref()
                            .and_then(|accepted| {
                                extended
                                    .get(evidence_field)
                                    .map(|evidence| (accepted.clone(), evidence.clone()))
                            })
                            .into_iter()
                            .collect()
                    } else {
                        BTreeMap::new()
                    },
                }
            }
            WorkflowSelector::PhraseMatch {
                field,
                cases,
                fallback_transition_id,
            } => {
                let value = extended
                    .get(field)
                    .map_or("", String::as_str)
                    .to_lowercase();
                let transition_id = phrase_match_transition(
                    &value,
                    cases
                        .iter()
                        .map(|case| (case.transition_id.as_str(), case.phrases.as_slice())),
                    fallback_transition_id,
                );
                WorkflowSelection {
                    transition_id,
                    outputs: BTreeMap::new(),
                }
            }
            WorkflowSelector::LocalModel { instruction } => {
                let mut oracle = LocalModelOracle { runner: self };
                select_from_oracle(
                    &mut oracle,
                    ModelOracleContext {
                        state: state.clone(),
                        instruction: instruction.clone(),
                        extended: extended.clone(),
                    },
                )
                .await?
            }
            WorkflowSelector::User | WorkflowSelector::Terminal { .. } => {
                bail!("state {} requires external selection", state.id)
            }
        };
        let selected_by = match state.selector {
            WorkflowSelector::LocalModel { .. } => "local_model",
            _ => "deterministic",
        };
        Ok((selection, selected_by.into()))
    }

    async fn select_with_model(
        &self,
        state: &WorkflowState,
        instruction: &str,
        extended: &BTreeMap<String, String>,
    ) -> Result<WorkflowSelection> {
        let branches = state
            .transitions
            .iter()
            .map(|transition| {
                json!({
                    "type":"object",
                    "additionalProperties":false,
                    "required":["transition_id","outputs"],
                    "properties":{
                        "transition_id":{"const":transition.id},
                        "outputs":output_object_schema(&transition.output_fields)
                    }
                })
            })
            .collect::<Vec<_>>();
        let format = json!({"oneOf":branches});
        let choices = state
            .transitions
            .iter()
            .map(|transition| format!("{}: {}", transition.id, transition.description))
            .collect::<Vec<_>>()
            .join("\n");
        let base_prompt = format!(
            "{instruction}\nSelect exactly one declared transition and fill only its declared output fields.\nTransitions:\n{choices}\nExtended state:\n{}",
            serde_json::to_string_pretty(extended)?
        );
        let mut correction = String::new();
        let mut last_error = None;
        for _ in 0..3 {
            let value = self
                .ask_local_model(&format!("{base_prompt}{correction}"), format.clone())
                .await?;
            let selection: WorkflowSelection = serde_json::from_value(value)?;
            match validate_selection(state, &selection) {
                Ok(_) => return Ok(selection),
                Err(error) => {
                    correction = format!(
                        "\nYour previous response violated the hard schema: {error:#}. Return a corrected selection."
                    );
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("local model returned no valid selection")))
    }

    async fn select_workflows(
        &self,
        workflows: &[WorkflowDefinition],
        input: &WorkflowInput,
    ) -> Result<HashSet<String>> {
        if workflows.is_empty() {
            return Ok(HashSet::new());
        }
        let ids = workflows
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let descriptions = workflows
            .iter()
            .map(|item| format!("{}: {} — {}", item.id, item.name, item.description))
            .collect::<Vec<_>>()
            .join("\n");
        let format = json!({
            "type":"object","additionalProperties":false,"required":["workflow_ids"],
            "properties":{"workflow_ids":{"type":"array","uniqueItems":true,"items":{"enum":ids}}}
        });
        let prompt = format!(
            "Select every relevant workflow. Select none if none applies.\n{descriptions}\nTranscript:\n{}",
            input.transcript
        );
        Ok(self
            .ask_local_model(&prompt, format)
            .await?
            .get("workflow_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect())
    }

    async fn ask_local_model(&self, prompt: &str, format: Value) -> Result<Value> {
        let (model, url) = self.model_configuration();
        self.ensure_24b_model(&model, &url).await?;
        let response = self
            .http
            .post(format!("{}/api/chat", url.trim_end_matches('/')))
            .json(&json!({
                "model":model,"stream":false,"format":format,
                "messages":[{"role":"user","content":prompt}],
                "options":{"temperature":0}
            }))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let content = response
            .pointer("/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("local model returned no content"))?;
        Ok(serde_json::from_str(content)?)
    }

    fn model_configuration(&self) -> (String, String) {
        let rules = fs::read(self.state_root.join("rules.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or_else(|| json!({}));
        let model = rules
            .get("semantic_model")
            .and_then(Value::as_str)
            .unwrap_or("mistral-small3.2:latest")
            .to_owned();
        let url = rules
            .get("ollama_url")
            .and_then(Value::as_str)
            .unwrap_or("http://127.0.0.1:11434")
            .to_owned();
        (model, url)
    }

    async fn ensure_24b_model(&self, model: &str, url: &str) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/api/show", url.trim_end_matches('/')))
            .timeout(Duration::from_secs(10))
            .json(&json!({"model":model}))
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<Value>().await?;
        if !status.is_success() {
            bail!("Ollama model inspection failed ({status}): {body}");
        }
        let parameters = body
            .pointer("/details/parameter_size")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !is_required_local_model_size(parameters) {
            bail!("semantic model {model} is {parameters}, expected local 24B");
        }
        Ok(())
    }

    async fn adjust(
        &self,
        run_id: Uuid,
        state_id: &str,
        selection: WorkflowSelection,
    ) -> Result<()> {
        let mut request = self.load_request(run_id)?;
        let old = self.load_record(run_id)?;
        let workflow = self.workflow(&request.workflow_id)?;
        let split = old
            .steps
            .iter()
            .position(|step| step.state_id == state_id)
            .ok_or_else(|| anyhow!("state {state_id} was not visited by this run"))?;
        for step in old.steps[split..].iter().rev() {
            if step.receipt.is_some() {
                let transition_id = step
                    .selection
                    .as_ref()
                    .map(|value| value.transition_id.as_str())
                    .ok_or_else(|| anyhow!("effect trace has no transition"))?;
                if !step.undoable {
                    bail!("cannot replay past non-reversible transition {transition_id}");
                }
                self.compensate(&workflow, &request, transition_id, &BTreeMap::new(), true)
                    .await?;
            }
        }
        let prefix = old.steps[..split].to_vec();
        let mut extended = base_extended(&request.input);
        for step in &prefix {
            if let Some(selection) = &step.selection {
                extended.extend(selection.outputs.clone());
            }
        }
        request.resume_state_id = Some(state_id.into());
        request.forced_selection = Some(selection);
        request.requested_by = WorkflowRunRequester::User;
        self.execute(&workflow, &request, extended, prefix).await
    }

    async fn undo(&self, run_id: Uuid, transition_id: &str) -> Result<()> {
        let request = self.load_request(run_id)?;
        let workflow = self.workflow(&request.workflow_id)?;
        let record = self.load_record(run_id)?;
        let mut extended = base_extended(&request.input);
        for step in &record.steps {
            if let Some(selection) = &step.selection {
                extended.extend(selection.outputs.clone());
            }
        }
        let receipt = self
            .compensate(&workflow, &request, transition_id, &extended, false)
            .await?;
        let asynchronous = workflow
            .states
            .iter()
            .flat_map(|state| &state.transitions)
            .find(|transition| transition.id == transition_id)
            .is_some_and(|transition| {
                matches!(
                    transition.action,
                    WorkflowAction::Effect {
                        undo_adapter: Some(WorkflowAdapter::SystemActionUndo),
                        ..
                    }
                )
            });
        if asynchronous {
            return Ok(());
        }
        let undo = WorkflowEffectUndo {
            run_id,
            transition_id: transition_id.into(),
            undone_at_ms: Utc::now().timestamp_millis(),
            receipt,
        };
        self.persist_undo_marker(&undo)?;
        emit(&Envelope::root(Message::WorkflowEffectUndone(undo)))
    }

    async fn compensate(
        &self,
        workflow: &WorkflowDefinition,
        request: &WorkflowRunRequest,
        transition_id: &str,
        extended: &BTreeMap<String, String>,
        suppress_completion_event: bool,
    ) -> Result<String> {
        let transition = workflow
            .states
            .iter()
            .flat_map(|state| &state.transitions)
            .find(|transition| transition.id == transition_id)
            .ok_or_else(|| anyhow!("transition {transition_id} is not configured"))?;
        let WorkflowAction::Effect {
            undo_adapter: Some(adapter),
            ..
        } = &transition.action
        else {
            bail!("transition {transition_id} has no compensating action");
        };
        let payload = EffectPayload {
            workflow_id: &workflow.id,
            run_id: request.run_id,
            recording_id: request.input.recording_id,
            transition_id,
            extended_state: extended,
        };
        match adapter {
            WorkflowAdapter::SystemActionUndo => {
                let action_id = action_id(request.run_id, transition_id);
                let suppression = self
                    .state_root
                    .join("workflow-state/suppressed-undo")
                    .join(action_id.to_string());
                if suppress_completion_event {
                    fs::create_dir_all(suppression.parent().expect("suppression has a parent"))?;
                    fs::write(&suppression, b"adjustment owns this compensation")?;
                }
                emit(&Envelope::root(Message::ActionUndoRequested { action_id }))?;
                let undone = self
                    .state_root
                    .join("calendar/undone")
                    .join(format!("{action_id}.json"));
                for _ in 0..240 {
                    if let Ok(bytes) = fs::read(&undone) {
                        let receipt: personal_audio_lake_contracts::ActionUndo =
                            serde_json::from_slice(&bytes)?;
                        if suppress_completion_event {
                            fs::write(&suppression, receipt.undone_at_ms.to_string())?;
                        }
                        return Ok(format!("removed item from {}", receipt.target));
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                fs::remove_file(suppression).ok();
                bail!("timed out waiting for the Mac action to be removed")
            }
            WorkflowAdapter::DocumentUndo => {
                let record = self.load_record(request.run_id)?;
                let receipt = record
                    .steps
                    .iter()
                    .rev()
                    .find(|step| {
                        step.selection
                            .as_ref()
                            .is_some_and(|selection| selection.transition_id == transition_id)
                            && step.receipt.is_some()
                    })
                    .and_then(|step| step.receipt.as_deref())
                    .ok_or_else(|| anyhow!("transition {transition_id} has no delivery receipt"))?;
                let receipt: DocumentReceipt = serde_json::from_str(receipt)
                    .context("decode native Mac integration receipt")?;
                self.undo_mac_app(&receipt)
            }
            _ => self.deliver(adapter, &payload).await,
        }
    }

    async fn deliver_effect(
        &self,
        adapter: &WorkflowAdapter,
        payload: &EffectPayload<'_>,
        request: &WorkflowRunRequest,
    ) -> Result<String> {
        let WorkflowAdapter::SystemAction {
            kind,
            title_field,
            due_at_ms_field,
            evidence_field,
            time_phrase_field,
        } = adapter
        else {
            if let WorkflowAdapter::Document {
                kind,
                title_field,
                content_field,
                target_field,
            } = adapter
            {
                return self.deliver_mac_app(
                    *kind,
                    title_field,
                    content_field.as_deref(),
                    target_field.as_deref(),
                    payload,
                );
            }
            return self.deliver(adapter, payload).await;
        };
        let title = payload
            .extended_state
            .get(title_field)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("Mac action title field {title_field} is missing"))?
            .clone();
        let due_at_ms = due_at_ms_field
            .as_ref()
            .and_then(|field| payload.extended_state.get(field))
            .map(|value| value.parse::<i64>())
            .transpose()
            .context("Mac action due time must be Unix milliseconds")?;
        let evidence_quote = evidence_field
            .as_ref()
            .map(|field| {
                payload
                    .extended_state
                    .get(field)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| anyhow!("Mac action evidence field {field} is missing"))
            })
            .transpose()?;
        let time_phrase = time_phrase_field
            .as_ref()
            .and_then(|field| payload.extended_state.get(field))
            .filter(|value| !value.trim().is_empty());
        validate_mac_action_grounding(
            &request.input.transcript,
            request.input.recorded_at_ms,
            due_at_ms,
            evidence_quote.map(String::as_str),
            time_phrase_field.is_some(),
            time_phrase.map(String::as_str),
        )?;
        let action_id = action_id(request.run_id, payload.transition_id);
        let proposal = Envelope::root(Message::ActionProposed(ActionProposal {
            action_id,
            recording_id: request.input.recording_id,
            transcript_run_id: request.input.transcript_run_id,
            kind: *kind,
            title,
            due_at_ms,
            original_time_phrase: time_phrase.cloned(),
            timezone: "Europe/London".into(),
            confidence: 1.0,
            evidence: evidence_quote
                .map(|quote| {
                    vec![SegmentEvidence {
                        ordinal: 0,
                        start_ms: 0,
                        end_ms: 0,
                        quote: quote.clone(),
                    }]
                })
                .unwrap_or_default(),
            extractor_version: "user-osm-v2-evidence-bound".into(),
        }));
        let mut approval = Envelope::caused_by(&proposal, Message::ActionApproved { action_id });
        approval.emitted_at_ms = proposal.emitted_at_ms + 1;
        emit(&proposal)?;
        emit(&approval)?;
        atomic_json(
            &self
                .state_root
                .join("workflow-state/action-map")
                .join(format!("{action_id}.json")),
            &SystemActionLink {
                run_id: request.run_id,
                transition_id: payload.transition_id.into(),
            },
        )?;
        Ok(format!("queued Mac action {action_id}"))
    }

    fn deliver_mac_app(
        &self,
        kind: DocumentKind,
        title_field: &str,
        content_field: Option<&str>,
        target_field: Option<&str>,
        payload: &EffectPayload<'_>,
    ) -> Result<String> {
        let title = workflow_field(payload.extended_state, title_field, true)?;
        let content = content_field
            .map(|field| workflow_field(payload.extended_state, field, false))
            .transpose()?
            .unwrap_or_default();
        let target = target_field
            .map(|field| workflow_field(payload.extended_state, field, false))
            .transpose()?
            .unwrap_or_default();
        let receipt = match kind {
            DocumentKind::Notes => {
                let body = if content.trim().is_empty() {
                    title.to_owned()
                } else {
                    content.to_owned()
                };
                let html = format!("<div>{}</div>", html_escape(&body).replace('\n', "<br>"));
                let id = run_osascript(NOTES_CREATE_SCRIPT, &[title, &html, target])?;
                DocumentReceipt {
                    kind,
                    external_id: id,
                    destination: "Apple Notes".into(),
                    summary: format!("Created note ‘{title}’"),
                }
            }
            DocumentKind::MailDraft => {
                if !target.is_empty() && !looks_like_email(target) {
                    bail!("Mail draft recipient must be an email address");
                }
                let id = run_osascript(MAIL_DRAFT_CREATE_SCRIPT, &[target, title, content])?;
                DocumentReceipt {
                    kind,
                    external_id: id,
                    destination: "Apple Mail Drafts".into(),
                    summary: format!("Created email draft ‘{title}’"),
                }
            }
            DocumentKind::Contacts => {
                let (first, last) = split_contact_name(title);
                let method = if target.contains('@') {
                    "email"
                } else if target.is_empty() {
                    "none"
                } else {
                    "phone"
                };
                let id = run_osascript(
                    CONTACT_CREATE_SCRIPT,
                    &[first, last, content, method, target],
                )?;
                DocumentReceipt {
                    kind,
                    external_id: id,
                    destination: "Apple Contacts".into(),
                    summary: format!("Created contact ‘{title}’"),
                }
            }
            DocumentKind::TextFile => self.create_text_file(title, content, target)?,
        };
        Ok(serde_json::to_string(&receipt)?)
    }

    fn create_text_file(
        &self,
        title: &str,
        content: &str,
        target: &str,
    ) -> Result<DocumentReceipt> {
        let home = std::env::var_os("HOME").context("HOME is not configured")?;
        let root = PathBuf::from(home).join("Documents/Captain's Log");
        create_text_file_in(&root, title, content, target)
    }

    fn undo_mac_app(&self, receipt: &DocumentReceipt) -> Result<String> {
        match receipt.kind {
            DocumentKind::Notes => {
                run_osascript(NOTES_DELETE_SCRIPT, &[&receipt.external_id])?;
            }
            DocumentKind::MailDraft => {
                let (stable_message_id, compose_id) = receipt.external_id.split_once('|').map_or(
                    (receipt.external_id.as_str(), None),
                    |(message_id, compose_id)| (message_id, Some(compose_id)),
                );
                if let Some(compose_id) = compose_id {
                    run_osascript(MAIL_DRAFT_CLOSE_SCRIPT, &[compose_id])?;
                    std::thread::sleep(Duration::from_millis(250));
                }
                run_osascript(MAIL_DRAFT_DELETE_SCRIPT, &[stable_message_id])?;
            }
            DocumentKind::Contacts => {
                run_osascript(CONTACT_DELETE_SCRIPT, &[&receipt.external_id])?;
            }
            DocumentKind::TextFile => {
                let home = std::env::var_os("HOME").context("HOME is not configured")?;
                let documents = PathBuf::from(home).join("Documents");
                let current_root = documents.join("Captain's Log");
                let legacy_root = documents.join("Personal Audio Lake");
                let path = Path::new(&receipt.external_id);
                let root = if path.parent() == Some(legacy_root.as_path()) {
                    &legacy_root
                } else {
                    &current_root
                };
                remove_text_file_in(root, path)?;
            }
        }
        Ok(format!("Undid: {}", receipt.summary))
    }

    async fn deliver(&self, adapter: &WorkflowAdapter, payload: &impl Serialize) -> Result<String> {
        match adapter {
            WorkflowAdapter::Webhook { target } => {
                validate_webhook(target)?;
                let response = self.http.post(target).json(payload).send().await?;
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    bail!("webhook returned {status}: {}", truncate(&body, 500));
                }
                Ok(format!("HTTP {status}: {}", truncate(&body, 500)))
            }
            WorkflowAdapter::Shortcut { target } => {
                if target.trim().is_empty() {
                    bail!("shortcut target is empty");
                }
                let inputs = self.state_root.join("workflow-state/shortcut-inputs");
                fs::create_dir_all(&inputs)?;
                let input = inputs.join(format!("{}.json", Uuid::new_v4()));
                fs::write(&input, serde_json::to_vec_pretty(payload)?)?;
                let output = Command::new("/usr/bin/shortcuts")
                    .args(["run", target, "-i"])
                    .arg(&input)
                    .output()
                    .with_context(|| format!("run macOS Shortcut {target}"))?;
                let _ = fs::remove_file(input);
                if !output.status.success() {
                    bail!(
                        "Shortcut failed: {}",
                        truncate(&String::from_utf8_lossy(&output.stderr), 500)
                    );
                }
                Ok(truncate(&String::from_utf8_lossy(&output.stdout), 500))
            }
            WorkflowAdapter::SystemAction { .. }
            | WorkflowAdapter::SystemActionUndo
            | WorkflowAdapter::Document { .. }
            | WorkflowAdapter::DocumentUndo => {
                bail!("Mac adapters require workflow effect context")
            }
        }
    }

    fn load_request(&self, run_id: Uuid) -> Result<WorkflowRunRequest> {
        serde_json::from_slice(&fs::read(self.run_directory(run_id).join("request.json"))?)
            .context("load workflow run request")
    }

    fn finish_mac_undo(&self, undo: &personal_audio_lake_contracts::ActionUndo) -> Result<()> {
        let suppression = self
            .state_root
            .join("workflow-state/suppressed-undo")
            .join(undo.action_id.to_string());
        if let Ok(value) = fs::read_to_string(&suppression) {
            fs::remove_file(suppression)?;
            if value.trim() == undo.undone_at_ms.to_string() {
                return Ok(());
            }
        }
        let link_path = self
            .state_root
            .join("workflow-state/action-map")
            .join(format!("{}.json", undo.action_id));
        let bytes = match fs::read(&link_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let link: SystemActionLink = serde_json::from_slice(&bytes)?;
        let workflow_undo = WorkflowEffectUndo {
            run_id: link.run_id,
            transition_id: link.transition_id,
            undone_at_ms: undo.undone_at_ms,
            receipt: format!("removed item from {}", undo.target),
        };
        self.persist_undo_marker(&workflow_undo)?;
        emit(&Envelope::root(Message::WorkflowEffectUndone(
            workflow_undo,
        )))
    }

    fn persist_undo_marker(&self, undo: &WorkflowEffectUndo) -> Result<()> {
        let identity = format!("{}:{}", undo.run_id, undo.transition_id);
        let marker = Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes());
        atomic_json(
            &self
                .run_directory(undo.run_id)
                .join("undone")
                .join(format!("{marker}.json")),
            &json!({"transition_id":undo.transition_id,"receipt":undo.receipt}),
        )
    }

    fn load_record(&self, run_id: Uuid) -> Result<WorkflowRunRecord> {
        serde_json::from_slice(&fs::read(self.run_directory(run_id).join("record.json"))?)
            .context("load workflow run record")
    }

    fn run_directory(&self, run_id: Uuid) -> PathBuf {
        self.state_root
            .join("workflow-runs")
            .join(run_id.to_string())
    }

    fn failure(&self, code: &str, detail: String) -> Result<()> {
        emit(&Envelope::root(Message::Failure(Failure {
            worker: WorkerRole::Services,
            code: code.into(),
            detail,
            retryable: true,
            failed_message_id: None,
        })))
    }
}

fn workflow_field<'a>(
    state: &'a BTreeMap<String, String>,
    field: &str,
    required: bool,
) -> Result<&'a str> {
    match state.get(field).map(String::as_str) {
        Some(value) if required && value.trim().is_empty() => {
            bail!("native Mac integration field {field} is missing")
        }
        Some(value) => Ok(value),
        None if required => bail!("native Mac integration field {field} is missing"),
        None => Ok(""),
    }
}

fn run_osascript(script: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .arg("--")
        .args(arguments)
        .output()
        .context("run native macOS integration")?;
    if !output.status.success() {
        bail!(
            "native macOS integration failed: {}",
            truncate(&String::from_utf8_lossy(&output.stderr), 500)
        );
    }
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    if value.is_empty() {
        bail!("native macOS integration returned no object identifier");
    }
    Ok(value)
}

fn split_contact_name(name: &str) -> (&str, &str) {
    name.trim()
        .split_once(' ')
        .map_or((name.trim(), ""), |(first, last)| (first, last.trim()))
}

fn looks_like_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !value.chars().any(char::is_whitespace)
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn safe_file_stem(value: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in value.trim().chars() {
        if character.is_alphanumeric() || matches!(character, '-' | '_') {
            if separator && !result.is_empty() {
                result.push('-');
            }
            separator = false;
            result.push(character);
        } else {
            separator = true;
        }
        if result.chars().count() >= 80 {
            break;
        }
    }
    let result = result.trim_matches('-');
    if result.is_empty() {
        "Captains-Log-note".into()
    } else {
        result.into()
    }
}

fn create_text_file_in(
    root: &Path,
    title: &str,
    content: &str,
    target: &str,
) -> Result<DocumentReceipt> {
    fs::create_dir_all(root).context("create Captain's Log text export folder")?;
    let requested = if target.trim().is_empty() {
        title
    } else {
        target
    };
    let stem = safe_file_stem(requested);
    let path = (0..1_000)
        .map(|attempt| {
            let suffix = if attempt == 0 {
                String::new()
            } else {
                format!("-{attempt}")
            };
            root.join(format!("{stem}{suffix}.txt"))
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| anyhow!("could not choose an unused export filename"))?;
    let text = if content.trim().is_empty() {
        title
    } else {
        content
    };
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(&path)
        .context("create Captain's Log text export")?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(DocumentReceipt {
        kind: DocumentKind::TextFile,
        external_id: path.display().to_string(),
        destination: "Documents/Captain's Log".into(),
        summary: format!(
            "Created {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
    })
}

fn remove_text_file_in(root: &Path, path: &Path) -> Result<()> {
    if path.parent() != Some(root)
        || path.extension().and_then(|value| value.to_str()) != Some("txt")
    {
        bail!("refusing to remove a file outside the Captain's Log export folder");
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

const NOTES_CREATE_SCRIPT: &str = r#"
on run argv
    set noteTitle to item 1 of argv
    set noteBody to item 2 of argv
    set folderName to item 3 of argv
    tell application "Notes"
        set targetAccount to default account
        set targetFolder to default folder of targetAccount
        if folderName is not "" then
            set folderMatches to every folder of targetAccount whose name is folderName
            if (count of folderMatches) is 0 then error "Apple Notes folder not found: " & folderName
            set targetFolder to item 1 of folderMatches
        end if
        set createdNote to make new note at targetFolder with properties {name:noteTitle, body:noteBody}
        return id of createdNote
    end tell
end run
"#;

const NOTES_DELETE_SCRIPT: &str = r#"
on run argv
    set noteID to item 1 of argv
    tell application "Notes"
        set matches to every note whose id is noteID
        if (count of matches) is greater than 0 then delete item 1 of matches
    end tell
    return "removed"
end run
"#;

const MAIL_DRAFT_CREATE_SCRIPT: &str = r#"
on run argv
    set recipientAddress to item 1 of argv
    set draftSubject to item 2 of argv
    set draftBody to item 3 of argv
    tell application "Mail"
        set existingIDs to message id of every message of drafts mailbox
        set draftMessage to make new outgoing message with properties {subject:draftSubject, content:draftBody, visible:false}
        if recipientAddress is not "" then
            make new to recipient at end of to recipients of draftMessage with properties {address:recipientAddress}
        end if
        save draftMessage
        set stableMessageID to missing value
        repeat 20 times
            set newIDs to {}
            set candidates to every message of drafts mailbox whose subject is draftSubject
            repeat with candidate in candidates
                set candidateID to message id of candidate
                if existingIDs does not contain candidateID and ((content of candidate) as text) contains draftBody then
                    set end of newIDs to candidateID
                end if
            end repeat
            if (count of newIDs) is 1 then
                set stableMessageID to item 1 of newIDs
                exit repeat
            else if (count of newIDs) is greater than 1 then
                close draftMessage saving no
                error "Apple Mail published multiple indistinguishable drafts"
            end if
            delay 0.1
        end repeat
        if stableMessageID is missing value then
            close draftMessage saving no
            error "Apple Mail did not publish the saved draft"
        end if
        set composeID to id of draftMessage
        return stableMessageID & "|" & (composeID as text)
    end tell
end run
"#;

const MAIL_DRAFT_CLOSE_SCRIPT: &str = r#"
on run argv
    set composeID to (item 1 of argv) as integer
    tell application "Mail"
        set composeMatches to every outgoing message whose id is composeID
        repeat with composeMessage in composeMatches
            close composeMessage saving no
        end repeat
    end tell
    return "closed"
end run
"#;

const MAIL_DRAFT_DELETE_SCRIPT: &str = r#"
on run argv
    set stableMessageID to item 1 of argv
    tell application "Mail"
        set matches to every message of drafts mailbox whose message id is stableMessageID
        if (count of matches) is greater than 0 then delete matches
    end tell
    return "removed"
end run
"#;

const CONTACT_CREATE_SCRIPT: &str = r#"
on run argv
    set firstName to item 1 of argv
    set lastName to item 2 of argv
    set noteText to item 3 of argv
    set methodName to item 4 of argv
    set methodValue to item 5 of argv
    tell application "Contacts"
        set createdPerson to make new person with properties {first name:firstName, last name:lastName, note:noteText}
        if methodName is "email" then
            make new email at end of emails of createdPerson with properties {label:"work", value:methodValue}
        else if methodName is "phone" then
            make new phone at end of phones of createdPerson with properties {label:"mobile", value:methodValue}
        end if
        save
        return id of createdPerson
    end tell
end run
"#;

const CONTACT_DELETE_SCRIPT: &str = r#"
on run argv
    set personID to item 1 of argv
    tell application "Contacts"
        set matches to every person whose id is personID
        if (count of matches) is greater than 0 then
            delete item 1 of matches
            save
        end if
    end tell
    return "removed"
end run
"#;

fn validate_workflow(workflow: &WorkflowDefinition) -> Result<()> {
    if workflow.id.trim().is_empty() || workflow.name.trim().is_empty() {
        bail!("workflow id and name are required");
    }
    if workflow.initial_state_id != "message_ingested" {
        bail!("workflow initial state must be message_ingested");
    }
    let state_ids = workflow
        .states
        .iter()
        .map(|state| state.id.as_str())
        .collect::<HashSet<_>>();
    if state_ids.len() != workflow.states.len() || !state_ids.contains("message_ingested") {
        bail!("workflow state ids must be unique and include message_ingested");
    }
    let mut transitions = HashSet::new();
    for state in &workflow.states {
        for transition in &state.transitions {
            if !transitions.insert(transition.id.as_str()) {
                bail!("transition ids must be unique within a workflow");
            }
            if !state_ids.contains(transition.destination_state_id.as_str()) {
                bail!("transition {} has an unknown destination", transition.id);
            }
        }
        if matches!(state.selector, WorkflowSelector::Terminal { .. })
            != state.transitions.is_empty()
        {
            bail!("only terminal states may have no transitions");
        }
    }
    Ok(())
}

fn validate_selection<'a>(
    state: &'a WorkflowState,
    selection: &WorkflowSelection,
) -> Result<&'a personal_audio_lake_contracts::WorkflowTransition> {
    let transition = state
        .transitions
        .iter()
        .find(|transition| transition.id == selection.transition_id)
        .ok_or_else(|| {
            anyhow!(
                "transition {} is unavailable in state {}",
                selection.transition_id,
                state.id
            )
        })?;
    for key in selection.outputs.keys() {
        if !transition
            .output_fields
            .iter()
            .any(|field| field.name == *key)
        {
            bail!("transition {} cannot write field {key}", transition.id);
        }
    }
    for field in &transition.output_fields {
        let value = selection.outputs.get(&field.name);
        if field.required && value.is_none_or(|value| value.trim().is_empty()) {
            bail!(
                "transition {} requires output field {}",
                transition.id,
                field.name
            );
        }
        if let Some(value) = value {
            validate_output(field, value)?;
        }
    }
    Ok(transition)
}

fn tolerant_phrase_match(value: &str, phrase: &str) -> bool {
    let value_words = value
        .split_whitespace()
        .map(normalize_word)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let phrase_words = phrase
        .split_whitespace()
        .map(normalize_word)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if phrase_words.is_empty() {
        return false;
    }
    let expected = phrase_words.join(" ");
    (0..value_words.len()).any(|start| {
        let minimum = phrase_words.len().saturating_sub(1).max(1);
        let maximum = (phrase_words.len() + 1).min(value_words.len() - start);
        (minimum..=maximum).any(|length| {
            start + length <= value_words.len()
                && text_similarity(&value_words[start..start + length].join(" "), &expected) >= 0.86
        })
    })
}

fn phrase_match_transition<'a>(
    value: &str,
    cases: impl IntoIterator<Item = (&'a str, &'a [String])>,
    fallback: &str,
) -> String {
    cases
        .into_iter()
        .find(|(_, phrases)| {
            phrases
                .iter()
                .any(|phrase| tolerant_phrase_match(value, phrase))
        })
        .map_or(fallback, |(transition, _)| transition)
        .to_owned()
}

fn text_length_transition(value: &str, maximum_chars: u32, within: &str, overflow: &str) -> String {
    if value.chars().count() <= maximum_chars as usize {
        within
    } else {
        overflow
    }
    .to_owned()
}

fn normalized_contains(value: &str, excerpt: &str) -> bool {
    let value = normalize_text(value);
    let excerpt = normalize_text(excerpt);
    !excerpt.is_empty() && value.contains(&excerpt)
}

fn normalize_text(value: &str) -> String {
    value
        .split_whitespace()
        .map(normalize_word)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_mac_action_grounding(
    transcript: &str,
    recorded_at_ms: i64,
    due_at_ms: Option<i64>,
    evidence_quote: Option<&str>,
    configured_time_phrase: bool,
    time_phrase: Option<&str>,
) -> Result<()> {
    if due_at_ms.is_some_and(|due| due <= recorded_at_ms) {
        bail!("Mac action due time must be later than the recording time");
    }
    if let Some(quote) = evidence_quote
        && !normalized_contains(transcript, quote)
    {
        bail!("Mac action evidence must be a verbatim quote from the transcript");
    }
    if due_at_ms.is_some() && configured_time_phrase && time_phrase.is_none() {
        bail!("A due time requires a verbatim time phrase");
    }
    if let Some(phrase) = time_phrase
        && !normalized_contains(evidence_quote.unwrap_or(transcript), phrase)
    {
        bail!("Mac action time phrase must occur in its evidence quote");
    }
    Ok(())
}

fn action_evidence_is_valid(
    extended: &BTreeMap<String, String>,
    transcript_field: &str,
    evidence_field: &str,
    prior_evidence_fields: &[String],
    due_at_ms_field: Option<&str>,
    time_phrase_field: Option<&str>,
) -> bool {
    let Some(transcript) = extended.get(transcript_field) else {
        return false;
    };
    let Some(evidence) = extended.get(evidence_field) else {
        return false;
    };
    let Some(recorded_at_ms) = extended
        .get("recorded_at_ms")
        .and_then(|value| value.parse::<i64>().ok())
    else {
        return false;
    };
    let due_at_ms = match due_at_ms_field.and_then(|field| extended.get(field)) {
        Some(value) => match value.parse::<i64>() {
            Ok(value) => Some(value),
            Err(_) => return false,
        },
        None => None,
    };
    let time_phrase = time_phrase_field.and_then(|field| extended.get(field));
    if validate_mac_action_grounding(
        transcript,
        recorded_at_ms,
        due_at_ms,
        Some(evidence),
        time_phrase_field.is_some(),
        time_phrase.map(String::as_str),
    )
    .is_err()
    {
        return false;
    }
    let evidence = normalize_text(evidence);
    prior_evidence_fields
        .iter()
        .filter_map(|field| extended.get(field))
        .map(|prior| normalize_text(prior))
        .all(|prior| prior.is_empty() || (!evidence.contains(&prior) && !prior.contains(&evidence)))
}

fn normalize_word(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn text_similarity(left: &str, right: &str) -> f32 {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_character) in left.iter().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_character) in right.iter().enumerate() {
            current.push(
                (previous[right_index + 1] + 1)
                    .min(current[right_index] + 1)
                    .min(previous[right_index] + usize::from(left_character != right_character)),
            );
        }
        previous = current;
    }
    1.0 - previous[right.len()] as f32 / left.len().max(right.len()) as f32
}

/// Human and local-model oracles implement the same `WorkflowSelection` contract. A user-run
/// workflow substitutes the human at every model slot; code-owned selectors remain code-owned.
fn human_can_select(selector: &WorkflowSelector, requester: WorkflowRunRequester) -> bool {
    matches!(selector, WorkflowSelector::User)
        || (matches!(selector, WorkflowSelector::LocalModel { .. })
            && requester == WorkflowRunRequester::User)
}

fn validate_output(field: &WorkflowOutputField, value: &str) -> Result<()> {
    match &field.kind {
        WorkflowOutputKind::NaturalLanguage { max_length } => {
            if value.chars().count() > *max_length as usize {
                bail!("field {} exceeds its maximum length", field.name);
            }
        }
        WorkflowOutputKind::Choice { values } => {
            if !values.iter().any(|allowed| allowed == value) {
                bail!("field {} is not one of its declared choices", field.name);
            }
        }
        WorkflowOutputKind::Integer { minimum, maximum } => {
            let parsed = value
                .parse::<i64>()
                .with_context(|| format!("field {} is not an integer", field.name))?;
            if parsed < *minimum || parsed > *maximum {
                bail!("field {} is outside its declared range", field.name);
            }
        }
    }
    Ok(())
}

fn output_object_schema(fields: &[WorkflowOutputField]) -> Value {
    let properties = fields
        .iter()
        .map(|field| {
            let schema = match &field.kind {
                WorkflowOutputKind::NaturalLanguage { max_length } => {
                    json!({"type":"string","maxLength":max_length,"description":field.description})
                }
                WorkflowOutputKind::Choice { values } => {
                    json!({"type":"string","enum":values,"description":field.description})
                }
                WorkflowOutputKind::Integer { minimum, maximum } => {
                    json!({"type":"string","pattern":format!("^-?[0-9]+$"),"description":format!("{} (between {minimum} and {maximum})",field.description)})
                }
            };
            (field.name.clone(), schema)
        })
        .collect::<serde_json::Map<_, _>>();
    let required = fields
        .iter()
        .filter(|field| field.required)
        .map(|field| field.name.clone())
        .collect::<Vec<_>>();
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}

fn base_extended(input: &WorkflowInput) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("recording_id".into(), input.recording_id.to_string()),
        (
            "transcript_run_id".into(),
            input.transcript_run_id.to_string(),
        ),
        ("recorded_at_ms".into(), input.recorded_at_ms.to_string()),
        ("transcript".into(), input.transcript.clone()),
    ])
}

fn workflow_input(message: &Message) -> Option<WorkflowInput> {
    let Message::TranscriptCompleted(run) = message else {
        return None;
    };
    Some(WorkflowInput {
        recording_id: run.recording_id,
        transcript_run_id: run.run_id,
        recorded_at_ms: run.recorded_at_ms,
        transcript: run
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    })
}

fn validate_webhook(target: &str) -> Result<()> {
    if target.starts_with("https://")
        || target.starts_with("http://127.0.0.1:")
        || target.starts_with("http://localhost:")
    {
        Ok(())
    } else {
        bail!("webhooks require HTTPS or an explicit loopback URL")
    }
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().ok_or_else(|| anyhow!("path has no parent"))?)?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    File::open(&temporary)?.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn truncate(value: &str, length: usize) -> String {
    value.chars().take(length).collect()
}

fn action_id(run_id: Uuid, transition_id: &str) -> Uuid {
    Uuid::new_v5(&run_id, transition_id.as_bytes())
}

fn message_type(message: &Message) -> &'static str {
    match message {
        Message::StartWorker { .. } => "start_worker",
        Message::StopWorker { .. } => "stop_worker",
        Message::WorkerHealth(_) => "worker_health",
        Message::SourceAdapterSelectionRequested { .. } => "source_adapter_selection_requested",
        Message::SourceCommandRequested(_) => "source_command_requested",
        Message::SourceStatusChanged(_) => "source_status_changed",
        Message::ReleaseSourceRequested => "release_source_requested",
        Message::RecordingAvailable(_) => "recording_available",
        Message::AudioIngested(_) => "audio_ingested",
        Message::TranscriptionRequested(_) => "transcription_requested",
        Message::TranscriptCompleted(_) => "transcript_completed",
        Message::DerivationRequested(_) => "derivation_requested",
        Message::ActionProposed(_) => "action_proposed",
        Message::ActionRevisionRequested(_) => "action_revision_requested",
        Message::ActionApproved { .. } => "action_approved",
        Message::ActionRejected { .. } => "action_rejected",
        Message::ActionDelivered(_) => "action_delivered",
        Message::ActionUndoRequested { .. } => "action_undo_requested",
        Message::ActionUndone(_) => "action_undone",
        Message::DerivationCompleted(_) => "derivation_completed",
        Message::NotificationDelivered(_) => "notification_delivered",
        Message::RecordingReviewed(_) => "recording_reviewed",
        Message::TranscriptAnnotated(_) => "transcript_annotated",
        Message::WorkflowRunRequested(_) => "workflow_run_requested",
        Message::WorkflowRoutingRequested(_) => "workflow_routing_requested",
        Message::WorkflowTransitionAdjusted(_) => "workflow_transition_adjusted",
        Message::WorkflowEffectUndoRequested(_) => "workflow_effect_undo_requested",
        Message::WorkflowRunConfirmed(_) => "workflow_run_confirmed",
        Message::WorkflowRunRecorded(_) => "workflow_run_recorded",
        Message::WorkflowEffectUndone(_) => "workflow_effect_undone",
        Message::ServiceDelivered(_) => "service_delivered",
        Message::Failure(_) => "failure",
        Message::LegacySourceEvent(_) => "legacy_source_event",
    }
}

fn emit(envelope: &Envelope) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(envelope.to_ndjson()?.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let runner = Runner::new(cli.state_root, cli.lake_root);
    let (model, url) = runner.model_configuration();
    let readiness = runner.ensure_24b_model(&model, &url).await;
    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
        role: WorkerRole::Services,
        state: if readiness.is_ok() {
            HealthState::Ready
        } else {
            HealthState::Degraded
        },
        detail: readiness.map_or_else(
            |error| format!("local 24B workflow model unavailable: {error:#}"),
            |_| format!("local 24B workflow model ready: {model}"),
        ),
        backlog: 0,
    })))?;
    let mut stdin = AsyncBufReader::new(tokio::io::stdin()).lines();
    let mut poll = interval(Duration::from_millis(cli.poll_ms));
    loop {
        tokio::select! {
            line = stdin.next_line() => match line? {
                Some(line) => {
                    let envelope = Envelope::from_json(&line)?;
                    if let Err(error) = runner.command(envelope).await {
                        runner.failure("workflow_command_failed", format!("{error:#}"))?;
                    }
                }
                None => break,
            },
            _ = poll.tick() => runner.cycle().await?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_audio_lake_contracts::{WorkflowState, WorkflowTransition};

    #[test]
    fn an_external_action_undo_is_ignored_by_a_workflow_that_does_not_own_it() {
        let root = tempfile::tempdir().unwrap();
        let runner = Runner::new(root.path().join("state"), root.path().join("lake"));
        let undo = personal_audio_lake_contracts::ActionUndo {
            action_id: Uuid::new_v4(),
            target: "fixture.calendar".into(),
            undone_at_ms: 1,
        };
        assert!(runner.finish_mac_undo(&undo).is_ok());
    }

    #[test]
    fn oracle_output_cannot_escape_the_declared_transition_or_fields() {
        let state = WorkflowState {
            id: "message_ingested".into(),
            description: String::new(),
            selector: WorkflowSelector::LocalModel {
                instruction: "route".into(),
            },
            transitions: vec![WorkflowTransition {
                id: "create".into(),
                description: "create reminder".into(),
                destination_state_id: "done".into(),
                output_fields: vec![WorkflowOutputField {
                    name: "reminder.title".into(),
                    description: "Reminder text".into(),
                    kind: WorkflowOutputKind::NaturalLanguage { max_length: 100 },
                    required: true,
                }],
                action: WorkflowAction::None,
            }],
        };
        assert!(
            validate_selection(
                &state,
                &WorkflowSelection {
                    transition_id: "invented".into(),
                    outputs: BTreeMap::new()
                }
            )
            .is_err()
        );
        assert!(
            validate_selection(
                &state,
                &WorkflowSelection {
                    transition_id: "create".into(),
                    outputs: BTreeMap::from([("undeclared".into(), "value".into())])
                }
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_human_can_fill_every_local_model_slot_without_owning_deterministic_control() {
        let model = WorkflowSelector::LocalModel {
            instruction: "choose".into(),
        };
        assert!(human_can_select(&model, WorkflowRunRequester::User));
        assert!(!human_can_select(&model, WorkflowRunRequester::Automatic));
        assert!(!human_can_select(
            &WorkflowSelector::Deterministic {
                transition_id: "next".into()
            },
            WorkflowRunRequester::User
        ));
        assert!(!human_can_select(
            &WorkflowSelector::PhraseMatch {
                field: "transcript".into(),
                cases: vec![],
                fallback_transition_id: "next".into()
            },
            WorkflowRunRequester::User
        ));
        let expected = WorkflowSelection {
            transition_id: "chosen".into(),
            outputs: BTreeMap::from([("free_text".into(), "Call the dentist".into())]),
        };
        let mut human = HumanSelectionOracle {
            selection: Some(expected.clone()),
        };
        assert_eq!(select_from_oracle(&mut human, ()).await.unwrap(), expected);
    }

    #[test]
    fn deterministic_phrase_selector_tolerates_a_transcription_error() {
        assert!(tolerant_phrase_match(
            "please remimd me to buy milk",
            "remind me"
        ));
        assert!(!tolerant_phrase_match(
            "we discussed buying milk",
            "remind me"
        ));
        let descriptive_message =
            "The documentary mentioned emails but did not request any action.";
        for action_signal in [
            "remind me",
            "remember to",
            "i need to",
            "i should",
            "i will",
            "calendar add",
            "schedule",
            "meeting",
            "appointment",
        ] {
            assert!(!tolerant_phrase_match(descriptive_message, action_signal));
        }
    }

    #[test]
    fn mac_action_gate_regresses_the_observed_recording_corpus() {
        assert_eq!(
            text_length_transition(&"x".repeat(64_621), 4_000, "within_limit", "too_long"),
            "too_long"
        );
        assert_eq!(
            text_length_transition(&"x".repeat(4_000), 4_000, "within_limit", "too_long"),
            "within_limit"
        );
        let opt_out = [
            "do not activate",
            "don't activate",
            "do not create",
            "don't create",
            "do not add",
            "don't add",
            "no hooks",
            "without activating",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let candidate = [
            "remind me",
            "remember to",
            "add to my todo",
            "add to my calendar",
            "need to",
            "i need to",
            "i have to",
            "i should",
            "i will",
            "meeting",
            "appointment",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let route = |transcript: &str| {
            phrase_match_transition(
                transcript,
                [
                    ("opt_out", opt_out.as_slice()),
                    ("candidate", candidate.as_slice()),
                ],
                "not_candidate",
            )
        };

        assert_eq!(
            route("The documentary about email systems was surprisingly interesting."),
            "not_candidate"
        );
        assert_eq!(
            route("That was the most thoughtful message I heard all week."),
            "not_candidate"
        );
        assert_eq!(
            route("This is a test to see if notifications work. Please do not activate any hooks."),
            "opt_out"
        );
        assert_eq!(
            route("Remind me tomorrow to email the accountant."),
            "candidate"
        );
        assert_eq!(
            route("Need to buy replacement curtains for summer."),
            "candidate"
        );
        assert_eq!(route("Hello, hello, hello."), "not_candidate");
    }

    #[test]
    fn action_evidence_must_be_a_nonempty_transcript_excerpt() {
        let transcript = "Remind me tomorrow at noon to call Jess.";
        assert!(normalized_contains(transcript, "tomorrow at noon"));
        assert!(normalized_contains(transcript, "Remind me tomorrow"));
        assert!(!normalized_contains(transcript, "email the accountant"));
        assert!(!normalized_contains(transcript, ""));
        assert!(
            validate_mac_action_grounding(
                transcript,
                1_700_000_000_000,
                Some(1_700_043_200_000),
                Some(transcript),
                true,
                Some("tomorrow at noon"),
            )
            .is_ok()
        );
        assert!(
            validate_mac_action_grounding(
                transcript,
                1_700_000_000_000,
                Some(1_700_000_000_000),
                Some(transcript),
                true,
                Some("tomorrow at noon"),
            )
            .is_err()
        );
        assert!(
            validate_mac_action_grounding(
                transcript,
                1_700_000_000_000,
                Some(1_700_043_200_000),
                Some("Remind me tomorrow"),
                true,
                Some("at noon"),
            )
            .is_err()
        );
    }

    #[test]
    fn distinct_clauses_can_trigger_multiple_hooks_but_one_clause_cannot() {
        let transcript = "Remind me tomorrow to call Jess. Add my dentist appointment next Tuesday at 2pm to my calendar. I also need to buy milk.";
        let mut extended = BTreeMap::from([
            ("transcript".into(), transcript.into()),
            ("recorded_at_ms".into(), "1784110000000".into()),
            (
                "reminder_accepted_evidence".into(),
                "Remind me tomorrow to call Jess.".into(),
            ),
            (
                "calendar_evidence".into(),
                "Add my dentist appointment next Tuesday at 2pm to my calendar.".into(),
            ),
            ("calendar_due_at_ms".into(), "1784710800000".into()),
            ("calendar_time_phrase".into(), "next Tuesday at 2pm".into()),
        ]);
        assert!(action_evidence_is_valid(
            &extended,
            "transcript",
            "calendar_evidence",
            &["reminder_accepted_evidence".into()],
            Some("calendar_due_at_ms"),
            Some("calendar_time_phrase"),
        ));

        extended.insert(
            "calendar_evidence".into(),
            "Remind me tomorrow to call Jess.".into(),
        );
        extended.insert("calendar_time_phrase".into(), "tomorrow".into());
        assert!(!action_evidence_is_valid(
            &extended,
            "transcript",
            "calendar_evidence",
            &["reminder_accepted_evidence".into()],
            Some("calendar_due_at_ms"),
            Some("calendar_time_phrase"),
        ));
    }

    #[test]
    fn native_integration_values_are_passed_as_arguments_not_script_source() {
        assert_eq!(
            html_escape("<script>&\"'"),
            "&lt;script&gt;&amp;&quot;&#39;"
        );
        assert_eq!(split_contact_name("Ada Lovelace"), ("Ada", "Lovelace"));
        assert_eq!(split_contact_name("Prince"), ("Prince", ""));
        assert!(looks_like_email("person@example.com"));
        assert!(!looks_like_email("not an address"));
        assert_eq!(safe_file_stem("../../Tax notes?.txt"), "Tax-notes-txt");
        assert!(NOTES_CREATE_SCRIPT.contains("Apple Notes folder not found"));
        assert!(!NOTES_CREATE_SCRIPT.contains("            try\n"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_apple_scripts_compile() {
        let temp = tempfile::tempdir().unwrap();
        for (name, script) in [
            ("notes-create", NOTES_CREATE_SCRIPT),
            ("notes-delete", NOTES_DELETE_SCRIPT),
            ("mail-draft-create", MAIL_DRAFT_CREATE_SCRIPT),
            ("mail-draft-close", MAIL_DRAFT_CLOSE_SCRIPT),
            ("mail-draft-delete", MAIL_DRAFT_DELETE_SCRIPT),
            ("contact-create", CONTACT_CREATE_SCRIPT),
            ("contact-delete", CONTACT_DELETE_SCRIPT),
        ] {
            let source = temp.path().join(format!("{name}.applescript"));
            let compiled = temp.path().join(format!("{name}.scpt"));
            fs::write(&source, script).unwrap();
            let mut last_error = String::new();
            let mut succeeded = false;
            for attempt in 0..3 {
                let output = Command::new("/usr/bin/osacompile")
                    .args(["-o"])
                    .arg(&compiled)
                    .arg(&source)
                    .output()
                    .unwrap();
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                if output.status.success() && compiled.exists() && !stderr.contains("error:") {
                    succeeded = true;
                    break;
                }
                last_error = stderr;
                let xpc_unavailable = last_error.contains("Connection invalid")
                    || last_error.contains("Connection Invalid");
                if !xpc_unavailable || attempt == 2 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            if !succeeded
                && (last_error.contains("Connection invalid")
                    || last_error.contains("Connection Invalid"))
            {
                eprintln!(
                    "skipping Apple application terminology compilation because the host XPC service is unavailable: {last_error}"
                );
                return;
            }
            assert!(succeeded, "{name} did not compile: {last_error}");
        }
    }

    #[test]
    fn text_exports_are_unique_and_undo_is_confined_to_the_export_folder() {
        let temp = tempfile::tempdir().unwrap();
        let first = create_text_file_in(temp.path(), "Meeting notes", "First", "").unwrap();
        let second = create_text_file_in(temp.path(), "Meeting notes", "Second", "").unwrap();
        assert_ne!(first.external_id, second.external_id);
        assert_eq!(fs::read_to_string(&first.external_id).unwrap(), "First");
        remove_text_file_in(temp.path(), Path::new(&first.external_id)).unwrap();
        assert!(!Path::new(&first.external_id).exists());
        assert!(remove_text_file_in(temp.path(), &temp.path().join("../outside.txt")).is_err());
    }

    #[tokio::test]
    async fn a_new_workflow_starts_at_the_live_manifest_cursor_without_backfilling() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let lake = temp.path().join("lake");
        let manifests = lake.join("manifests");
        fs::create_dir_all(&manifests).unwrap();
        fs::write(manifests.join("events.ndjson"), "old\nold\nold\n").unwrap();
        fs::write(manifests.join("sequence"), "2\n").unwrap();
        let workflow = WorkflowDefinition {
            id: "future-only".into(),
            name: "Future only".into(),
            description: "Do not replay history".into(),
            enabled: true,
            subscriptions: vec!["transcript_completed".into()],
            initial_state_id: "message_ingested".into(),
            states: vec![],
        };
        let runner = Runner::new(state.clone(), lake);
        runner.run_subscriptions(&workflow).await.unwrap();
        let checkpoint: Checkpoint = serde_json::from_slice(
            &fs::read(state.join("workflow-state/future-only/checkpoint.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(checkpoint.cursor, 3);
    }
}
