//! Versioned, implementation-agnostic messages shared by independently built workers.

use std::collections::BTreeMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const CONTRACT_VERSION: u16 = 1;
pub const RECORDING_NAMESPACE: Uuid = Uuid::from_u128(0xa51d6b7f_6bc3_4f9d_a86e_31d43f45801a);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope {
    pub version: u16,
    pub id: Uuid,
    pub correlation_id: Uuid,
    pub causation_id: Option<Uuid>,
    pub emitted_at_ms: i64,
    #[serde(flatten)]
    pub message: Message,
}

impl Envelope {
    pub fn root(message: Message) -> Self {
        let id = Uuid::new_v4();
        Self {
            version: CONTRACT_VERSION,
            id,
            correlation_id: id,
            causation_id: None,
            emitted_at_ms: Utc::now().timestamp_millis(),
            message,
        }
    }

    pub fn caused_by(parent: &Self, message: Message) -> Self {
        Self {
            version: CONTRACT_VERSION,
            id: Uuid::new_v4(),
            correlation_id: parent.correlation_id,
            causation_id: Some(parent.id),
            emitted_at_ms: Utc::now().timestamp_millis(),
            message,
        }
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if self.version != CONTRACT_VERSION {
            return Err(ContractError::UnsupportedVersion(self.version));
        }
        if self.emitted_at_ms <= 0 {
            return Err(ContractError::InvalidTime);
        }
        self.message.validate()
    }

    pub fn to_ndjson(&self) -> Result<String, ContractError> {
        self.validate()?;
        let mut encoded = serde_json::to_string(self)?;
        encoded.push('\n');
        Ok(encoded)
    }

