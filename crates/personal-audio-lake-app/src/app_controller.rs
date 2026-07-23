//! Side-effect adapter for renderer-independent application effects.

use std::{collections::BTreeMap, fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use personal_audio_lake_contracts::{
    ActionKind, ActionProposal, ActionRevisionRequest, Envelope, Message, RecordingReview,
    ReviewOutcome, SourceCommand, SourceOperation, SourceState, SourceStatus, TranscriptAnnotation,
    WorkflowAction, WorkflowAdapter, WorkflowDefinition, WorkflowEffectUndoRequest, WorkflowInput,
    WorkflowOutputField, WorkflowOutputKind, WorkflowPhraseCase, WorkflowRoutingRequest,
    WorkflowRunConfirmation, WorkflowRunRequest, WorkflowRunRequester, WorkflowSelector,
    WorkflowState, WorkflowTransition, WorkflowTransitionAdjustment,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    Config, read_model, source_catalog,
    view_model::{
        Automation, Effect, Permission, PermissionState, RoutingMode, Settings, SourceOption,
        ViewModel,
    },
};

#[derive(Clone)]
pub struct AppController {
    config: Config,
}

impl AppController {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn state_root(&self) -> &Path {
        &self.config.state_root
    }

    pub fn load_model(&self) -> Result<ViewModel> {
        seed_default_workflows(&self.config.state_root, &self.config.lake_root)?;
        let mut snapshot = read_model::fetch(&self.config.api_bind, &self.config.state_root)
            .context("load the application model from the local service")?;
        apply_recording_titles(&self.config.state_root, &mut snapshot);
        let (automations, workflows, settings) = (
            load_automations(&self.config.state_root),
            load_workflows(&self.config.state_root),
            load_settings(&self.config, &snapshot),
        );
        let mut model = ViewModel::new(snapshot, automations, workflows, settings);
        self.load_selected_segments(&mut model)?;
        Ok(model)
    }

    pub fn apply_all(&self, effects: Vec<Effect>, model: &mut ViewModel) -> Result<()> {
        for effect in effects {
            self.apply(effect, model)?;
        }
        Ok(())
    }

    pub fn apply(&self, effect: Effect, model: &mut ViewModel) -> Result<()> {
        match effect {
            Effect::Reload => {
                let mut snapshot =
                    read_model::fetch(&self.config.api_bind, &self.config.state_root)?;
                apply_recording_titles(&self.config.state_root, &mut snapshot);
                let settings = load_settings(&self.config, &snapshot);
                model.replace_snapshot(snapshot);
                model.replace_configuration(
                    load_automations(&self.config.state_root),
                    load_workflows(&self.config.state_root),
                    settings,
                );
                self.load_selected_segments(model)?;
            }
            Effect::LoadRecording(recording_id) => {
                let segments = read_model::fetch_segments(
                    &self.config.api_bind,
                    &self.config.state_root,
                    recording_id,
                )?;
                model.replace_segments(recording_id, segments);
            }
            Effect::SyncSource => {
                let config = load_current_config(&self.config);
                if config.source_adapter_id.trim().is_empty()
                    || config.source_adapter_binary.trim().is_empty()
                {
                    bail!("choose a recording source before syncing");
                }
                enqueue(
                    &config.state_root,
                    Envelope::root(Message::SourceCommandRequested(SourceCommand {
                        adapter_id: config.source_adapter_id.clone(),
                        source_id: config.selected_source_id.clone(),
                        operation: SourceOperation::Discover {
                            duration_ms: config.scan_duration_ms,
                            allow_pairing: true,
                        },
                    })),
                )?;
            }
            Effect::ConfigureSourceAdapter { adapter_id } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::SourceAdapterSelectionRequested { adapter_id }),
            )?,
            Effect::ReleaseSource => enqueue(
                &self.config.state_root,
                Envelope::root(Message::ReleaseSourceRequested),
            )?,
            Effect::PlayAudio {
                recording_id: _,
                relative_path,
                start_ms,
            } => {
                let path = self.config.lake_root.join(relative_path);
                if !path.is_file() {
                    bail!("audio file is not available at {}", path.display());
                }
                if let Some(start_ms) = start_ms.filter(|value| *value > 0)
                    && Path::new("/opt/homebrew/bin/ffplay").is_file()
                {
                    Command::new("/opt/homebrew/bin/ffplay")
                        .args(["-nodisp", "-autoexit", "-loglevel", "quiet", "-ss"])
                        .arg(format!("{:.3}", start_ms as f64 / 1000.0))
                        .arg(path)
                        .spawn()
                        .context("play recording from transcript timestamp")?;
                } else {
                    Command::new("/usr/bin/afplay")
                        .arg(path)
                        .spawn()
                        .context("play recording audio")?;
                }
            }
            Effect::SaveRecordingTitle {
                recording_id,
                title,
            } => {
                save_recording_title(&self.config.state_root, recording_id, &title)?;
            }
            Effect::ReviewRecording {
                recording_id,
                looks_right,
                note,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::RecordingReviewed(RecordingReview {
                    review_id: Uuid::new_v4(),
                    recording_id,
                    outcome: if looks_right {
                        ReviewOutcome::Confirmed
                    } else {
                        ReviewOutcome::NeedsAttention
                    },
                    note,
                    reviewed_at_ms: Utc::now().timestamp_millis(),
                })),
            )?,
            Effect::CorrectTranscript {
                recording_id,
                transcript_run_id,
                ordinal,
                replacement,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::TranscriptAnnotated(TranscriptAnnotation {
                    annotation_id: Uuid::new_v4(),
                    recording_id,
                    transcript_run_id,
                    ordinal,
                    replacement_text: replacement,
                    reason: Some("Corrected in Captain’s Log".into()),
                    annotated_at_ms: Utc::now().timestamp_millis(),
                })),
            )?,
            Effect::DecideAction {
                action_id,
                approved,
                reason,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(if approved {
                    Message::ActionApproved { action_id }
                } else {
                    Message::ActionRejected {
                        action_id,
                        reason: reason.unwrap_or_else(|| "Rejected in Captain’s Log".into()),
                    }
                }),
            )?,
            Effect::UndoAction { action_id } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::ActionUndoRequested { action_id }),
            )?,
            Effect::CreateAction {
                action_id,
                recording_id,
                transcript_run_id,
                kind,
                title,
                due_at_ms,
            } => {
                let proposal = Envelope::root(Message::ActionProposed(ActionProposal {
                    action_id,
                    recording_id,
                    transcript_run_id,
                    kind,
                    title,
                    due_at_ms,
                    original_time_phrase: None,
                    timezone: self.config.timezone.clone(),
                    confidence: 1.0,
                    evidence: Vec::new(),
                    extractor_version: "human-v1".into(),
                }));
                let mut approval =
                    Envelope::caused_by(&proposal, Message::ActionApproved { action_id });
                approval.emitted_at_ms = proposal.emitted_at_ms + 1;
                enqueue_many(&self.config.state_root, [proposal, approval])?;
            }
            Effect::ReviseAction {
                action_id,
                kind,
                title,
                due_at_ms,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::ActionRevisionRequested(ActionRevisionRequest {
                    action_id,
                    kind,
                    title,
                    due_at_ms,
                    requested_at_ms: Utc::now().timestamp_millis(),
                })),
            )?,
            Effect::RunWorkflow {
                workflow_id,
                resume_state_id,
                recording_id,
                transcript_run_id,
                recorded_at_ms,
                transcript,
                selection,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::WorkflowRunRequested(WorkflowRunRequest {
                    run_id: Uuid::new_v4(),
                    workflow_id,
                    input: WorkflowInput {
                        recording_id,
                        transcript_run_id,
                        recorded_at_ms,
                        transcript,
                    },
                    resume_state_id: Some(resume_state_id),
                    forced_selection: Some(selection),
                    requested_by: WorkflowRunRequester::User,
                    requested_at_ms: Utc::now().timestamp_millis(),
                })),
            )?,
            Effect::RunWorkflowWithLocalModel {
                workflow_id,
                resume_state_id,
                recording_id,
                transcript_run_id,
                recorded_at_ms,
                transcript,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::WorkflowRunRequested(WorkflowRunRequest {
                    run_id: Uuid::new_v4(),
                    workflow_id,
                    input: WorkflowInput {
                        recording_id,
                        transcript_run_id,
                        recorded_at_ms,
                        transcript,
                    },
                    resume_state_id: Some(resume_state_id),
                    forced_selection: None,
                    requested_by: WorkflowRunRequester::LocalModel,
                    requested_at_ms: Utc::now().timestamp_millis(),
                })),
            )?,
            Effect::RouteWithLocalModel {
                recording_id,
                transcript_run_id,
                recorded_at_ms,
                transcript,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::WorkflowRoutingRequested(WorkflowRoutingRequest {
                    request_id: Uuid::new_v4(),
                    input: WorkflowInput {
                        recording_id,
                        transcript_run_id,
                        recorded_at_ms,
                        transcript,
                    },
                    requested_at_ms: Utc::now().timestamp_millis(),
                })),
            )?,
            Effect::AdjustWorkflowRun {
                run_id,
                state_id,
                selection,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::WorkflowTransitionAdjusted(
                    WorkflowTransitionAdjustment {
                        run_id,
                        state_id,
                        selection,
                        adjusted_at_ms: Utc::now().timestamp_millis(),
                    },
                )),
            )?,
            Effect::UndoWorkflowStep {
                run_id,
                transition_id,
            } => enqueue(
                &self.config.state_root,
                Envelope::root(Message::WorkflowEffectUndoRequested(
                    WorkflowEffectUndoRequest {
                        run_id,
                        transition_id,
                        requested_at_ms: Utc::now().timestamp_millis(),
                    },
                )),
            )?,
            Effect::ConfirmWorkflowRun { run_id } => {
                let directory = self
                    .config
                    .state_root
                    .join("workflow-runs")
                    .join(run_id.to_string());
                fs::create_dir_all(&directory)?;
                fs::write(
                    directory.join("confirmed"),
                    Utc::now().timestamp_millis().to_string(),
                )?;
                enqueue(
                    &self.config.state_root,
                    Envelope::root(Message::WorkflowRunConfirmed(WorkflowRunConfirmation {
                        run_id,
                        confirmed_at_ms: Utc::now().timestamp_millis(),
                    })),
                )?;
            }
            Effect::SaveAutomation(automation) => {
                save_automation(&self.config.state_root, &automation)?;
            }
            Effect::SaveWorkflow(workflow) => save_workflow(&self.config.state_root, &workflow)?,
            Effect::DeleteWorkflow(id) => delete_workflow(&self.config.state_root, &id)?,
            Effect::RequestPermission(permission) => {
                request_permission(&self.config.state_root, permission)?;
            }
        }
        Ok(())
    }

    fn load_selected_segments(&self, model: &mut ViewModel) -> Result<()> {
        let Some(recording_id) = model.selected_id() else {
            return Ok(());
        };
        let segments = read_model::fetch_segments(
            &self.config.api_bind,
            &self.config.state_root,
            recording_id,
        )?;
        model.replace_segments(recording_id, segments);
        Ok(())
    }
}

