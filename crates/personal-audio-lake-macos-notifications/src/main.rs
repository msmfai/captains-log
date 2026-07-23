use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use clap::Parser;
use personal_audio_lake_contracts::{
    ActionKind, DerivationCompleted, Envelope, Failure, HealthState, Message, NotificationDelivery,
    NotificationHook, TranscriptCompleted, WorkerHealth, WorkerRole, WorkflowDefinition,
    WorkflowRunRecord, is_required_local_model_size,
};
use reqwest::blocking::Client;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Parser)]
struct Cli {
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_STATE_ROOT")]
    state_root: Option<PathBuf>,
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_SEMANTIC_MODEL")]
    semantic_model: Option<String>,
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_OLLAMA_URL")]
    ollama_url: Option<String>,
    #[arg(long, default_value_t = 60_000)]
    delivery_timeout_ms: u64,
}

#[derive(Clone)]
struct TrackedAction {
    kind: ActionKind,
    title: String,
    auto_approved: bool,
    approved: bool,
    target: Option<String>,
}

struct Trace {
    parent: Envelope,
    recording_id: Uuid,
    transcript: String,
    actions: HashMap<Uuid, TrackedAction>,
    completed: bool,
    expected_workflows: HashSet<String>,
    finished_workflows: HashSet<String>,
    started_at: Instant,
}

impl Trace {
    fn new(
        parent: &Envelope,
        transcript: &TranscriptCompleted,
        expected_workflows: HashSet<String>,
    ) -> Self {
        Self {
            parent: parent.clone(),
            recording_id: transcript.recording_id,
            transcript: transcript
                .segments
                .iter()
                .map(|segment| segment.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            actions: HashMap::new(),
            completed: false,
            expected_workflows,
            finished_workflows: HashSet::new(),
            started_at: Instant::now(),
        }
    }

    fn finish(&mut self, completed: &DerivationCompleted) {
        for action in &completed.actions {
            self.actions
                .entry(action.action_id)
                .and_modify(|tracked| {
                    tracked.kind = action.kind;
                    tracked.title.clone_from(&action.title);
                    tracked.auto_approved = action.auto_approved;
                    tracked.approved |= action.auto_approved;
                })
                .or_insert_with(|| TrackedAction {
                    kind: action.kind,
                    title: action.title.clone(),
                    auto_approved: action.auto_approved,
                    approved: action.auto_approved,
                    target: None,
                });
        }
        self.completed = true;
    }

    fn finish_workflow(&mut self, record: &WorkflowRunRecord) {
        self.finished_workflows.insert(record.workflow_id.clone());
    }

    fn ready(&self) -> bool {
        self.completed
            && self.expected_workflows.is_subset(&self.finished_workflows)
            && self
                .actions
                .values()
                .all(|action| !action.approved || action.target.is_some())
    }

    fn expired(&self, timeout: Duration) -> bool {
        self.started_at.elapsed() >= timeout
    }

    fn hooks(&self) -> Vec<NotificationHook> {
        let mut hooks = self
            .actions
            .iter()
            .map(|(action_id, action)| NotificationHook {
                action_id: *action_id,
                kind: action.kind,
                title: action.title.clone(),
                status: if action.target.is_some() {
                    "delivered"
                } else if action.auto_approved || action.approved {
                    "delivery_unconfirmed"
                } else {
                    "awaiting_review"
                }
                .into(),
                target: action.target.clone(),
            })
            .collect::<Vec<_>>();
        hooks.sort_by_key(|hook| hook.action_id);
        hooks
    }
}

struct Notifier {
    model: String,
    ollama_url: String,
    model_verified: bool,
    http: Client,
}

#[derive(Deserialize)]
struct ModelSummary {
    summary: String,
}

#[derive(Deserialize)]
struct ModelConfig {
    #[serde(default = "default_model")]
    semantic_model: String,
    #[serde(default = "default_ollama_url")]
    ollama_url: String,
}

fn default_model() -> String {
    "mistral-small3.2:latest".into()
}

fn default_ollama_url() -> String {
    "http://127.0.0.1:11434".into()
}

impl Notifier {
    fn new(model: String, ollama_url: String) -> Self {
        Self {
            model,
            ollama_url,
            model_verified: false,
            http: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(600))
                .build()
                .expect("local HTTP client"),
        }
    }

