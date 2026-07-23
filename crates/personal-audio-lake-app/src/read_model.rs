use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use personal_audio_lake_contracts::{HealthState, ports::ReadModelClient};
use reqwest::blocking::Client;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppSnapshot {
    pub recordings: Vec<AppRecording>,
    pub failures: Vec<AppFailure>,
    pub workers: Vec<AppWorkerHealth>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppWorkerHealth {
    pub role: String,
    pub state: HealthState,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRecording {
    pub recording_id: Uuid,
    /// User-authored presentation metadata. This is loaded from a separate annotation overlay;
    /// the lake's raw recording and transcript remain immutable.
    pub title: Option<String>,
    pub recorded_at_ms: i64,
    pub relative_audio_path: Option<String>,
    pub transcript_run_id: Option<Uuid>,
    pub summary: String,
    pub transcript: String,
    pub segments: Vec<AppSegment>,
    pub review_outcome: Option<String>,
    pub actions: Vec<AppAction>,
    pub workflow_runs: Vec<AppWorkflowRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppWorkflowRun {
    pub run_id: Uuid,
    pub workflow_id: String,
    pub requested_by: String,
    pub status: String,
    pub confirmed: bool,
    pub steps: Vec<AppWorkflowStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppWorkflowStep {
    pub state_id: String,
    pub available_transition_ids: Vec<String>,
    pub transition_id: Option<String>,
    pub outputs: BTreeMap<String, String>,
    pub selected_by: String,
    pub destination_state_id: Option<String>,
    pub receipt: Option<String>,
    pub undoable: bool,
    pub undone: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSegment {
    pub transcript_run_id: Uuid,
    pub ordinal: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub raw_text: String,
    pub replacement_text: Option<String>,
}

impl AppSegment {
    pub fn displayed_text(&self) -> &str {
        self.replacement_text.as_deref().unwrap_or(&self.raw_text)
    }
}

impl AppRecording {
    pub fn needs_review(&self) -> bool {
        self.review_outcome
            .as_deref()
            .is_none_or(|outcome| outcome.is_empty() || outcome == "needs_attention")
    }

    #[cfg(test)]
    pub fn action_receipt(&self) -> String {
        if self.actions.is_empty() {
            return "No actions triggered".into();
        }
        self.actions
            .iter()
            .map(AppAction::receipt)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppAction {
    pub action_id: Uuid,
    pub kind: String,
    pub title: String,
    pub due_at_ms: Option<i64>,
    pub evidence_quote: Option<String>,
    pub decision: String,
    pub delivered_target: Option<String>,
}

impl AppAction {
    #[cfg(test)]
    fn receipt(&self) -> String {
        match &self.delivered_target {
            Some(target) => format!("{} · {} · delivered to {target}", self.kind, self.title),
            None => format!("{} · {} · {}", self.kind, self.title, self.decision),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFailure {
    pub recording_id: Option<Uuid>,
    pub detail: String,
}

/// Replaceable client leaf for the native application's versioned local HTTP API. The view model
/// and controller never depend on reqwest; coupling to HTTP and token storage stays in this module.
pub struct LocalApiReadModelClient {
    api_bind: String,
    state_root: PathBuf,
}

impl LocalApiReadModelClient {
    fn new(api_bind: &str, state_root: &Path) -> Self {
        Self {
            api_bind: api_bind.to_owned(),
            state_root: state_root.to_owned(),
        }
    }
}

impl ReadModelClient<(), AppSnapshot> for LocalApiReadModelClient {
    type Error = anyhow::Error;

    fn query(&mut self, (): ()) -> Result<AppSnapshot> {
        fetch_snapshot_http(&self.api_bind, &self.state_root)
    }
}

impl ReadModelClient<Uuid, Vec<AppSegment>> for LocalApiReadModelClient {
    type Error = anyhow::Error;

    fn query(&mut self, recording_id: Uuid) -> Result<Vec<AppSegment>> {
        fetch_segments_http(&self.api_bind, &self.state_root, recording_id)
    }
}

pub fn fetch(api_bind: &str, state_root: &Path) -> Result<AppSnapshot> {
    LocalApiReadModelClient::new(api_bind, state_root).query(())
}

fn fetch_snapshot_http(api_bind: &str, state_root: &Path) -> Result<AppSnapshot> {
    let client = client(state_root)?;
    let token = token(state_root)?;
    let response = client
        .get(format!("http://{api_bind}/api/v1/inbox"))
        .bearer_auth(token.trim())
        .send()?
        .error_for_status()?
        .json::<Value>()?;
    let mut snapshot = parse(&response)?;
    let health = client
        .get(format!("http://{api_bind}/api/v1/health"))
        .bearer_auth(token.trim())
        .send()?
        .error_for_status()?
        .json::<Value>()?;
    snapshot.workers = parse_worker_health(&health);
    Ok(snapshot)
}

fn parse_worker_health(value: &Value) -> Vec<AppWorkerHealth> {
    value
        .get("workers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|worker| {
            let state = match worker.get("state").and_then(Value::as_str)? {
                "starting" => HealthState::Starting,
                "ready" => HealthState::Ready,
                "degraded" => HealthState::Degraded,
                "stopped" => HealthState::Stopped,
                _ => return None,
            };
            Some(AppWorkerHealth {
                role: string(worker.get("role")),
                state,
                detail: string(worker.get("detail")),
            })
        })
        .collect()
}

pub fn fetch_segments(
    api_bind: &str,
    state_root: &Path,
    recording_id: Uuid,
) -> Result<Vec<AppSegment>> {
    LocalApiReadModelClient::new(api_bind, state_root).query(recording_id)
}

fn fetch_segments_http(
    api_bind: &str,
    state_root: &Path,
    recording_id: Uuid,
) -> Result<Vec<AppSegment>> {
    let response = client(state_root)?
        .get(format!(
            "http://{api_bind}/api/v1/recordings/{recording_id}"
        ))
        .bearer_auth(token(state_root)?.trim())
        .send()?
        .error_for_status()?
        .json::<Value>()?;
    Ok(response
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_segment)
        .collect())
}

fn client(_state_root: &Path) -> Result<Client> {
    Ok(Client::builder().no_proxy().build()?)
}

fn token(state_root: &Path) -> Result<String> {
    fs::read_to_string(state_root.join("api-token")).context("read local API token")
}

#[cfg(test)]
pub fn fixture_snapshot() -> AppSnapshot {
    let first = Uuid::new_v5(
        &personal_audio_lake_contracts::RECORDING_NAMESPACE,
        b"test-recording-one",
    );
    let second = Uuid::new_v5(
        &personal_audio_lake_contracts::RECORDING_NAMESPACE,
        b"test-recording-two",
    );
    AppSnapshot {
        recordings: vec![
            AppRecording {
                recording_id: first,
                title: None,
                recorded_at_ms: 1_700_000_000_000,
                relative_audio_path: Some("objects/audio/test-one.m4a".into()),
                transcript_run_id: Some(Uuid::new_v5(
                    &personal_audio_lake_contracts::RECORDING_NAMESPACE,
                    b"test-recording-one-transcript",
                )),
                summary: "Synthetic recording with a delivered reminder".into(),
                transcript: "Synthetic transcript for the first view-model test recording.".into(),
                segments: vec![AppSegment {
                    transcript_run_id: Uuid::new_v5(
                        &personal_audio_lake_contracts::RECORDING_NAMESPACE,
                        b"test-recording-one-transcript",
                    ),
                    ordinal: 0,
                    start_ms: 0,
                    end_ms: 4_200,
                    raw_text: "Synthetic transcript for the first view-model test recording."
                        .into(),
                    replacement_text: None,
                }],
                review_outcome: None,
                actions: vec![AppAction {
                    action_id: Uuid::new_v5(
                        &personal_audio_lake_contracts::RECORDING_NAMESPACE,
                        b"test-recording-one-action",
                    ),
                    kind: "Reminder".into(),
                    title: "Synthetic reminder".into(),
                    due_at_ms: Some(1_700_010_000_000),
                    evidence_quote: Some("Synthetic transcript evidence".into()),
                    decision: "approved".into(),
                    delivered_target: Some("Reminders".into()),
                }],
                workflow_runs: vec![],
            },
            AppRecording {
                recording_id: second,
                title: None,
                recorded_at_ms: 1_699_999_000_000,
                relative_audio_path: Some("objects/audio/test-two.m4a".into()),
                transcript_run_id: Some(Uuid::new_v5(
                    &personal_audio_lake_contracts::RECORDING_NAMESPACE,
                    b"test-recording-two-transcript",
                )),
                summary: "Synthetic second recording without an action".into(),
                transcript: "Synthetic transcript for the second view-model test recording.".into(),
                segments: vec![AppSegment {
                    transcript_run_id: Uuid::new_v5(
                        &personal_audio_lake_contracts::RECORDING_NAMESPACE,
                        b"test-recording-two-transcript",
                    ),
                    ordinal: 0,
                    start_ms: 0,
                    end_ms: 3_800,
                    raw_text: "Synthetic transcript for the second view-model test recording."
                        .into(),
                    replacement_text: None,
                }],
                review_outcome: None,
                actions: vec![],
                workflow_runs: vec![],
            },
        ],
        failures: vec![],
        workers: vec![AppWorkerHealth {
            role: "transfer_ingress".into(),
            state: HealthState::Ready,
            detail: "encrypted local recording transfer ready".into(),
        }],
    }
}

fn parse(value: &Value) -> Result<AppSnapshot> {
    let actions = value
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_action)
        .collect::<Vec<_>>();
    let workflow_runs = value
        .get("workflow_runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_workflow_run)
        .collect::<Vec<_>>();
    let recordings = value
        .get("recordings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|recording| {
            let recording_id = uuid(recording.get("recording_id")?)?;
            Some(AppRecording {
                recording_id,
                title: None,
                recorded_at_ms: integer(recording.get("recorded_at_ms")).unwrap_or_default(),
                relative_audio_path: nonempty(recording.get("relative_audio_path")),
                transcript_run_id: recording.get("transcript_run_id").and_then(uuid),
                summary: string(recording.get("summary")),
                transcript: string(recording.get("transcript")),
                segments: Vec::new(),
                review_outcome: nonempty(recording.get("review_outcome")),
                actions: actions
                    .iter()
                    .filter(|(owner, _)| *owner == recording_id)
                    .map(|(_, action)| action.clone())
                    .collect(),
                workflow_runs: workflow_runs
                    .iter()
                    .filter(|(owner, _)| *owner == recording_id)
                    .map(|(_, run)| run.clone())
                    .collect(),
            })
        })
        .collect();
    let failures = value
        .get("failures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|failure| AppFailure {
            recording_id: failure.get("recording_id").and_then(uuid),
            detail: user_failure_detail(&string(failure.get("detail"))),
        })
        .collect();
    Ok(AppSnapshot {
        recordings,
        failures,
        workers: Vec::new(),
    })
}

fn user_failure_detail(detail: &str) -> String {
    if detail.contains("EventKit access is")
        || detail.contains("personal-audio-lake-macos-actions request-access")
    {
        "Calendar or Reminders is unavailable. Nothing was added; the recording remains in Review."
            .into()
    } else {
        detail.into()
    }
}

fn parse_workflow_run(value: &Value) -> Option<(Uuid, AppWorkflowRun)> {
    let recording_id = uuid(value.get("recording_id")?)?;
    let undone = value
        .get("undone_transitions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.get("transition_id").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    let steps = value
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|step| {
            let transition_id = nonempty(step.pointer("/selection/transition_id"));
            AppWorkflowStep {
                undone: transition_id
                    .as_deref()
                    .is_some_and(|id| undone.contains(id)),
                state_id: string(step.get("state_id")),
                available_transition_ids: step
                    .get("available_transition_ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                transition_id,
                outputs: step
                    .pointer("/selection/outputs")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(|values| values.iter())
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.into()))
                    })
                    .collect(),
                selected_by: string(step.get("selected_by")),
                destination_state_id: nonempty(step.get("destination_state_id")),
                receipt: nonempty(step.get("receipt")),
                undoable: step
                    .get("undoable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }
        })
        .collect();
    Some((
        recording_id,
        AppWorkflowRun {
            run_id: uuid(value.get("run_id")?)?,
            workflow_id: string(value.get("workflow_id")),
            requested_by: string(value.get("requested_by")),
            status: string(value.get("status")),
            confirmed: value
                .get("confirmed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            steps,
        },
    ))
}

fn parse_segment(value: &Value) -> Option<AppSegment> {
    Some(AppSegment {
        transcript_run_id: uuid(value.get("run_id")?)?,
        ordinal: integer(value.get("ordinal"))?.try_into().ok()?,
        start_ms: integer(value.get("start_ms"))?.try_into().ok()?,
        end_ms: integer(value.get("end_ms"))?.try_into().ok()?,
        raw_text: string(value.get("raw_text")),
        replacement_text: nonempty(value.get("replacement_text")),
    })
}

fn parse_action(value: &Value) -> Option<(Uuid, AppAction)> {
    let recording_id = uuid(value.get("recording_id")?)?;
    Some((
        recording_id,
        AppAction {
            action_id: uuid(value.get("action_id")?)?,
            kind: friendly_kind(&string(value.get("kind"))).into(),
            title: string(value.get("title")),
            due_at_ms: integer(value.get("due_at_ms")).filter(|value| *value > 0),
            evidence_quote: value
                .get("evidence_json")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|evidence| {
                    evidence
                        .as_array()?
                        .iter()
                        .find_map(|item| nonempty(item.get("quote")))
                }),
            decision: friendly_decision(&string(value.get("decision"))).into(),
            delivered_target: nonempty(value.get("delivered_target")).map(friendly_target),
        },
    ))
}

fn uuid(value: &Value) -> Option<Uuid> {
    value.as_str().and_then(|value| Uuid::parse_str(value).ok())
}

fn integer(value: Option<&Value>) -> Option<i64> {
    value.and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })
}

fn string(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn nonempty(value: Option<&Value>) -> Option<String> {
    string(value)
        .trim()
        .to_owned()
        .then_some_if(|value| !value.is_empty())
}

fn friendly_kind(value: &str) -> &'static str {
    match value {
        "todo" | "todos" | "to-do" => "Todo",
        "reminder" => "Reminder",
        "calendar" | "calendar_event" | "calendarevent" | "event" => "Calendar",
        _ => "Action",
    }
}

fn friendly_decision(value: &str) -> &'static str {
    match value {
        "approved" => "approved",
        "rejected" => "not added",
        "removed" => "removed",
        "pending" | "" => "waiting for review",
        _ => "processing",
    }
}

fn friendly_target(value: String) -> String {
    match value.as_str() {
        "macos_reminders" => "Reminders".into(),
        "macos_calendar" => "Calendar".into(),
        _ => value,
    }
}

trait ThenSomeIf: Sized {
    fn then_some_if(self, predicate: impl FnOnce(&Self) -> bool) -> Option<Self> {
        predicate(&self).then_some(self)
    }
}

impl<T> ThenSomeIf for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_http_leaf_implements_both_native_read_model_ports() {
        fn accepts_snapshot<Client>(_: &Client)
        where
            Client: ReadModelClient<(), AppSnapshot, Error = anyhow::Error>,
        {
        }
        fn accepts_segments<Client>(_: &Client)
        where
            Client: ReadModelClient<Uuid, Vec<AppSegment>, Error = anyhow::Error>,
        {
        }

        let client = LocalApiReadModelClient::new("127.0.0.1:1", Path::new("/tmp/unused"));
        accepts_snapshot(&client);
        accepts_segments(&client);
    }

    #[test]
    fn eventkit_worker_errors_are_contained_without_settings_recovery() {
        let detail = user_failure_detail(
            "EventKit access is EKAuthorizationStatus(0); run `personal-audio-lake-macos-actions request-access` from the app bundle",
        );
        assert_eq!(
            detail,
            "Calendar or Reminders is unavailable. Nothing was added; the recording remains in Review."
        );
    }

    #[test]
    fn generic_worker_health_exposes_transfer_readiness_without_adapter_assumptions() {
        let workers = parse_worker_health(&serde_json::json!({
            "workers":[
                {"role":"transfer_ingress","state":"ready","detail":"local transfer ready"},
                {"role":"source","state":"degraded","detail":"optional source unavailable"},
                {"role":"future","state":"unknown","detail":"ignored"}
            ]
        }));
        assert_eq!(workers.len(), 2);
        assert_eq!(workers[0].role, "transfer_ingress");
        assert_eq!(workers[0].state, HealthState::Ready);
        assert_eq!(workers[0].detail, "local transfer ready");
        assert_eq!(workers[1].state, HealthState::Degraded);
    }

    #[test]
    fn api_projection_uses_user_language_and_explicit_no_action_receipt() {
        let id = Uuid::new_v4();
        let snapshot = parse(&serde_json::json!({
            "recordings":[{
                "recording_id":id,
                "recorded_at_ms":"1700000000000",
                "summary":"A test thought",
                "transcript":"Testing one two three",
                "review_outcome":""
            }],
            "actions":[],
            "failures":[]
        }))
        .unwrap();
        assert_eq!(snapshot.recordings.len(), 1);
        assert!(snapshot.recordings[0].needs_review());
        assert_eq!(
            snapshot.recordings[0].action_receipt(),
            "No actions triggered"
        );
    }

    #[test]
    fn action_delivery_is_projected_without_backend_target_names() {
        let id = Uuid::new_v4();
        let snapshot = parse(&serde_json::json!({
            "recordings":[{"recording_id":id,"summary":"Buy milk"}],
            "actions":[{
                "action_id":Uuid::new_v4(),
                "recording_id":id,
                "kind":"reminder",
                "title":"Buy milk",
                "evidence_json":"[{\"quote\":\"Remind me to buy milk\"}]",
                "decision":"approved",
                "delivered_target":"macos_reminders"
            }],
            "failures":[]
        }))
        .unwrap();
        assert_eq!(
            snapshot.recordings[0].action_receipt(),
            "Reminder · Buy milk · delivered to Reminders"
        );
        assert_eq!(
            snapshot.recordings[0].actions[0].evidence_quote.as_deref(),
            Some("Remind me to buy milk")
        );
    }

    #[test]
    fn compensated_action_is_presented_as_removed() {
        assert_eq!(friendly_decision("removed"), "removed");
    }

    #[test]
    fn action_kind_aliases_never_fall_back_to_an_ambiguous_integration() {
        assert_eq!(friendly_kind("todo"), "Todo");
        assert_eq!(friendly_kind("todos"), "Todo");
        assert_eq!(friendly_kind("calendar"), "Calendar");
        assert_eq!(friendly_kind("calendar_event"), "Calendar");
        assert_eq!(friendly_kind("calendarevent"), "Calendar");
    }
}