fn title_overlay_path(state_root: &Path) -> std::path::PathBuf {
    state_root.join("annotations/recording-titles.json")
}

fn apply_recording_titles(state_root: &Path, snapshot: &mut read_model::AppSnapshot) {
    let titles = fs::read(title_overlay_path(state_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<BTreeMap<Uuid, String>>(&bytes).ok())
        .unwrap_or_default();
    for recording in &mut snapshot.recordings {
        recording.title = titles.get(&recording.recording_id).cloned();
    }
}

fn save_recording_title(state_root: &Path, recording_id: Uuid, title: &str) -> Result<()> {
    let path = title_overlay_path(state_root);
    let mut titles = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<BTreeMap<Uuid, String>>(&bytes).ok())
        .unwrap_or_default();
    titles.insert(recording_id, title.to_owned());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&titles)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn enqueue(state_root: &Path, envelope: Envelope) -> Result<()> {
    enqueue_many(state_root, [envelope])
}

fn enqueue_many(state_root: &Path, envelopes: impl IntoIterator<Item = Envelope>) -> Result<()> {
    let inbox = state_root.join("control/inbox");
    fs::create_dir_all(&inbox)?;
    for envelope in envelopes {
        let target = inbox.join(format!("{}-{}.json", envelope.emitted_at_ms, envelope.id));
        let temporary = target.with_extension("tmp");
        fs::write(&temporary, envelope.to_ndjson()?)?;
        fs::rename(temporary, target)?;
    }
    Ok(())
}