    pub fn from_json(line: &str) -> Result<Self, ContractError> {
        let mut value: serde_json::Value = serde_json::from_str(line)?;
        migrate_wire_value(&mut value);
        let envelope: Self = serde_json::from_value(value)?;
        envelope.validate()?;
        Ok(envelope)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Message {
    StartWorker {
        role: WorkerRole,
    },
    StopWorker {
        reason: String,
    },
    WorkerHealth(WorkerHealth),
    SourceAdapterSelectionRequested {
        adapter_id: String,
    },
    SourceCommandRequested(SourceCommand),
    SourceStatusChanged(SourceStatus),
    ReleaseSourceRequested,
    RecordingAvailable(RecordingAvailable),
    AudioIngested(AudioIngested),
    TranscriptionRequested(TranscriptionRequest),
    TranscriptCompleted(TranscriptCompleted),
    DerivationRequested(DerivationRequest),
    ActionProposed(ActionProposal),
    ActionRevisionRequested(ActionRevisionRequest),
    ActionApproved {
        action_id: Uuid,
    },
    ActionRejected {
        action_id: Uuid,
        reason: String,
    },
    ActionDelivered(ActionDelivery),
    ActionUndoRequested {
        action_id: Uuid,
    },
    ActionUndone(ActionUndo),
    DerivationCompleted(DerivationCompleted),
    NotificationDelivered(NotificationDelivery),
    RecordingReviewed(RecordingReview),
    TranscriptAnnotated(TranscriptAnnotation),
    WorkflowRunRequested(WorkflowRunRequest),
    WorkflowRoutingRequested(WorkflowRoutingRequest),
    WorkflowTransitionAdjusted(WorkflowTransitionAdjustment),
    WorkflowEffectUndoRequested(WorkflowEffectUndoRequest),
    WorkflowRunConfirmed(WorkflowRunConfirmation),
    WorkflowRunRecorded(WorkflowRunRecord),
    WorkflowEffectUndone(WorkflowEffectUndo),
    ServiceDelivered(ServiceDelivery),
    Failure(Failure),
    /// Preserves pre-v1 source lifecycle facts while rebuilding an existing immutable manifest.
    /// New producers never emit this variant.
    LegacySourceEvent(serde_json::Value),
}

impl Message {
    fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::StopWorker { reason } if reason.trim().is_empty() => {
                Err(ContractError::EmptyField("reason"))
            }
            Self::RecordingAvailable(value) if value.source_path.trim().is_empty() => {
                Err(ContractError::EmptyField("source_path"))
            }
            Self::AudioIngested(value) if value.sha256.len() != 64 => {
                Err(ContractError::InvalidHash)
            }
            Self::TranscriptCompleted(value) if value.segments.is_empty() => {
                Err(ContractError::EmptyField("segments"))
            }
            Self::ActionProposed(value) if value.title.trim().is_empty() => {
                Err(ContractError::EmptyField("title"))
            }
            Self::ActionRevisionRequested(value) if value.title.trim().is_empty() => {
                Err(ContractError::EmptyField("title"))
            }
            Self::DerivationCompleted(value)
                if value
                    .actions
                    .iter()
                    .any(|action| action.title.trim().is_empty()) =>
            {
                Err(ContractError::EmptyField("derivation action title"))
            }
            Self::NotificationDelivered(value) if value.summary.trim().is_empty() => {
                Err(ContractError::EmptyField("notification summary"))
            }
            Self::RecordingReviewed(value)
                if value.outcome == ReviewOutcome::NeedsAttention
                    && value
                        .note
                        .as_ref()
                        .is_none_or(|note| note.trim().is_empty()) =>
            {
                Err(ContractError::EmptyField("recording review note"))
            }
            Self::ActionRejected { reason, .. } if reason.trim().is_empty() => {
                Err(ContractError::EmptyField("reason"))
            }
            Self::TranscriptAnnotated(value) if value.replacement_text.trim().is_empty() => {
                Err(ContractError::EmptyField("replacement_text"))
            }
            Self::ServiceDelivered(value) if value.service_id.trim().is_empty() => {
                Err(ContractError::EmptyField("service_id"))
            }
            Self::SourceCommandRequested(value) if value.adapter_id.trim().is_empty() => {
                Err(ContractError::EmptyField("adapter_id"))
            }
            Self::SourceAdapterSelectionRequested { adapter_id }
                if adapter_id.trim().is_empty() =>
            {
                Err(ContractError::EmptyField("adapter_id"))
            }
            Self::SourceStatusChanged(value)
                if value.adapter_id.trim().is_empty() || value.source_id.trim().is_empty() =>
            {
                Err(ContractError::EmptyField("source identity"))
            }
            Self::WorkflowRunRequested(value) if value.workflow_id.trim().is_empty() => {
                Err(ContractError::EmptyField("workflow_id"))
            }
            Self::WorkflowTransitionAdjusted(value)
                if value.selection.transition_id.trim().is_empty() =>
            {
                Err(ContractError::EmptyField("transition_id"))
            }
            Self::WorkflowRunRecorded(value) if value.workflow_id.trim().is_empty() => {
                Err(ContractError::EmptyField("workflow_id"))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    #[serde(alias = "bluetooth")]
    Source,
    TransferIngress,
    Lake,
    Transcribe,
    Derive,
    Calendar,
    Notify,
    Services,
}

/// One deterministic policy for every local-model leaf. Ollama has reported the same nominal
/// model as both `24B` and `24.0B`; accepting only those normalized forms avoids silently running
/// a smaller or remote substitute while tolerating metadata formatting.
pub fn is_required_local_model_size(value: &str) -> bool {
    matches!(value.trim().to_ascii_uppercase().as_str(), "24B" | "24.0B")
}

fn migrate_wire_value(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let message_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if matches!(
        message_type.as_deref(),
        Some(
            "scan_requested"
                | "device_observed"
                | "gatt_inventory"
                | "device_bound"
                | "release_device_requested"
                | "device_release_requested"
                | "device_released"
                | "transfer_requested"
        )
    ) {
        object.insert("type".into(), serde_json::json!("legacy_source_event"));
        if !object.contains_key("payload") {
            object.insert("payload".into(), serde_json::Value::Null);
        }
        return;
    }
    let Some(payload) = object
        .get_mut("payload")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    if matches!(
        message_type.as_deref(),
        Some("recording_available" | "audio_ingested")
    ) && let Some(source_id) = payload.remove("device_id")
    {
        payload.entry("source_id").or_insert(source_id);
    }
    if message_type.as_deref() == Some("recording_available") {
        let adapter_id = payload
            .remove("source")
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "legacy-source".into());
        payload
            .entry("adapter_id")
            .or_insert(serde_json::Value::String(adapter_id));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerHealth {
    pub role: WorkerRole,
    pub state: HealthState,
    pub detail: String,
    pub backlog: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Starting,
    Ready,
    Degraded,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceCommand {
    pub adapter_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub operation: SourceOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceOperation {
    Discover {
        duration_ms: u64,
        allow_pairing: bool,
    },
    Sync {
        destination_uri: String,
        idle_timeout_ms: u64,
    },
    Release,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceCapability {
    Discover,
    Pair,
    Sync,
    Release,
    BackgroundIngest,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Discovered,
    Bound,
    Ready,
    Syncing,
    Released,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceStatus {
    pub adapter_id: String,
    pub source_id: String,
    pub display_name: String,
    pub state: SourceState,
    pub capabilities: Vec<SourceCapability>,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// A replaceable source boundary. Concrete transports implement this interface in leaf crates;
/// process-isolated adapters carry the same commands and outputs in versioned envelopes.
pub trait SourceAdapter<Artifact> {
    type Error;

    fn adapter_id(&self) -> &str;
    fn source_id(&self) -> &str;
    fn capabilities(&self) -> &[SourceCapability];
    fn execute(
        &mut self,
        command: SourceCommand,
    ) -> Result<Vec<SourceAdapterOutput<Artifact>>, Self::Error>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceAdapterOutput<Artifact> {
    Status(SourceStatus),
    Artifact(Artifact),
}

/// Execute an in-process recording source through the same identity and capability contract used
/// at process boundaries. This is deliberately generic: folder, application-file, and network
/// leaves share the driver without the core importing any concrete adapter.
pub fn execute_recording_source<Adapter>(
    adapter: &mut Adapter,
    command: SourceCommand,
) -> Result<Vec<SourceAdapterOutput<RecordingAvailable>>, String>
where
    Adapter: SourceAdapter<RecordingAvailable>,
    Adapter::Error: std::fmt::Display,
{
    let adapter_id = adapter.adapter_id().to_owned();
    let source_id = adapter.source_id().to_owned();
    if command.adapter_id != adapter_id
        || command
            .source_id
            .as_deref()
            .is_some_and(|requested| requested != source_id)
    {
        return Err("source command identity does not match its adapter".into());
    }
    let required: &[SourceCapability] = match &command.operation {
        SourceOperation::Discover { allow_pairing, .. } if *allow_pairing => {
            &[SourceCapability::Discover, SourceCapability::Pair]
        }
        SourceOperation::Discover { .. } => &[SourceCapability::Discover],
        SourceOperation::Sync { .. } => &[SourceCapability::Sync],
        SourceOperation::Release => &[SourceCapability::Release],
    };
    if let Some(missing) = required
        .iter()
        .find(|capability| !adapter.capabilities().contains(capability))
    {
        return Err(format!(
            "source adapter {adapter_id} does not declare required capability {missing:?}"
        ));
    }
    let mut declared_capabilities = adapter.capabilities().to_vec();
    declared_capabilities.sort();
    declared_capabilities.dedup();
    if declared_capabilities.len() != adapter.capabilities().len() {
        return Err(format!(
            "source adapter {adapter_id} declares duplicate capabilities"
        ));
    }
    let outputs = adapter
        .execute(command)
        .map_err(|error| format!("source adapter {adapter_id} failed: {error}"))?;
    validate_source_contract(&adapter_id, &source_id, &outputs)?;
    for output in &outputs {
        if let SourceAdapterOutput::Status(status) = output {
            let mut advertised = status.capabilities.clone();
            advertised.sort();
            if advertised != declared_capabilities {
                return Err(format!(
                    "source adapter {adapter_id} changed capabilities at its output boundary"
                ));
            }
        }
    }
    Ok(outputs)
}

/// Shared conformance check used by process, folder, application, and network source leaves. A
/// source may expose lifecycle states, artifacts, or both; every output must preserve the same
/// adapter/source identity and advertise the capability required by its state.
pub fn validate_source_contract(
    adapter_id: &str,
    source_id: &str,
    outputs: &[SourceAdapterOutput<RecordingAvailable>],
) -> Result<(), String> {
    if adapter_id.trim().is_empty() || source_id.trim().is_empty() {
        return Err("source contract identities must be non-empty".into());
    }
    if outputs.is_empty() {
        return Err("source contract trace must contain an output".into());
    }
    for output in outputs {
        match output {
            SourceAdapterOutput::Status(status) => {
                if status.adapter_id != adapter_id || status.source_id != source_id {
                    return Err("source status changed adapter or source identity".into());
                }
                if status.display_name.trim().is_empty() {
                    return Err("source status display name must be non-empty".into());
                }
                let mut capabilities = status.capabilities.clone();
                capabilities.sort();
                capabilities.dedup();
                if capabilities.len() != status.capabilities.len() {
                    return Err("source capabilities must not contain duplicates".into());
                }
                let required = match status.state {
                    SourceState::Discovered => Some(SourceCapability::Discover),
                    SourceState::Bound => Some(SourceCapability::Pair),
                    SourceState::Syncing => Some(SourceCapability::Sync),
                    SourceState::Released => Some(SourceCapability::Release),
                    SourceState::Ready | SourceState::Unavailable => None,
                };
                if required.is_some_and(|required| !status.capabilities.contains(&required)) {
                    return Err(format!(
                        "source state {:?} is missing capability {:?}",
                        status.state, required
                    ));
                }
            }
            SourceAdapterOutput::Artifact(recording) => {
                if recording.adapter_id != adapter_id || recording.source_id != source_id {
                    return Err("recording artifact changed adapter or source identity".into());
                }
                if recording.source_path.trim().is_empty() || recording.recorded_at_ms <= 0 {
                    return Err("recording artifact requires a path and positive timestamp".into());
                }
            }
        }
    }
    Ok(())
}

/// Inward-facing ports keep infrastructure incidental. Each leaf chooses its own concrete error,
/// request, and response types while the state-machine core depends only on these capabilities.
pub mod ports {
    pub trait MessageTransport<Message> {
        type Error;
        fn send(&mut self, message: &Message) -> Result<(), Self::Error>;
    }

    pub trait EventStore<Event> {
        type Error;
        type Receipt;
        fn append(&mut self, event: &Event) -> Result<Self::Receipt, Self::Error>;
    }

    pub trait Projection<Event, View> {
        type Error;
        fn apply(&mut self, event: &Event) -> Result<(), Self::Error>;
        fn snapshot(&self) -> Result<View, Self::Error>;
    }

    pub trait Transcriber<Audio, Transcript> {
        type Error;
        fn transcribe(&mut self, audio: Audio) -> Result<Transcript, Self::Error>;
    }

    /// Human and model implementations intentionally satisfy the same bounded oracle slot.
    #[allow(async_fn_in_trait)]
    pub trait Oracle<Context, Selection> {
        type Error;
        async fn select(&mut self, context: Context) -> Result<Selection, Self::Error>;
    }

    pub trait EffectAdapter<Effect, Receipt> {
        type Error;
        fn apply(&mut self, effect: Effect) -> Result<Receipt, Self::Error>;
        fn undo(&mut self, receipt: Receipt) -> Result<(), Self::Error>;
    }

    pub trait ReadModelClient<Query, View> {
        type Error;
        fn query(&mut self, query: Query) -> Result<View, Self::Error>;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingAvailable {
    pub source_id: String,
    pub adapter_id: String,
    pub source_path: String,
    pub recorded_at_ms: i64,
}

impl RecordingAvailable {
    pub fn stable_id(&self, sha256: &str) -> Uuid {
        Uuid::new_v5(&RECORDING_NAMESPACE, sha256.as_bytes())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioIngested {
    pub recording_id: Uuid,
    pub source_id: String,
    pub sha256: String,
    pub relative_path: String,
    pub byte_len: u64,
    pub recorded_at_ms: i64,
    pub ingested_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptionRequest {
    pub recording_id: Uuid,
    pub absolute_audio_path: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptCompleted {
    pub recording_id: Uuid,
    pub run_id: Uuid,
    #[serde(default)]
    pub recorded_at_ms: i64,
    pub model: String,
    pub model_version: String,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub segments: Vec<TranscriptSegment>,
    /// The backend's unmodified structured response. This is retained in the immutable lake so
    /// future projections can use fields that today's normalized schema does not understand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptSegment {
    pub ordinal: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: Option<String>,
    pub text: String,
    pub confidence: Option<f32>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DerivationRequest {
    pub recording_id: Uuid,
    pub transcript_run_id: Uuid,
    pub recorded_at_ms: i64,
    pub segments: Vec<TranscriptSegment>,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionProposal {
    pub action_id: Uuid,
    pub recording_id: Uuid,
    pub transcript_run_id: Uuid,
    pub kind: ActionKind,
    pub title: String,
    pub due_at_ms: Option<i64>,
    pub original_time_phrase: Option<String>,
    pub timezone: String,
    pub confidence: f32,
    pub evidence: Vec<SegmentEvidence>,
    pub extractor_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionRevisionRequest {
    pub action_id: Uuid,
    pub kind: ActionKind,
    pub title: String,
    pub due_at_ms: Option<i64>,
    pub requested_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Todo,
    Reminder,
    CalendarEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentEvidence {
    pub ordinal: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub quote: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionDelivery {
    pub action_id: Uuid,
    pub external_id: String,
    pub delivered_at_ms: i64,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionUndo {
    pub action_id: Uuid,
    pub target: String,
    pub undone_at_ms: i64,
}

/// Terminal marker for one derivation run. It lets independent consumers distinguish an
/// action-free recording from a derivation that is still running, without inspecting logs or
/// relying on timing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DerivationCompleted {
    pub recording_id: Uuid,
    pub transcript_run_id: Uuid,
    pub actions: Vec<DerivedAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DerivedAction {
    pub action_id: Uuid,
    pub kind: ActionKind,
    pub title: String,
    pub auto_approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotificationDelivery {
    pub recording_id: Uuid,
    pub delivered_at_ms: i64,
    pub summary: String,
    pub hooks: Vec<NotificationHook>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotificationHook {
    pub action_id: Uuid,
    pub kind: ActionKind,
    pub title: String,
    /// Stable machine-generated state such as `delivered`, `awaiting_review`, or
    /// `delivery_unconfirmed`. The local model never controls this value.
    pub status: String,
    pub target: Option<String>,
}

/// A durable acknowledgement of the processing report for one recording. This is separate from
/// transcript corrections and action approvals: it records whether the user checked that the
/// complete set of inferred and delivered outcomes matched their intent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingReview {
    pub review_id: Uuid,
    pub recording_id: Uuid,
    pub outcome: ReviewOutcome,
    pub note: Option<String>,
    pub reviewed_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcome {
    Confirmed,
    NeedsAttention,
}

/// A non-destructive user correction. The immutable transcript segment remains canonical raw
/// evidence; lakehouse readers overlay the latest annotation when presenting corrected text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptAnnotation {
    pub annotation_id: Uuid,
    pub recording_id: Uuid,
    pub transcript_run_id: Uuid,
    pub ordinal: u32,
    pub replacement_text: String,
    pub reason: Option<String>,
    pub annotated_at_ms: i64,
}

/// A user-owned state machine. Definitions are persisted as JSON and consumed by the UI and
/// runner through this shared abstraction; neither side depends on the other's implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowDefinition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub subscriptions: Vec<String>,
    pub initial_state_id: String,
    pub states: Vec<WorkflowState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowState {
    pub id: String,
    #[serde(default)]
    pub description: String,
    pub selector: WorkflowSelector,
    #[serde(default)]
    pub transitions: Vec<WorkflowTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowSelector {
    /// A fixed transition; no model or user judgment is involved.
    Deterministic { transition_id: String },
    /// Deterministically bounds the amount of free text exposed to later oracle states.
    TextLength {
        field: String,
        maximum_chars: u32,
        within_transition_id: String,
        overflow_transition_id: String,
    },
    /// Deterministically validates oracle-supplied action evidence before an effect transition.
    /// Prior evidence fields prevent one clause from being reinterpreted by several hooks while
    /// still allowing distinct clauses in one message to produce separate actions.
    ActionEvidence {
        transcript_field: String,
        evidence_field: String,
        #[serde(default)]
        prior_evidence_fields: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_at_ms_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time_phrase_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accepted_evidence_field: Option<String>,
        valid_transition_id: String,
        invalid_transition_id: String,
    },
    /// Deterministic tolerant substring routing over a declared extended-state field.
    PhraseMatch {
        field: String,
        cases: Vec<WorkflowPhraseCase>,
        fallback_transition_id: String,
    },
    /// The model may select only a transition exposed by this state and may fill only the output
    /// fields declared by that transition.
    LocalModel { instruction: String },
    /// Execution pauses until the user selects one of this state's declared transitions.
    User,
    /// A terminal control state has no outgoing transitions.
    Terminal { outcome: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowPhraseCase {
    pub phrases: Vec<String>,
    pub transition_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowTransition {
    pub id: String,
    pub description: String,
    pub destination_state_id: String,
    #[serde(default)]
    pub output_fields: Vec<WorkflowOutputField>,
    #[serde(default)]
    pub action: WorkflowAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowOutputField {
    pub name: String,
    pub description: String,
    pub kind: WorkflowOutputKind,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowOutputKind {
    NaturalLanguage { max_length: u32 },
    Choice { values: Vec<String> },
    Integer { minimum: i64, maximum: i64 },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
// This is a versioned serialized contract shared by independent crates. Keeping the complete
// forward and compensating adapter definitions inline makes the JSON legible and stable; boxing a
// field only to equalize the in-memory size of `None` would leak an allocation concern into every
// contract consumer without changing the wire representation.
#[allow(clippy::large_enum_variant)]
pub enum WorkflowAction {
    #[default]
    None,
    Effect {
        adapter: WorkflowAdapter,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        undo_adapter: Option<WorkflowAdapter>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowAdapter {
    Shortcut {
        target: String,
    },
    Webhook {
        target: String,
    },
    #[serde(alias = "mac_action")]
    SystemAction {
        kind: ActionKind,
        title_field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_at_ms_field: Option<String>,
        /// Optional model/human-supplied verbatim evidence. The services adapter validates that
        /// it occurs in the immutable transcript before emitting an action proposal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_field: Option<String>,
        /// Optional verbatim time expression paired with `due_at_ms_field`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time_phrase_field: Option<String>,
    },
    #[serde(alias = "mac_action_undo")]
    SystemActionUndo,
    #[serde(alias = "mac_app")]
    Document {
        kind: DocumentKind,
        title_field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_field: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_field: Option<String>,
    },
    #[serde(alias = "mac_app_undo")]
    DocumentUndo,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Notes,
    MailDraft,
    Contacts,
    TextFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowSelection {
    pub transition_id: String,
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
}

// An oracle cannot create states, transitions, destinations, or effect kinds. Its complete output
// is a `WorkflowSelection`, whose fields are validated against one declared transition schema.
// Human and model oracles are deliberately substitutable providers of this exact same contract.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowInput {
    pub recording_id: Uuid,
    pub transcript_run_id: Uuid,
    pub recorded_at_ms: i64,
    pub transcript: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunRequester {
    Automatic,
    User,
    LocalModel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowRunRequest {
    pub run_id: Uuid,
    pub workflow_id: String,
    pub input: WorkflowInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_state_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_selection: Option<WorkflowSelection>,
    pub requested_by: WorkflowRunRequester,
    pub requested_at_ms: i64,
}

/// Asks the configured local model to choose relevant enabled workflows. The resulting workflows
/// still execute through the same deterministic runner and produce ordinary auditable runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowRoutingRequest {
    pub request_id: Uuid,
    pub input: WorkflowInput,
    pub requested_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowTransitionAdjustment {
    pub run_id: Uuid,
    pub state_id: String,
    pub selection: WorkflowSelection,
    pub adjusted_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowEffectUndoRequest {
    pub run_id: Uuid,
    pub transition_id: String,
    pub requested_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowRunConfirmation {
    pub run_id: Uuid,
    pub confirmed_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowRunRecord {
    pub run_id: Uuid,
    pub workflow_id: String,
    pub recording_id: Uuid,
    pub requested_by: WorkflowRunRequester,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub status: String,
    pub steps: Vec<WorkflowStepRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowStepRecord {
    pub state_id: String,
    pub available_transition_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<WorkflowSelection>,
    pub selected_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_state_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    #[serde(default)]
    pub undoable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowEffectUndo {
    pub run_id: Uuid,
    pub transition_id: String,
    pub undone_at_ms: i64,
    pub receipt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceDelivery {
    pub service_id: String,
    pub source_event_id: Uuid,
    pub delivered_at_ms: i64,
    pub target: String,
    pub receipt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Failure {
    pub worker: WorkerRole,
    pub code: String,
    pub detail: String,
    pub retryable: bool,
    pub failed_message_id: Option<Uuid>,
}

#[derive(Debug, Error)]
pub enum ContractError {
    #[error("unsupported contract version {0}")]
    UnsupportedVersion(u16),
    #[error("timestamp must be positive")]
    InvalidTime,
    #[error("field {0} must not be empty")]
    EmptyField(&'static str),
    #[error("sha256 must be 64 hex characters")]
    InvalidHash,
    #[error("invalid JSON contract: {0}")]
    Json(#[from] serde_json::Error),
}

/// Canonical examples consumed by every worker's independent contract test.
pub fn contract_fixtures() -> Vec<Envelope> {
    let recording_id = Uuid::new_v5(&RECORDING_NAMESPACE, b"fixture-recording");
    let transcript_run_id = Uuid::new_v5(&RECORDING_NAMESPACE, b"fixture-transcript");
    let action_id = Uuid::new_v5(&RECORDING_NAMESPACE, b"fixture-action");
    vec![
        Envelope::root(Message::SourceCommandRequested(SourceCommand {
            adapter_id: "fixture-source".into(),
            source_id: None,
            operation: SourceOperation::Discover {
                duration_ms: 1_000,
                allow_pairing: true,
            },
        })),
        Envelope::root(Message::AudioIngested(AudioIngested {
            recording_id,
            source_id: "fixture-source".into(),
            sha256: "a".repeat(64),
            relative_path: "objects/audio/aa/fixture.wav".into(),
            byte_len: 42,
            recorded_at_ms: 1_700_000_000_000,
            ingested_at_ms: 1_700_000_001_000,
        })),
        Envelope::root(Message::TranscriptCompleted(TranscriptCompleted {
            recording_id,
            run_id: transcript_run_id,
            recorded_at_ms: 1_700_000_000_000,
            model: "fixture".into(),
            model_version: "1".into(),
            started_at_ms: 1_700_000_001_000,
            completed_at_ms: 1_700_000_002_000,
            segments: vec![TranscriptSegment {
                ordinal: 0,
                start_ms: 0,
                end_ms: 2_000,
                speaker: Some("Speaker A".into()),
                text: "Remind me to call Jess tomorrow at nine".into(),
                confidence: Some(0.9),
                language: Some("en".into()),
            }],
            provider_payload: None,
        })),
        Envelope::root(Message::ActionProposed(ActionProposal {
            action_id,
            recording_id,
            transcript_run_id,
            kind: ActionKind::Reminder,
            title: "Call Jess".into(),
            due_at_ms: Some(1_700_086_400_000),
            original_time_phrase: Some("tomorrow at nine".into()),
            timezone: "Europe/London".into(),
            confidence: 0.9,
            evidence: vec![SegmentEvidence {
                ordinal: 0,
                start_ms: 0,
                end_ms: 2_000,
                quote: "Remind me to call Jess tomorrow at nine".into(),
            }],
            extractor_version: "fixture-1".into(),
        })),
        Envelope::root(Message::ActionRevisionRequested(ActionRevisionRequest {
            action_id,
            kind: ActionKind::Reminder,
            title: "Call Jessica".into(),
            due_at_ms: Some(1_700_090_000_000),
            requested_at_ms: 1_700_000_002_500,
        })),
        Envelope::root(Message::DerivationCompleted(DerivationCompleted {
            recording_id,
            transcript_run_id,
            actions: vec![DerivedAction {
                action_id,
                kind: ActionKind::Reminder,
                title: "Call Jess".into(),
                auto_approved: true,
            }],
        })),
        Envelope::root(Message::NotificationDelivered(NotificationDelivery {
            recording_id,
            delivered_at_ms: 1_700_000_003_000,
            summary: "Call Jess tomorrow.".into(),
            hooks: vec![NotificationHook {
                action_id,
                kind: ActionKind::Reminder,
                title: "Call Jess".into(),
                status: "delivered".into(),
                target: Some("macos_reminders".into()),
            }],
        })),
        Envelope::root(Message::RecordingReviewed(RecordingReview {
            review_id: Uuid::new_v5(&RECORDING_NAMESPACE, b"fixture-review"),
            recording_id,
            outcome: ReviewOutcome::Confirmed,
            note: None,
            reviewed_at_ms: 1_700_000_004_000,
        })),
    ]
}

pub fn assert_contract_fixtures() {
    for fixture in contract_fixtures() {
        let encoded = fixture.to_ndjson().expect("fixture must encode");
        let decoded = Envelope::from_json(encoded.trim()).expect("fixture must decode");
        assert_eq!(decoded, fixture);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureSource {
        advertised: Vec<SourceCapability>,
    }

    impl SourceAdapter<RecordingAvailable> for FixtureSource {
        type Error = String;

        fn adapter_id(&self) -> &str {
            "fixture"
        }

        fn source_id(&self) -> &str {
            "one"
        }

        fn capabilities(&self) -> &[SourceCapability] {
            &[SourceCapability::Sync, SourceCapability::BackgroundIngest]
        }

        fn execute(
            &mut self,
            _command: SourceCommand,
        ) -> Result<Vec<SourceAdapterOutput<RecordingAvailable>>, String> {
            Ok(vec![SourceAdapterOutput::Status(SourceStatus {
                adapter_id: "fixture".into(),
                source_id: "one".into(),
                display_name: "Fixture".into(),
                state: SourceState::Ready,
                capabilities: self.advertised.clone(),
                attributes: Default::default(),
            })])
        }
    }

    #[test]
    fn canonical_contracts_round_trip() {
        assert_contract_fixtures();
    }

    #[test]
    fn source_adapter_selection_requires_an_explicit_catalog_identity() {
        let valid = Envelope::root(Message::SourceAdapterSelectionRequested {
            adapter_id: "fixture-source".into(),
        });
        assert!(Envelope::from_json(&valid.to_ndjson().unwrap()).is_ok());

        let invalid = Envelope::root(Message::SourceAdapterSelectionRequested {
            adapter_id: "  ".into(),
        });
        assert!(invalid.to_ndjson().is_err());
    }

    #[test]
    fn every_model_leaf_uses_the_same_exact_24b_policy() {
        assert!(is_required_local_model_size("24B"));
        assert!(is_required_local_model_size("24.0b"));
        assert!(!is_required_local_model_size("7B"));
        assert!(!is_required_local_model_size("24.2B"));
        assert!(!is_required_local_model_size(""));
    }

    #[test]
    fn shared_source_contract_rejects_identity_and_capability_leaks() {
        let status = SourceStatus {
            adapter_id: "fixture".into(),
            source_id: "one".into(),
            display_name: "Fixture".into(),
            state: SourceState::Released,
            capabilities: vec![SourceCapability::Release],
            attributes: Default::default(),
        };
        let artifact = RecordingAvailable {
            adapter_id: "fixture".into(),
            source_id: "one".into(),
            source_path: "/fixture/audio.m4a".into(),
            recorded_at_ms: 1,
        };
        assert!(
            validate_source_contract(
                "fixture",
                "one",
                &[
                    SourceAdapterOutput::Status(status.clone()),
                    SourceAdapterOutput::Artifact(artifact.clone()),
                ],
            )
            .is_ok()
        );
        assert!(
            validate_source_contract("fixture", "two", &[SourceAdapterOutput::Status(status)])
                .is_err()
        );
        assert!(
            validate_source_contract(
                "fixture",
                "one",
                &[SourceAdapterOutput::Status(SourceStatus {
                    state: SourceState::Released,
                    capabilities: vec![SourceCapability::Discover],
                    adapter_id: "fixture".into(),
                    source_id: "one".into(),
                    display_name: "Fixture".into(),
                    attributes: Default::default(),
                })],
            )
            .is_err()
        );
    }

    #[test]
    fn generic_source_driver_enforces_command_and_advertised_capabilities() {
        let command = || SourceCommand {
            adapter_id: "fixture".into(),
            source_id: Some("one".into()),
            operation: SourceOperation::Sync {
                destination_uri: "lake://recordings".into(),
                idle_timeout_ms: 0,
            },
        };
        let mut valid = FixtureSource {
            advertised: vec![SourceCapability::Sync, SourceCapability::BackgroundIngest],
        };
        assert!(execute_recording_source(&mut valid, command()).is_ok());

        let mut drifted = FixtureSource {
            advertised: vec![SourceCapability::Sync],
        };
        assert!(execute_recording_source(&mut drifted, command()).is_err());

        let mut wrong_identity = command();
        wrong_identity.source_id = Some("another".into());
        assert!(execute_recording_source(&mut valid, wrong_identity).is_err());
    }

    #[test]
    fn stable_recording_identity_uses_content_hash() {
        let recording = RecordingAvailable {
            source_id: "source".into(),
            adapter_id: "folder".into(),
            source_path: "/tmp/a.wav".into(),
            recorded_at_ms: 1,
        };
        assert_eq!(recording.stable_id("abc"), recording.stable_id("abc"));
        assert_ne!(recording.stable_id("abc"), recording.stable_id("def"));
    }

    #[test]
    fn legacy_source_events_and_recording_fields_are_migrated_on_read() {
        let mut legacy =
            serde_json::to_value(Envelope::root(Message::SourceStatusChanged(SourceStatus {
                adapter_id: "fixture".into(),
                source_id: "one".into(),
                display_name: "Fixture".into(),
                state: SourceState::Ready,
                capabilities: vec![],
                attributes: Default::default(),
            })))
            .unwrap();
        legacy["type"] = serde_json::json!("gatt_inventory");
        assert!(matches!(
            Envelope::from_json(&legacy.to_string()).unwrap().message,
            Message::LegacySourceEvent(_)
        ));

        let mut recording = serde_json::to_value(Envelope::root(Message::RecordingAvailable(
            RecordingAvailable {
                source_id: "old-device".into(),
                adapter_id: "watched_folder".into(),
                source_path: "/tmp/a.wav".into(),
                recorded_at_ms: 1,
            },
        )))
        .unwrap();
        let payload = recording["payload"].as_object_mut().unwrap();
        let source_id = payload.remove("source_id").unwrap();
        let adapter_id = payload.remove("adapter_id").unwrap();
        payload.insert("device_id".into(), source_id);
        payload.insert("source".into(), adapter_id);
        let decoded = Envelope::from_json(&recording.to_string()).unwrap();
        let Message::RecordingAvailable(decoded) = decoded.message else {
            panic!("legacy recording did not migrate")
        };
        assert_eq!(decoded.source_id, "old-device");
        assert_eq!(decoded.adapter_id, "watched_folder");
    }
}