    fn summarize(&mut self, transcript: &str) -> Result<String> {
        self.ensure_24b_model()?;
        let schema = serde_json::json!({
            "type":"object",
            "properties":{"summary":{"type":"string"}},
            "required":["summary"]
        });
        let prompt = format!(
            "Summarize this private voice note in one factual sentence of at most 140 characters. Do not describe integrations, actions, or hook outcomes; code adds those separately. Do not add facts. Output only the required JSON.\n\n{}",
            transcript.chars().take(8_000).collect::<String>()
        );
        let response = self
            .http
            .post(format!("{}/api/chat", self.ollama_url.trim_end_matches('/')))
            .json(&serde_json::json!({
                "model":self.model,
                "stream":false,
                "format":schema,
                "options":{"temperature":0,"seed":11},
                "messages":[
                    {"role":"system","content":"You summarize one private local voice note. Be brief and literal."},
                    {"role":"user","content":prompt}
                ]
            }))
            .send()
            .context("run local notification summarizer")?;
        let status = response.status();
        let body: serde_json::Value = response.json()?;
        if !status.is_success() {
            bail!("Ollama summarizer failed ({status}): {body}");
        }
        let content = body
            .pointer("/message/content")
            .and_then(serde_json::Value::as_str)
            .context("Ollama summary has no structured content")?;
        let summary: ModelSummary = serde_json::from_str(content)?;
        let summary = summary.summary.trim();
        if summary.is_empty() {
            bail!("Ollama returned an empty summary");
        }
        Ok(summary.chars().take(180).collect())
    }

    fn ensure_24b_model(&mut self) -> Result<()> {
        if self.model_verified {
            return Ok(());
        }
        let response = self
            .http
            .post(format!(
                "{}/api/show",
                self.ollama_url.trim_end_matches('/')
            ))
            .timeout(Duration::from_secs(10))
            .json(&serde_json::json!({"model":self.model}))
            .send()
            .context("contact local Ollama")?;
        let status = response.status();
        let body: serde_json::Value = response.json()?;
        if !status.is_success() {
            bail!("Ollama model inspection failed ({status}): {body}");
        }
        let parameters = body
            .pointer("/details/parameter_size")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !is_required_local_model_size(parameters) {
            bail!(
                "notification model {} is {parameters}, expected local 24B",
                self.model
            );
        }
        self.model_verified = true;
        Ok(())
    }

    fn deliver(&mut self, trace: Trace) -> Result<NotificationDelivery> {
        let summary = self
            .summarize(&trace.transcript)
            .unwrap_or_else(|_| fallback_summary(&trace.transcript));
        let hooks = trace.hooks();
        Ok(NotificationDelivery {
            recording_id: trace.recording_id,
            delivered_at_ms: Utc::now().timestamp_millis(),
            summary,
            hooks,
        })
    }
}

fn fallback_summary(transcript: &str) -> String {
    let value = transcript.trim();
    if value.is_empty() {
        "A recording was received and transcribed.".into()
    } else {
        value.chars().take(180).collect()
    }
}

fn apply(trace: &mut Trace, envelope: &Envelope) {
    match &envelope.message {
        Message::ActionProposed(action) => {
            trace
                .actions
                .entry(action.action_id)
                .or_insert(TrackedAction {
                    kind: action.kind,
                    title: action.title.clone(),
                    auto_approved: false,
                    approved: false,
                    target: None,
                });
        }
        Message::ActionApproved { action_id } => {
            if let Some(action) = trace.actions.get_mut(action_id) {
                action.approved = true;
            }
        }
        Message::ActionDelivered(delivery) => {
            if let Some(action) = trace.actions.get_mut(&delivery.action_id) {
                action.target = Some(delivery.target.clone());
            }
        }
        Message::DerivationCompleted(completed) => trace.finish(completed),
        Message::WorkflowRunRecorded(record) => trace.finish_workflow(record),
        _ => {}
    }
}

fn enabled_transcript_workflows(state_root: &std::path::Path) -> HashSet<String> {
    let Ok(entries) = fs::read_dir(state_root.join("services")) else {
        return HashSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value == "json")
        })
        .filter_map(|entry| fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<WorkflowDefinition>(&bytes).ok())
        .filter(|workflow| {
            workflow.enabled
                && (workflow.subscriptions.is_empty()
                    || workflow
                        .subscriptions
                        .iter()
                        .any(|event| event == "transcript_completed"))
        })
        .map(|workflow| workflow.id)
        .collect()
}