fn load_settings(config: &Config, snapshot: &read_model::AppSnapshot) -> Settings {
    let current_config = load_current_config(config);
    let selected_source_id = current_config
        .selected_source_id
        .as_deref()
        .or(config.selected_source_id.as_deref());
    let source_adapter_configured = !current_config.source_adapter_id.trim().is_empty()
        && !current_config.source_adapter_binary.trim().is_empty();
    let source_configured = selected_source_id.is_some();
    let latest_source = latest_source_status(&config.state_root, selected_source_id);
    let selected_adapter_id =
        source_adapter_configured.then_some(current_config.source_adapter_id.as_str());
    let selected_adapter = selected_adapter_id.and_then(source_catalog::find);
    let transfer = snapshot
        .workers
        .iter()
        .find(|worker| worker.role == "transfer_ingress");
    let notification = fs::read(config.state_root.join("notifications/status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("authorization")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .map_or(PermissionState::NotRequested, |state| {
            permission_state(&state)
        });
    let eventkit = fs::read(config.state_root.join("permissions/eventkit.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or_default();
    Settings {
        recording_transfer_state: transfer.map(|worker| worker.state),
        recording_transfer_detail: transfer.map_or_else(
            || "Waiting for local recording transfer status".into(),
            |worker| worker.detail.clone(),
        ),
        source_name: latest_source.as_ref().map_or_else(
            || selected_adapter.map_or_else(|| "None selected".into(), |value| value.name.into()),
            |status| status.display_name.clone(),
        ),
        source_state: latest_source
            .map(|status| status.state)
            .or(source_configured.then_some(SourceState::Bound)),
        source_adapter_configured,
        source_configured,
        source_options: source_catalog::all()
            .iter()
            .map(|descriptor| SourceOption {
                id: descriptor.id.into(),
                name: descriptor.name.into(),
                description: descriptor.description.into(),
                selected: selected_adapter_id == Some(descriptor.id),
            })
            .collect(),
        local_model: semantic_model(&config.state_root)
            .unwrap_or_else(|| "mistral-small3.2:latest".into()),
        data_lake: config.lake_root.display().to_string(),
        starts_at_login: true,
        notifications: notification,
        calendar: eventkit
            .get("calendar")
            .and_then(Value::as_str)
            .map(permission_state)
            .unwrap_or(PermissionState::NotRequested),
        reminders: eventkit
            .get("reminders")
            .and_then(Value::as_str)
            .map(permission_state)
            .unwrap_or(PermissionState::NotRequested),
    }
}

fn load_current_config(config: &Config) -> Config {
    fs::read_to_string(config.state_root.join("config.toml"))
        .ok()
        .and_then(|value| toml::from_str::<Config>(&value).ok())
        .unwrap_or_else(|| config.clone())
}

fn latest_source_status(
    state_root: &Path,
    selected_source_id: Option<&str>,
) -> Option<SourceStatus> {
    fs::read_to_string(state_root.join("status/events.ndjson"))
        .ok()?
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|wrapper| {
            serde_json::from_value::<Envelope>(wrapper.get("event")?.clone()).ok()
        })
        .find_map(|envelope| match envelope.message {
            Message::SourceStatusChanged(status)
                if selected_source_id.is_none_or(|selected| selected == status.source_id) =>
            {
                Some(status)
            }
            _ => None,
        })
}

fn permission_state(value: &str) -> PermissionState {
    match value {
        "authorized" | "allowed" | "provisional" => PermissionState::Allowed,
        "denied" => PermissionState::Denied,
        "restricted" => PermissionState::Restricted,
        "not_determined" | "not requested" => PermissionState::NotRequested,
        _ => PermissionState::Unknown,
    }
}

fn semantic_model(state_root: &Path) -> Option<String> {
    fs::read(state_root.join("rules.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("semantic_model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

fn load_automations(state_root: &Path) -> Vec<Automation> {
    let Some(rules) = fs::read(state_root.join("rules.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.get("rules").and_then(Value::as_array).cloned())
    else {
        return Vec::new();
    };
    rules
        .iter()
        .filter_map(|rule| {
            let id = rule.get("id")?.as_str()?.to_owned();
            let mode = match rule
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("explicit")
            {
                "implicit" => RoutingMode::Implicit,
                "hybrid" => RoutingMode::Hybrid,
                _ => RoutingMode::Explicit,
            };
            let trigger = rule
                .get("phrases")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" / ");
            Some(Automation {
                name: humanize(&id),
                id,
                mode,
                trigger: if trigger.is_empty() {
                    "No fixed phrase".into()
                } else {
                    trigger
                },
                destination: rule
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("proposal")
                    .to_owned(),
                enabled: rule
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn load_workflows(state_root: &Path) -> Vec<personal_audio_lake_contracts::WorkflowDefinition> {
    let Ok(entries) = fs::read_dir(state_root.join("services")) else {
        return Vec::new();
    };
    let mut workflows = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|entry| fs::read(entry.path()).ok())
        .filter_map(|bytes| {
            serde_json::from_slice::<personal_audio_lake_contracts::WorkflowDefinition>(&bytes).ok()
        })
        .collect::<Vec<_>>();
    workflows.sort_by(|left, right| left.id.cmp(&right.id));
    workflows
}

fn seed_default_workflows(state_root: &Path, lake_root: &Path) -> Result<()> {
    let marker = state_root.join("workflow-seed-v5");
    if marker.exists() {
        return Ok(());
    }
    let previous_defaults = v4_default_mac_workflows();
    for workflow in &previous_defaults {
        let path = state_root
            .join("services")
            .join(format!("{}.json", workflow.id));
        let is_unedited_default = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WorkflowDefinition>(&bytes).ok())
            .is_some_and(|current| current == *workflow);
        if is_unedited_default {
            fs::remove_file(path)?;
        }
    }
    let mut existing_event_count = None;
    for workflow in default_mac_workflows() {
        let path = state_root
            .join("services")
            .join(format!("{}.json", workflow.id));
        let existed = path.exists();
        if !existed {
            save_workflow(state_root, &workflow)?;
            let event_count = *existing_event_count.get_or_insert_with(|| {
                fs::File::open(lake_root.join("manifests/events.ndjson"))
                    .map(|file| std::io::BufRead::lines(std::io::BufReader::new(file)).count())
                    .unwrap_or_default()
            });
            let checkpoint = state_root
                .join("workflow-state")
                .join(&workflow.id)
                .join("checkpoint.json");
            fs::create_dir_all(checkpoint.parent().expect("checkpoint has a parent"))?;
            fs::write(
                checkpoint,
                serde_json::to_vec_pretty(&json!({"cursor":event_count}))?,
            )?;
        }
    }
    disable_migrated_legacy_rules(state_root)?;
    fs::write(
        marker,
        "A single evidence-bound Mac Actions router replaced the three competing defaults.\n",
    )?;
    Ok(())
}

fn default_mac_workflows() -> Vec<WorkflowDefinition> {
    vec![default_mac_actions_workflow()]
}

fn v4_default_mac_workflows() -> Vec<WorkflowDefinition> {
    vec![
        v4_default_mac_workflow(
            "reminders",
            "Reminders",
            "Create a macOS reminder when the message asks to be reminded.",
            ActionKind::Reminder,
            "A deterministic intent gate matched reminder language. Select create only when the speaker genuinely asks to remember something. Fill title with the concise reminder text. If a time is stated, fill due_at_ms as Unix milliseconds relative to recorded_at_ms; otherwise omit it.",
            true,
            &[
                "remind me",
                "reminder",
                "remember to",
                "don't forget",
                "dont let me forget",
                "i need to remember",
                "i should remember",
                "make sure i remember",
            ],
        ),
        v4_default_mac_workflow(
            "todos",
            "Todos",
            "Add an item to the default macOS Reminders list when the message expresses a todo.",
            ActionKind::Todo,
            "A deterministic intent gate matched personal task language. Select create only for a genuine unfinished task or commitment by the speaker. Do not convert feelings, observations, or descriptions into tasks. Fill title with the concise todo text.",
            false,
            &[
                "add to my todo",
                "add to my to do",
                "todo",
                "to do",
                "i need to",
                "i have to",
                "i should",
                "i must",
                "i'll",
                "i will",
                "i am going to",
                "i'm going to",
                "make sure i",
            ],
        ),
        v4_default_mac_workflow(
            "calendar",
            "Calendar",
            "Add an event to the default macOS Calendar when the message asks to schedule one.",
            ActionKind::CalendarEvent,
            "A deterministic intent gate matched scheduling language. Select create only for a genuine future event the speaker intends to schedule or attend and only when a start time can be resolved from the transcript. Never use recorded_at_ms itself as an invented event time. Fill title and due_at_ms as Unix milliseconds relative to recorded_at_ms. Otherwise select ignore.",
            true,
            &[
                "for my calendar add",
                "calendar add",
                "add to my calendar",
                "schedule",
                "calendar event",
                "meeting",
                "appointment",
                "meet with",
                "i am seeing",
                "i'm seeing",
                "booked for",
            ],
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn v4_default_mac_workflow(
    id: &str,
    name: &str,
    description: &str,
    kind: ActionKind,
    instruction: &str,
    has_due_time: bool,
    phrases: &[&str],
) -> WorkflowDefinition {
    let mut output_fields = vec![WorkflowOutputField {
        name: "title".into(),
        description: format!("User-facing text for the {name} item"),
        kind: WorkflowOutputKind::NaturalLanguage { max_length: 240 },
        required: true,
    }];
    if has_due_time {
        output_fields.push(WorkflowOutputField {
            name: "due_at_ms".into(),
            description: "Resolved Unix timestamp in milliseconds".into(),
            kind: WorkflowOutputKind::Integer {
                minimum: 1_577_836_800_000,
                maximum: 4_102_444_800_000,
            },
            required: kind == ActionKind::CalendarEvent,
        });
    }
    let interpret = WorkflowState {
        id: "interpret".into(),
        description: format!("Interpret the bounded {name} candidate"),
        selector: WorkflowSelector::LocalModel {
            instruction: instruction.into(),
        },
        transitions: vec![
            WorkflowTransition {
                id: "create".into(),
                description: format!("Create the {name} item"),
                destination_state_id: "done".into(),
                output_fields,
                action: WorkflowAction::Effect {
                    adapter: WorkflowAdapter::SystemAction {
                        kind,
                        title_field: "title".into(),
                        due_at_ms_field: has_due_time.then(|| "due_at_ms".into()),
                        evidence_field: None,
                        time_phrase_field: None,
                    },
                    undo_adapter: Some(WorkflowAdapter::SystemActionUndo),
                },
            },
            WorkflowTransition {
                id: "ignore".into(),
                description: format!("The message does not request a {name} item"),
                destination_state_id: "done".into(),
                output_fields: vec![],
                action: WorkflowAction::None,
            },
        ],
    };
    let states = vec![
        WorkflowState {
            id: "message_ingested".into(),
            description: "The voice message and transcript are available in extended state".into(),
            selector: WorkflowSelector::PhraseMatch {
                field: "transcript".into(),
                cases: vec![WorkflowPhraseCase {
                    phrases: phrases.iter().map(|phrase| (*phrase).into()).collect(),
                    transition_id: "candidate".into(),
                }],
                fallback_transition_id: "not_candidate".into(),
            },
            transitions: vec![
                WorkflowTransition {
                    id: "candidate".into(),
                    description: format!("The message may request a {name} item"),
                    destination_state_id: "interpret".into(),
                    output_fields: vec![],
                    action: WorkflowAction::None,
                },
                WorkflowTransition {
                    id: "not_candidate".into(),
                    description: format!("The message has no {name} trigger"),
                    destination_state_id: "done".into(),
                    output_fields: vec![],
                    action: WorkflowAction::None,
                },
            ],
        },
        interpret,
        terminal_state(),
    ];

    WorkflowDefinition {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        enabled: true,
        subscriptions: vec!["transcript_completed".into()],
        initial_state_id: "message_ingested".into(),
        states,
    }
}

fn default_mac_actions_workflow() -> WorkflowDefinition {
    WorkflowDefinition {
        id: "mac-actions".into(),
        name: "Mac Actions".into(),
        description: "Routes every distinct evidence-backed voice intent through independent Reminders, Calendar, and Todos slots without cross-triggering one clause.".into(),
        enabled: true,
        subscriptions: vec!["transcript_completed".into()],
        initial_state_id: "message_ingested".into(),
        states: vec![
            WorkflowState {
                id: "message_ingested".into(),
                description: "Keep automatic routing bounded for personal voice notes".into(),
                selector: WorkflowSelector::TextLength {
                    field: "transcript".into(),
                    maximum_chars: 4_000,
                    within_transition_id: "within_limit".into(),
                    overflow_transition_id: "too_long".into(),
                },
                transitions: vec![
                    WorkflowTransition {
                        id: "within_limit".into(),
                        description: "The voice note is small enough for automatic action routing"
                            .into(),
                        destination_state_id: "intent_gate".into(),
                        output_fields: vec![],
                        action: WorkflowAction::None,
                    },
                    WorkflowTransition {
                        id: "too_long".into(),
                        description: "Long conversational recordings require a manual action check"
                            .into(),
                        destination_state_id: "done".into(),
                        output_fields: vec![],
                        action: WorkflowAction::None,
                    },
                ],
            },
            WorkflowState {
                id: "intent_gate".into(),
                description: "Reject explicit opt-outs, then detect plausible action language"
                    .into(),
                selector: WorkflowSelector::PhraseMatch {
                    field: "transcript".into(),
                    cases: vec![
                        WorkflowPhraseCase {
                            phrases: [
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
                            .collect(),
                            transition_id: "opt_out".into(),
                        },
                        WorkflowPhraseCase {
                            phrases: [
                                "remind me",
                                "remember to",
                                "don't forget",
                                "add to my todo",
                                "add to my to do",
                                "put on my todo",
                                "put on my to do",
                                "add to my calendar",
                                "calendar add",
                                "schedule",
                                "calendar event",
                                "need to",
                                "i need to",
                                "i have to",
                                "i should",
                                "i must",
                                "i'll",
                                "i will",
                                "i am going to",
                                "i'm going to",
                                "meeting",
                                "appointment",
                                "booked for",
                            ]
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                            transition_id: "candidate".into(),
                        },
                    ],
                    fallback_transition_id: "not_candidate".into(),
                },
                transitions: vec![
                    WorkflowTransition {
                        id: "opt_out".into(),
                        description: "The speaker explicitly asked the app not to run hooks".into(),
                        destination_state_id: "done".into(),
                        output_fields: vec![],
                        action: WorkflowAction::None,
                    },
                    WorkflowTransition {
                        id: "candidate".into(),
                        description: "The transcript contains plausible action language".into(),
                        destination_state_id: "route_reminder".into(),
                        output_fields: vec![],
                        action: WorkflowAction::None,
                    },
                    WorkflowTransition {
                        id: "not_candidate".into(),
                        description: "No plausible action language was found".into(),
                        destination_state_id: "done".into(),
                        output_fields: vec![],
                        action: WorkflowAction::None,
                    },
                ],
            },
            action_route_state(
                "route_reminder",
                "reminder",
                "Find one genuine request to remember something. Do not treat a task, calendar event, feeling, observation, example, test, hypothetical or quoted speech, completed work, immediate act already underway, or another person's intent as a reminder.",
                "validate_reminder",
                "route_calendar",
                true,
                false,
            ),
            action_evidence_state(
                "validate_reminder",
                "reminder",
                &[],
                "route_calendar",
                ActionKind::Reminder,
                true,
            ),
            action_route_state(
                "route_calendar",
                "calendar",
                "Find one genuine future event the speaker wants scheduled or records that they intend to attend. Require a spoken time. Do not reuse reminder_accepted_evidence. Ignore feelings, observations, examples, tests, hypothetical or quoted speech, completed events, and another person's plans.",
                "validate_calendar",
                "route_todo",
                true,
                true,
            ),
            action_evidence_state(
                "validate_calendar",
                "calendar",
                &["reminder_accepted_evidence"],
                "route_todo",
                ActionKind::CalendarEvent,
                true,
            ),
            action_route_state(
                "route_todo",
                "todo",
                "Find one genuine unfinished personal task or commitment. It must have evidence distinct from reminder_accepted_evidence and calendar_accepted_evidence. Do not turn feelings, observations, examples, tests, hypothetical or quoted speech, completed work, an immediate act already underway, or another person's intent into a task.",
                "validate_todo",
                "done",
                false,
                false,
            ),
            action_evidence_state(
                "validate_todo",
                "todo",
                &["reminder_accepted_evidence", "calendar_accepted_evidence"],
                "done",
                ActionKind::Todo,
                false,
            ),
            terminal_state(),
        ],
    }
}

fn action_route_state(
    state_id: &str,
    prefix: &str,
    instruction: &str,
    validation_state: &str,
    skip_destination: &str,
    supports_due_time: bool,
    requires_due_time: bool,
) -> WorkflowState {
    let mut output_fields = vec![
        natural_language_field(
            &format!("{prefix}_title"),
            "Concise user-facing action text",
            240,
            true,
        ),
        natural_language_field(
            &format!("{prefix}_evidence"),
            "Exact contiguous transcript quote proving this distinct intent",
            500,
            true,
        ),
    ];
    if supports_due_time {
        output_fields.push(WorkflowOutputField {
            name: format!("{prefix}_due_at_ms"),
            description:
                "Future Unix timestamp in milliseconds resolved from the quoted time phrase".into(),
            kind: WorkflowOutputKind::Integer {
                minimum: 1_577_836_800_000,
                maximum: 4_102_444_800_000,
            },
            required: requires_due_time,
        });
        output_fields.push(natural_language_field(
            &format!("{prefix}_time_phrase"),
            "Exact time expression quoted inside the evidence",
            100,
            requires_due_time,
        ));
    }
    WorkflowState {
        id: state_id.into(),
        description: format!("Model or human slot for the {prefix} hook"),
        selector: WorkflowSelector::LocalModel {
            instruction: format!(
                "{instruction} Select propose_{prefix} only for one matching intent not already consumed by an accepted earlier hook; otherwise select skip_{prefix}. For a proposal, copy {prefix}_evidence exactly and contiguously from transcript. You may rewrite {prefix}_title concisely. If supplying a due time, copy {prefix}_time_phrase exactly from that evidence and resolve {prefix}_due_at_ms relative to recorded_at_ms. Never use recorded_at_ms itself as a due time."
            ),
        },
        transitions: vec![
            WorkflowTransition {
                id: format!("propose_{prefix}"),
                description: format!("Propose one evidence-backed {prefix}"),
                destination_state_id: validation_state.into(),
                output_fields,
                action: WorkflowAction::None,
            },
            WorkflowTransition {
                id: format!("skip_{prefix}"),
                description: format!("No distinct {prefix} intent is present"),
                destination_state_id: skip_destination.into(),
                output_fields: vec![],
                action: WorkflowAction::None,
            },
        ],
    }
}

fn action_evidence_state(
    state_id: &str,
    prefix: &str,
    prior_evidence_fields: &[&str],
    next_state: &str,
    kind: ActionKind,
    supports_due_time: bool,
) -> WorkflowState {
    let accepted_field = format!("{prefix}_accepted_evidence");
    WorkflowState {
        id: state_id.into(),
        description: format!("Deterministically ground and deduplicate the {prefix} proposal"),
        selector: WorkflowSelector::ActionEvidence {
            transcript_field: "transcript".into(),
            evidence_field: format!("{prefix}_evidence"),
            prior_evidence_fields: prior_evidence_fields
                .iter()
                .map(|field| (*field).into())
                .collect(),
            due_at_ms_field: supports_due_time.then(|| format!("{prefix}_due_at_ms")),
            time_phrase_field: supports_due_time.then(|| format!("{prefix}_time_phrase")),
            accepted_evidence_field: Some(accepted_field.clone()),
            valid_transition_id: format!("deliver_{prefix}"),
            invalid_transition_id: format!("reject_{prefix}"),
        },
        transitions: vec![
            WorkflowTransition {
                id: format!("deliver_{prefix}"),
                description: format!("Deliver the grounded, distinct {prefix}"),
                destination_state_id: next_state.into(),
                output_fields: vec![natural_language_field(
                    &accepted_field,
                    "Evidence reserved by this accepted hook",
                    500,
                    true,
                )],
                action: WorkflowAction::Effect {
                    adapter: WorkflowAdapter::SystemAction {
                        kind,
                        title_field: format!("{prefix}_title"),
                        due_at_ms_field: supports_due_time.then(|| format!("{prefix}_due_at_ms")),
                        evidence_field: Some(format!("{prefix}_evidence")),
                        time_phrase_field: supports_due_time
                            .then(|| format!("{prefix}_time_phrase")),
                    },
                    undo_adapter: Some(WorkflowAdapter::SystemActionUndo),
                },
            },
            WorkflowTransition {
                id: format!("reject_{prefix}"),
                description: format!("Reject ungrounded or duplicate {prefix} evidence"),
                destination_state_id: next_state.into(),
                output_fields: vec![],
                action: WorkflowAction::None,
            },
        ],
    }
}

fn natural_language_field(
    name: &str,
    description: &str,
    max_length: u32,
    required: bool,
) -> WorkflowOutputField {
    WorkflowOutputField {
        name: name.into(),
        description: description.into(),
        kind: WorkflowOutputKind::NaturalLanguage { max_length },
        required,
    }
}

fn terminal_state() -> WorkflowState {
    WorkflowState {
        id: "done".into(),
        description: "The decision and any side effect are complete".into(),
        selector: WorkflowSelector::Terminal {
            outcome: "complete".into(),
        },
        transitions: vec![],
    }
}

fn disable_migrated_legacy_rules(state_root: &Path) -> Result<()> {
    let path = state_root.join("rules.json");
    let Some(mut root) = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return Ok(());
    };
    if let Some(rules) = root.get_mut("rules").and_then(Value::as_array_mut) {
        for rule in rules {
            if matches!(
                rule.get("id").and_then(Value::as_str),
                Some("reminders" | "todos" | "calendar")
            ) && let Some(rule) = rule.as_object_mut()
            {
                rule.insert("enabled".into(), json!(false));
                rule.insert("migrated_to_workflow".into(), json!(true));
            }
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&root)?)?;
        fs::rename(temporary, path)?;
    }
    Ok(())
}

fn save_workflow(
    state_root: &Path,
    workflow: &personal_audio_lake_contracts::WorkflowDefinition,
) -> Result<()> {
    let directory = state_root.join("services");
    fs::create_dir_all(&directory)?;
    let target = directory.join(format!("{}.json", workflow.id));
    let temporary = target.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(workflow)?)?;
    fs::rename(temporary, target)?;
    Ok(())
}

fn delete_workflow(state_root: &Path, id: &str) -> Result<()> {
    let path = state_root.join("services").join(format!("{id}.json"));
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn save_automation(state_root: &Path, automation: &Automation) -> Result<()> {
    let path = state_root.join("rules.json");
    let mut root = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or_else(|| json!({"rules":[]}));
    let rules = root
        .get_mut("rules")
        .and_then(Value::as_array_mut)
        .context("rules.json must contain a rules array")?;
    let phrases = match automation.mode {
        RoutingMode::Implicit => Vec::new(),
        RoutingMode::Explicit | RoutingMode::Hybrid => automation
            .trigger
            .split('/')
            .map(str::trim)
            .filter(|phrase| !phrase.is_empty())
            .map(str::to_owned)
            .collect(),
    };
    let mode = match automation.mode {
        RoutingMode::Explicit => "explicit",
        RoutingMode::Hybrid => "hybrid",
        RoutingMode::Implicit => "implicit",
    };
    if let Some(existing) = rules
        .iter_mut()
        .find(|rule| rule.get("id").and_then(Value::as_str) == Some(&automation.id))
    {
        let existing = existing
            .as_object_mut()
            .context("an automation rule must be a JSON object")?;
        existing.insert("enabled".into(), json!(automation.enabled));
        existing.insert("mode".into(), json!(mode));
        existing.insert("phrases".into(), json!(phrases));
        existing.insert("action".into(), json!(automation.destination));
        existing.insert("approval".into(), json!("auto_when_unambiguous"));
    } else {
        rules.push(json!({
            "id": automation.id,
            "enabled": automation.enabled,
            "mode": mode,
            "phrases": phrases,
            "action": automation.destination,
            "approval": "auto_when_unambiguous",
            "threshold": if automation.mode == RoutingMode::Implicit { 0.9 } else { 0.78 }
        }));
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&root)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn request_permission(state_root: &Path, permission: Permission) -> Result<()> {
    let commands = state_root.join("ui-commands");
    fs::create_dir_all(&commands)?;
    let name = match permission {
        Permission::Notifications => "request-notification-access",
        Permission::Calendar => "request-calendar-access",
        Permission::Reminders => "request-reminders-access",
    };
    fs::write(
        commands.join(name),
        Utc::now().timestamp_millis().to_string(),
    )?;
    Ok(())
}

fn humanize(value: &str) -> String {
    value
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_separate_always_on_transfer_from_optional_device_selection() {
        let root = tempfile::tempdir().unwrap();
        let config = Config {
            state_root: root.path().to_owned(),
            lake_root: root.path().join("lake"),
            ..Config::default()
        };
        let snapshot = read_model::AppSnapshot {
            workers: vec![read_model::AppWorkerHealth {
                role: "transfer_ingress".into(),
                state: personal_audio_lake_contracts::HealthState::Ready,
                detail: "local transfer ready".into(),
            }],
            ..Default::default()
        };

        let settings = load_settings(&config, &snapshot);
        assert_eq!(
            settings.recording_transfer_state,
            Some(personal_audio_lake_contracts::HealthState::Ready)
        );
        assert_eq!(settings.recording_transfer_detail, "local transfer ready");
        assert!(!settings.source_adapter_configured);
        assert!(!settings.source_options.is_empty());
        assert!(
            settings
                .source_options
                .iter()
                .all(|option| !option.selected)
        );
    }

    #[test]
    fn automation_loader_preserves_mode_trigger_and_destination() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("rules.json"),
            serde_json::to_vec(&json!({"rules":[{
                "id":"calendar", "enabled":true, "mode":"hybrid",
                "phrases":["schedule", "calendar add"], "action":"macos.calendar"
            }]}))
            .unwrap(),
        )
        .unwrap();
        let loaded = load_automations(temp.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].mode, RoutingMode::Hybrid);
        assert_eq!(loaded[0].trigger, "schedule / calendar add");
        assert_eq!(loaded[0].destination, "macos.calendar");
    }

    #[test]
    fn permission_mapping_is_user_facing_and_total() {
        assert_eq!(permission_state("authorized"), PermissionState::Allowed);
        assert_eq!(
            permission_state("not_determined"),
            PermissionState::NotRequested
        );
        assert_eq!(permission_state("future-value"), PermissionState::Unknown);
    }

    #[test]
    fn latest_source_status_uses_the_generic_versioned_source_fact() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("status")).unwrap();
        let first = Envelope::root(Message::SourceStatusChanged(SourceStatus {
            adapter_id: "folder".into(),
            source_id: "inbox-a".into(),
            display_name: "Folder inbox".into(),
            state: SourceState::Ready,
            capabilities: Vec::new(),
            attributes: BTreeMap::new(),
        }));
        let second = Envelope::root(Message::SourceStatusChanged(SourceStatus {
            adapter_id: "network".into(),
            source_id: "remote-b".into(),
            display_name: "Remote recorder".into(),
            state: SourceState::Syncing,
            capabilities: Vec::new(),
            attributes: BTreeMap::new(),
        }));
        fs::write(
            temp.path().join("status/events.ndjson"),
            [first, second]
                .into_iter()
                .map(|event| json!({"worker":"source", "event":event}).to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let status = latest_source_status(temp.path(), Some("inbox-a")).unwrap();
        assert_eq!(status.display_name, "Folder inbox");
        assert_eq!(status.state, SourceState::Ready);
        let latest = latest_source_status(temp.path(), None).unwrap();
        assert_eq!(latest.display_name, "Remote recorder");
        assert_eq!(latest.state, SourceState::Syncing);
    }

    #[test]
    fn saving_an_automation_enforces_ask_forgiveness_policy() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rules.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({"rules":[{
                "id":"reminders", "enabled":true, "mode":"explicit",
                "phrases":["remind me to"], "action":"macos.reminder",
                "approval":"always_review", "threshold":0.78
            }]}))
            .unwrap(),
        )
        .unwrap();
        save_automation(
            temp.path(),
            &Automation {
                id: "reminders".into(),
                name: "Reminders".into(),
                mode: RoutingMode::Explicit,
                trigger: "remind me to".into(),
                destination: "macos.reminder".into(),
                enabled: false,
            },
        )
        .unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            saved.pointer("/rules/0/approval").and_then(Value::as_str),
            Some("auto_when_unambiguous")
        );
        assert_eq!(
            saved.pointer("/rules/0/threshold").and_then(Value::as_f64),
            Some(0.78)
        );
    }

    #[test]
    fn workflow_round_trips_the_versioned_state_machine_contract() {
        let temp = tempfile::tempdir().unwrap();
        let workflow = personal_audio_lake_contracts::WorkflowDefinition {
            id: "daily-note".into(),
            name: "Daily note".into(),
            description: "Append relevant transcripts".into(),
            enabled: true,
            subscriptions: vec!["transcript_completed".into()],
            initial_state_id: "message_ingested".into(),
            states: vec![personal_audio_lake_contracts::WorkflowState {
                id: "message_ingested".into(),
                description: "New transcript".into(),
                selector: personal_audio_lake_contracts::WorkflowSelector::Terminal {
                    outcome: "complete".into(),
                },
                transitions: vec![],
            }],
        };
        save_workflow(temp.path(), &workflow).unwrap();
        assert_eq!(load_workflows(temp.path()), vec![workflow.clone()]);
        delete_workflow(temp.path(), &workflow.id).unwrap();
        assert!(load_workflows(temp.path()).is_empty());
    }

    #[test]
    fn default_mac_actions_supports_distinct_multi_hook_results_with_code_between_slots() {
        let workflow = default_mac_actions_workflow();
        assert_eq!(
            workflow
                .states
                .iter()
                .filter(|state| matches!(state.selector, WorkflowSelector::LocalModel { .. }))
                .count(),
            3
        );
        assert_eq!(
            workflow
                .states
                .iter()
                .filter(|state| matches!(state.selector, WorkflowSelector::ActionEvidence { .. }))
                .count(),
            3
        );
        let effects = workflow
            .states
            .iter()
            .flat_map(|state| &state.transitions)
            .filter(|transition| matches!(transition.action, WorkflowAction::Effect { .. }))
            .collect::<Vec<_>>();
        assert_eq!(effects.len(), 3);
        for transition in effects {
            if let WorkflowAction::Effect {
                adapter,
                undo_adapter,
            } = &transition.action
            {
                assert!(matches!(
                    adapter,
                    WorkflowAdapter::SystemAction {
                        evidence_field: Some(_),
                        ..
                    }
                ));
                assert!(matches!(
                    undo_adapter,
                    Some(WorkflowAdapter::SystemActionUndo)
                ));
            }
        }
        let todo_validation = workflow
            .states
            .iter()
            .find(|state| state.id == "validate_todo")
            .unwrap();
        assert!(matches!(
            &todo_validation.selector,
            WorkflowSelector::ActionEvidence { prior_evidence_fields, .. }
                if prior_evidence_fields == &["reminder_accepted_evidence", "calendar_accepted_evidence"]
        ));
    }

    #[test]
    fn v5_seed_replaces_competing_defaults_and_preserves_user_edits() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let lake = temp.path().join("lake");
        let mut old = v4_default_mac_workflows();
        old[0].description = "My edited reminder workflow".into();
        for workflow in &old {
            save_workflow(&state, workflow).unwrap();
        }
        fs::write(state.join("workflow-seed-v4"), "seeded").unwrap();

        seed_default_workflows(&state, &lake).unwrap();

        let loaded = load_workflows(&state);
        let reminders = loaded
            .iter()
            .find(|workflow| workflow.id == "reminders")
            .unwrap();
        assert_eq!(reminders.description, "My edited reminder workflow");
        assert!(!loaded.iter().any(|workflow| workflow.id == "todos"));
        assert!(!loaded.iter().any(|workflow| workflow.id == "calendar"));
        let router = loaded
            .iter()
            .find(|workflow| workflow.id == "mac-actions")
            .unwrap();
        assert_eq!(router.states.len(), 9);
        assert!(matches!(
            router.states[0].selector,
            WorkflowSelector::TextLength { .. }
        ));
        assert!(matches!(
            router.states[1].selector,
            WorkflowSelector::PhraseMatch { .. }
        ));
        assert_eq!(
            router
                .states
                .iter()
                .filter(|state| matches!(state.selector, WorkflowSelector::LocalModel { .. }))
                .count(),
            3
        );
        assert!(state.join("workflow-seed-v5").exists());
    }
}