fn emit(envelope: &Envelope) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(envelope.to_ndjson()?.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let state_root = cli.state_root.clone().unwrap_or_else(default_state_root);
    let timeout = Duration::from_millis(cli.delivery_timeout_ms);
    let configured = std::fs::read(state_root.join("rules.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ModelConfig>(&bytes).ok())
        .unwrap_or_else(|| ModelConfig {
            semantic_model: default_model(),
            ollama_url: default_ollama_url(),
        });
    let mut notifier = Notifier::new(
        cli.semantic_model.unwrap_or(configured.semantic_model),
        cli.ollama_url.unwrap_or(configured.ollama_url),
    );
    let readiness = notifier.ensure_24b_model();
    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
        role: WorkerRole::Notify,
        state: if readiness.is_ok() {
            HealthState::Ready
        } else {
            HealthState::Degraded
        },
        detail: readiness.map_or_else(
            |error| format!("local 24B notification model unavailable: {error:#}"),
            |_| format!("local 24B notification model ready: {}", notifier.model),
        ),
        backlog: 0,
    })))?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(io::stdin()).lines().map_while(Result::ok) {
            if let Ok(envelope) = Envelope::from_json(&line)
                && tx.send(envelope).is_err()
            {
                break;
            }
        }
    });
    let mut traces: HashMap<Uuid, Trace> = HashMap::new();
    let mut action_owners = HashMap::<Uuid, Uuid>::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(envelope) => match &envelope.message {
                Message::TranscriptCompleted(transcript) => {
                    traces.insert(
                        transcript.recording_id,
                        Trace::new(
                            &envelope,
                            transcript,
                            enabled_transcript_workflows(&state_root),
                        ),
                    );
                }
                Message::ActionProposed(action) => {
                    action_owners.insert(action.action_id, action.recording_id);
                    if let Some(trace) = traces.get_mut(&action.recording_id) {
                        apply(trace, &envelope);
                    }
                }
                Message::ActionApproved { action_id }
                | Message::ActionDelivered(personal_audio_lake_contracts::ActionDelivery {
                    action_id,
                    ..
                }) => {
                    if let Some(recording_id) = action_owners.get(action_id)
                        && let Some(trace) = traces.get_mut(recording_id)
                    {
                        apply(trace, &envelope);
                    }
                }
                Message::DerivationCompleted(completed) => {
                    if let Some(trace) = traces.get_mut(&completed.recording_id) {
                        apply(trace, &envelope);
                    }
                }
                Message::WorkflowRunRecorded(record) => {
                    if let Some(trace) = traces.get_mut(&record.recording_id) {
                        apply(trace, &envelope);
                    }
                }
                _ => {}
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if traces.is_empty() => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
        let ready = traces
            .iter()
            .filter(|(_, trace)| trace.ready() || trace.expired(timeout))
            .map(|(recording_id, _)| *recording_id)
            .collect::<Vec<_>>();
        for recording_id in ready {
            let trace = traces
                .remove(&recording_id)
                .ok_or_else(|| anyhow!("ready trace disappeared"))?;
            let parent = trace.parent.clone();
            match notifier.deliver(trace) {
                Ok(delivery) => emit(&Envelope::caused_by(
                    &parent,
                    Message::NotificationDelivered(delivery),
                ))?,
                Err(error) => emit(&Envelope::caused_by(
                    &parent,
                    Message::Failure(Failure {
                        worker: WorkerRole::Notify,
                        code: "notification_failed".into(),
                        detail: format!("{error:#}"),
                        retryable: true,
                        failed_message_id: Some(parent.id),
                    }),
                ))?,
            }
        }
    }
    Ok(())
}

fn default_state_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Application Support/PersonalAudioLake")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_is_independently_supported() {
        personal_audio_lake_contracts::assert_contract_fixtures();
    }

    #[test]
    fn hook_outcomes_are_code_generated() {
        let action_id = Uuid::new_v4();
        let trace = Trace {
            parent: Envelope::root(Message::StopWorker {
                reason: "fixture".into(),
            }),
            recording_id: Uuid::new_v4(),
            transcript: "Test reminder payload.".into(),
            actions: HashMap::from([(
                action_id,
                TrackedAction {
                    kind: ActionKind::Reminder,
                    title: "fixture task".into(),
                    auto_approved: true,
                    approved: true,
                    target: Some("macos_reminders".into()),
                },
            )]),
            completed: true,
            expected_workflows: HashSet::new(),
            finished_workflows: HashSet::new(),
            started_at: Instant::now(),
        };
        assert_eq!(
            trace.hooks(),
            vec![NotificationHook {
                action_id,
                kind: ActionKind::Reminder,
                title: "fixture task".into(),
                status: "delivered".into(),
                target: Some("macos_reminders".into()),
            }]
        );
    }

    #[test]
    fn notification_waits_for_every_enabled_workflow_and_approved_delivery() {
        let action_id = Uuid::new_v4();
        let mut trace = Trace {
            parent: Envelope::root(Message::StopWorker {
                reason: "fixture".into(),
            }),
            recording_id: Uuid::new_v4(),
            transcript: "Test".into(),
            actions: HashMap::from([(
                action_id,
                TrackedAction {
                    kind: ActionKind::Reminder,
                    title: "Test".into(),
                    auto_approved: true,
                    approved: true,
                    target: None,
                },
            )]),
            completed: true,
            expected_workflows: HashSet::from(["reminders".into()]),
            finished_workflows: HashSet::new(),
            started_at: Instant::now(),
        };
        assert!(!trace.ready());
        trace.finished_workflows.insert("reminders".into());
        assert!(!trace.ready());
        trace.actions.get_mut(&action_id).unwrap().target = Some("macos_reminders".into());
        assert!(trace.ready());
    }
}
