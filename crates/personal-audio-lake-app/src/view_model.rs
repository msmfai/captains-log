//! Renderer-independent application behavior.
//!
//! This module deliberately contains no AppKit, terminal, HTTP, filesystem, or process APIs.
//! Views send `Intent`s, render the resulting state, and hand emitted `Effect`s to an adapter.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use personal_audio_lake_contracts::{
    ActionKind, DocumentKind, HealthState, SourceState, WorkflowAction, WorkflowAdapter,
    WorkflowDefinition, WorkflowOutputField, WorkflowOutputKind, WorkflowSelection,
    WorkflowSelector, WorkflowState, WorkflowTransition,
};
use uuid::Uuid;

use crate::read_model::{AppAction, AppRecording, AppSegment, AppSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Review,
    Library,
    Automations,
    Settings,
}

impl Route {
    pub const ALL: [Self; 4] = [
        Self::Review,
        Self::Library,
        Self::Automations,
        Self::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::Library => "Library",
            Self::Automations => "Automations",
            Self::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Notifications,
    Calendar,
    Reminders,
}

impl Permission {
    pub fn label(self) -> &'static str {
        match self {
            Self::Notifications => "Notifications",
            Self::Calendar => "Calendar",
            Self::Reminders => "Reminders",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
    NotRequested,
    Allowed,
    Denied,
    Restricted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub recording_transfer_state: Option<HealthState>,
    pub recording_transfer_detail: String,
    pub source_name: String,
    pub source_state: Option<SourceState>,
    pub source_adapter_configured: bool,
    pub source_configured: bool,
    pub source_options: Vec<SourceOption>,
    pub local_model: String,
    pub data_lake: String,
    pub starts_at_login: bool,
    pub notifications: PermissionState,
    pub calendar: PermissionState,
    pub reminders: PermissionState,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            recording_transfer_state: None,
            recording_transfer_detail: "Waiting for local recording transfer status".into(),
            source_name: "None selected".into(),
            source_state: None,
            source_adapter_configured: false,
            source_configured: false,
            source_options: Vec::new(),
            local_model: "mistral-small3.2:latest".into(),
            data_lake: String::new(),
            starts_at_login: true,
            notifications: PermissionState::Unknown,
            calendar: PermissionState::Unknown,
            reminders: PermissionState::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOption {
    pub id: String,
    pub name: String,
    pub description: String,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingMode {
    Explicit,
    Hybrid,
    Implicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationTiming {
    WhenRelevant,
    EveryRecording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationConnection {
    Notes,
    MailDraft,
    Contacts,
    TextFile,
    Shortcut,
    Webhook,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationOption {
    pub connection: AutomationConnection,
    pub name: String,
    pub description: String,
    pub target_heading: Option<String>,
    pub target_help: Option<String>,
    pub undo_heading: Option<String>,
    pub undo_help: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationDraft {
    pub name: String,
    pub description: String,
    pub timing: AutomationTiming,
    pub connection: AutomationConnection,
    pub target: String,
    pub undo_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub mode: RoutingMode,
    pub trigger: String,
    pub destination: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Navigate(Route),
    Search(String),
    SelectVisible(usize),
    SelectRecording(Uuid),
    Refresh,
    Sync,
    SelectSourceAdapter(usize),
    ReleaseSource,
    PlayAudio,
    PlayTranscriptSegment(u32),
    RenameRecording(String),
    ConfirmProcessing,
    CorrectTranscript {
        ordinal: u32,
        replacement: String,
    },
    ApproveAction(usize),
    RejectAction {
        index: usize,
        reason: String,
    },
    RemoveAction(usize),
    AddAction {
        kind: ActionKind,
        title: String,
        due_at_ms: Option<i64>,
    },
    EditAction {
        index: usize,
        kind: ActionKind,
        title: String,
        due_at_ms: Option<i64>,
    },
    RunWorkflow {
        index: usize,
        selection: WorkflowSelection,
    },
    RunWorkflowWithLocalModel(usize),
    RouteWithLocalModel,
    AdjustWorkflowRun {
        run_index: usize,
        state_id: String,
        transition_id: String,
        outputs: BTreeMap<String, String>,
    },
    UndoWorkflowStep {
        run_index: usize,
        step_index: usize,
    },
    UndoLatestWorkflowEffect(usize),
    ToggleAutomation(usize),
    CreateAutomation(Automation),
    ToggleWorkflow(usize),
    CreateWorkflowFromDraft(AutomationDraft),
    CreateWorkflow(WorkflowDefinition),
    DeleteWorkflow(usize),
    RequestPermission(Permission),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Reload,
    LoadRecording(Uuid),
    SyncSource,
    ConfigureSourceAdapter {
        adapter_id: String,
    },
    ReleaseSource,
    PlayAudio {
        recording_id: Uuid,
        relative_path: String,
        start_ms: Option<u64>,
    },
    SaveRecordingTitle {
        recording_id: Uuid,
        title: String,
    },
    ReviewRecording {
        recording_id: Uuid,
        looks_right: bool,
        note: Option<String>,
    },
    CorrectTranscript {
        recording_id: Uuid,
        transcript_run_id: Uuid,
        ordinal: u32,
        replacement: String,
    },
    DecideAction {
        action_id: Uuid,
        approved: bool,
        reason: Option<String>,
    },
    UndoAction {
        action_id: Uuid,
    },
    CreateAction {
        action_id: Uuid,
        recording_id: Uuid,
        transcript_run_id: Uuid,
        kind: ActionKind,
        title: String,
        due_at_ms: Option<i64>,
    },
    ReviseAction {
        action_id: Uuid,
        kind: ActionKind,
        title: String,
        due_at_ms: Option<i64>,
    },
    RunWorkflow {
        workflow_id: String,
        resume_state_id: String,
        recording_id: Uuid,
        transcript_run_id: Uuid,
        recorded_at_ms: i64,
        transcript: String,
        selection: WorkflowSelection,
    },
    RunWorkflowWithLocalModel {
        workflow_id: String,
        resume_state_id: String,
        recording_id: Uuid,
        transcript_run_id: Uuid,
        recorded_at_ms: i64,
        transcript: String,
    },
    RouteWithLocalModel {
        recording_id: Uuid,
        transcript_run_id: Uuid,
        recorded_at_ms: i64,
        transcript: String,
    },
    AdjustWorkflowRun {
        run_id: Uuid,
        state_id: String,
        selection: WorkflowSelection,
    },
    UndoWorkflowStep {
        run_id: Uuid,
        transition_id: String,
    },
    ConfirmWorkflowRun {
        run_id: Uuid,
    },
    SaveAutomation(Automation),
    SaveWorkflow(WorkflowDefinition),
    DeleteWorkflow(String),
    RequestPermission(Permission),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewModel {
    route: Route,
    snapshot: AppSnapshot,
    selected: Option<Uuid>,
    library_query: String,
    automations: Vec<Automation>,
    workflows: Vec<WorkflowDefinition>,
    settings: Settings,
    feedback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRow {
    pub recording_id: Uuid,
    pub title: String,
    pub subtitle: String,
    pub outcome: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingHeader {
    pub title: String,
    pub metadata: String,
    pub review_status: String,
    pub audio_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionCard {
    pub index: usize,
    pub kind: ActionKind,
    pub operation: String,
    pub payload_label: String,
    pub payload: String,
    pub due_at_ms: Option<i64>,
    pub scheduled_for: Option<String>,
    pub evidence_quote: Option<String>,
    pub destination: String,
    pub state: ActionReceiptState,
    pub state_label: String,
    pub state_detail: String,
    pub can_decide: bool,
    pub remove_label: Option<String>,
    pub edit_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionEditor {
    pub heading: String,
    pub explanation: String,
    pub submit_label: String,
    pub kind: ActionKind,
    pub title: String,
    pub due_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionReceiptState {
    AwaitingApproval,
    ApprovedPendingDelivery,
    Delivered,
    Skipped,
    Undone,
    Processing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSegmentRow {
    pub ordinal: u32,
    pub time_label: String,
    pub text: String,
    pub corrected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityReceipt {
    pub run_index: usize,
    pub workflow: String,
    pub triggered_by: String,
    pub state: String,
    pub result: String,
    pub confirmation: String,
    pub can_adjust: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowCorrectionField {
    pub name: String,
    pub label: String,
    pub value: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowCorrectionEditor {
    pub run_index: usize,
    pub state_id: String,
    pub transition_id: String,
    pub heading: String,
    pub explanation: String,
    pub apply_label: String,
    pub can_undo: bool,
    pub fields: Vec<WorkflowCorrectionField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewAcknowledgement {
    pub heading: String,
    pub explanation: String,
    pub action_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingPresentation {
    pub header: RecordingHeader,
    pub review_acknowledgement: Option<ReviewAcknowledgement>,
    pub summary: String,
    pub no_action_message: Option<String>,
    pub actions: Vec<ActionCard>,
    pub transcript: Vec<TranscriptSegmentRow>,
    pub activity: Vec<ActivityReceipt>,
    pub technical_trace: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRow {
    pub index: usize,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub status_label: String,
    pub toggle_label: String,
    pub run_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRow {
    pub permission: Permission,
    pub name: String,
    pub status: String,
    pub action_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPresentation {
    pub recording_transfer_status: String,
    pub recording_transfer_detail: String,
    pub recording_transfer_help: String,
    pub source_name: String,
    pub connection: String,
    pub source_setup_required: bool,
    pub can_release_source: bool,
    pub source_options: Vec<SourceOption>,
    pub processing: String,
    pub storage: String,
    pub open_at_login: String,
    pub permissions: Vec<PermissionRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundOperationalState {
    Idle,
    Active,
    NeedsAttention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundControlPresentation {
    pub primary_route: Route,
    pub primary_action: String,
    pub source_status: String,
    pub sync_action: String,
    pub can_sync: bool,
    pub source_setup_required: bool,
    pub operational_state: BackgroundOperationalState,
}

impl ViewModel {
    pub fn integration_catalog() -> Vec<IntegrationOption> {
        vec![
            IntegrationOption {
                connection: AutomationConnection::Notes,
                name: "Apple Notes".into(),
                description: "Create an undoable note, optionally in an existing folder.".into(),
                target_heading: None,
                target_help: None,
                undo_heading: None,
                undo_help: None,
            },
            IntegrationOption {
                connection: AutomationConnection::MailDraft,
                name: "Apple Mail Draft".into(),
                description: "Prepare a saved email draft. The app never sends it.".into(),
                target_heading: None,
                target_help: None,
                undo_heading: None,
                undo_help: None,
            },
            IntegrationOption {
                connection: AutomationConnection::Contacts,
                name: "Apple Contacts".into(),
                description: "Create a contact with an email address or phone number.".into(),
                target_heading: None,
                target_help: None,
                undo_heading: None,
                undo_help: None,
            },
            IntegrationOption {
                connection: AutomationConnection::TextFile,
                name: "Text File".into(),
                description: "Write a unique text file in Documents/Captain's Log.".into(),
                target_heading: None,
                target_help: None,
                undo_heading: None,
                undo_help: None,
            },
            IntegrationOption {
                connection: AutomationConnection::Shortcut,
                name: "Apple Shortcut".into(),
                description:
                    "Connect Messages, Home, Maps, Music, or any app supported by Shortcuts.".into(),
                target_heading: Some("Shortcut Name".into()),
                target_help: Some("Enter the exact name shown in the Shortcuts app.".into()),
                undo_heading: Some("Undo Shortcut".into()),
                undo_help: Some("Optional: enter a Shortcut that reverses the change.".into()),
            },
            IntegrationOption {
                connection: AutomationConnection::Webhook,
                name: "Webhook".into(),
                description: "Send structured data to an HTTPS endpoint.".into(),
                target_heading: Some("Webhook Address".into()),
                target_help: Some(
                    "Enter the HTTPS address that should receive the recording data.".into(),
                ),
                undo_heading: Some("Undo Webhook".into()),
                undo_help: Some("Optional: enter a webhook that reverses the change.".into()),
            },
        ]
    }

    pub fn workflow_correction_editor(
        &self,
        run_index: usize,
    ) -> Result<WorkflowCorrectionEditor, String> {
        let run = self
            .require_selected()?
            .workflow_runs
            .get(run_index)
            .ok_or_else(|| format!("No workflow run {}", run_index + 1))?;
        let workflow = self
            .workflows
            .iter()
            .find(|workflow| workflow.id == run.workflow_id)
            .ok_or_else(|| format!("Automation '{}' is no longer configured", run.workflow_id))?;
        let (step, state, transition) = run
            .steps
            .iter()
            .rev()
            .filter_map(|step| {
                let transition_id = step.transition_id.as_deref()?;
                let state = workflow
                    .states
                    .iter()
                    .find(|state| state.id == step.state_id)?;
                let transition = state
                    .transitions
                    .iter()
                    .find(|transition| transition.id == transition_id)?;
                matches!(transition.action, WorkflowAction::Effect { .. })
                    .then_some((step, state, transition))
            })
            .next()
            .ok_or_else(|| "This automation did not create an editable action".to_owned())?;
        let fields = transition
            .output_fields
            .iter()
            .map(|field| WorkflowCorrectionField {
                name: field.name.clone(),
                label: field.description.clone(),
                value: step.outputs.get(&field.name).cloned().unwrap_or_default(),
                required: field.required,
            })
            .collect();
        Ok(WorkflowCorrectionEditor {
            run_index,
            state_id: state.id.clone(),
            transition_id: transition.id.clone(),
            heading: format!("Change {}", workflow_display_name(workflow)),
            explanation: "Edit the values the app used. Applying changes removes the previous native item before creating its corrected replacement.".into(),
            apply_label: "Apply Changes".into(),
            can_undo: step.undoable && !step.undone,
            fields,
        })
    }

    pub fn empty(settings: Settings) -> Self {
        Self::new(AppSnapshot::default(), Vec::new(), Vec::new(), settings)
    }

    pub fn new(
        snapshot: AppSnapshot,
        automations: Vec<Automation>,
        workflows: Vec<WorkflowDefinition>,
        settings: Settings,
    ) -> Self {
        let route = if snapshot.recordings.iter().any(AppRecording::needs_review) {
            Route::Review
        } else {
            Route::Library
        };
        let mut model = Self {
            route,
            snapshot,
            selected: None,
            library_query: String::new(),
            automations,
            workflows,
            settings,
            feedback: None,
        };
        model.ensure_selection();
        model
    }

    pub fn route(&self) -> Route {
        self.route
    }

    pub fn selected_id(&self) -> Option<Uuid> {
        self.selected
    }

    fn selected_recording(&self) -> Option<&AppRecording> {
        let selected = self.selected?;
        self.snapshot
            .recordings
            .iter()
            .find(|recording| recording.recording_id == selected)
    }

    fn visible_recordings(&self) -> Vec<&AppRecording> {
        self.snapshot
            .recordings
            .iter()
            .filter(|recording| match self.route {
                Route::Review => recording.needs_review(),
                Route::Library => {
                    let query = self.library_query.trim().to_lowercase();
                    query.is_empty()
                        || self
                            .recording_title(recording)
                            .to_lowercase()
                            .contains(&query)
                        || recording.summary.to_lowercase().contains(&query)
                        || recording.transcript.to_lowercase().contains(&query)
                }
                Route::Automations | Route::Settings => false,
            })
            .collect()
    }

    #[cfg(test)]
    fn recordings(&self) -> &[AppRecording] {
        &self.snapshot.recordings
    }

    pub fn review_count(&self) -> usize {
        self.snapshot
            .recordings
            .iter()
            .filter(|recording| recording.needs_review())
            .count()
    }

    pub fn inbox_badge_text(&self) -> Option<String> {
        match self.review_count() {
            0 => None,
            count @ 1..=99 => Some(count.to_string()),
            _ => Some("99+".into()),
        }
    }

    pub fn background_control_presentation(&self) -> BackgroundControlPresentation {
        let waiting = self.review_count();
        let primary_route = if waiting == 0 {
            Route::Library
        } else {
            Route::Review
        };
        let primary_action = match waiting {
            0 => "Open Captain’s Log".into(),
            1 => "Review 1 Recording".into(),
            count => format!("Review {count} Recordings"),
        };
        let transfer_status = match self.settings.recording_transfer_state {
            Some(HealthState::Ready) => "Recorder App Transfer · Ready",
            Some(HealthState::Starting) | None => "Recorder App Transfer · Starting",
            Some(HealthState::Degraded | HealthState::Stopped) => {
                "Recorder App Transfer · Needs Attention"
            }
        };
        let source_status = if !self.settings.source_adapter_configured {
            transfer_status.into()
        } else {
            match self.settings.source_state {
                Some(SourceState::Discovered) => format!("{} · Found", self.settings.source_name),
                Some(SourceState::Bound) => format!("{} · Paired", self.settings.source_name),
                Some(SourceState::Ready) => format!("{} · Ready", self.settings.source_name),
                Some(SourceState::Syncing) => format!("{} · Syncing", self.settings.source_name),
                Some(SourceState::Unavailable) => {
                    format!("{} · Needs Attention", self.settings.source_name)
                }
                Some(SourceState::Released) | None if !self.settings.source_configured => {
                    format!("{} · Not Paired", self.settings.source_name)
                }
                Some(SourceState::Released) | None => {
                    format!("{} · Paired, Waiting", self.settings.source_name)
                }
            }
        };
        let (sync_action, can_sync) = match self.settings.source_state {
            Some(SourceState::Syncing) => ("Syncing…".into(), false),
            Some(SourceState::Unavailable) => ("Try Sync Again".into(), true),
            Some(SourceState::Released) | None if !self.settings.source_adapter_configured => {
                ("Add Compatibility Device…".into(), true)
            }
            _ => ("Sync Now".into(), true),
        };
        let operational_state = match self.settings.recording_transfer_state {
            Some(HealthState::Degraded | HealthState::Stopped) => {
                BackgroundOperationalState::NeedsAttention
            }
            Some(HealthState::Starting) | None => BackgroundOperationalState::Active,
            Some(HealthState::Ready) => match self.settings.source_state {
                Some(SourceState::Syncing) => BackgroundOperationalState::Active,
                Some(SourceState::Unavailable) => BackgroundOperationalState::NeedsAttention,
                _ => BackgroundOperationalState::Idle,
            },
        };
        BackgroundControlPresentation {
            primary_route,
            primary_action,
            source_status,
            sync_action,
            can_sync,
            source_setup_required: !self.settings.source_adapter_configured,
            operational_state,
        }
    }

    pub fn contains_recording(&self, recording_id: Uuid) -> bool {
        self.snapshot
            .recordings
            .iter()
            .any(|recording| recording.recording_id == recording_id)
    }

    pub fn library_rows(&self) -> Vec<LibraryRow> {
        self.visible_recordings()
            .into_iter()
            .map(|recording| LibraryRow {
                recording_id: recording.recording_id,
                title: self.recording_title(recording),
                subtitle: timestamp(recording.recorded_at_ms),
                outcome: if recording.needs_review() {
                    "Needs Review".into()
                } else if recording.actions.is_empty() {
                    "Nothing Added".into()
                } else {
                    format!(
                        "{} change{}",
                        recording.actions.len(),
                        if recording.actions.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )
                },
                selected: self.selected == Some(recording.recording_id),
            })
            .collect()
    }

    pub fn recording_presentation(&self) -> Option<RecordingPresentation> {
        let recording = self.selected_recording()?;
        let actions = recording
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let destination = action_destination(action);
                let state = action_receipt_state(action);
                let (state_label, state_detail) = action_state_copy(state, &destination);
                ActionCard {
                    index,
                    kind: match action.kind.as_str() {
                        "Todo" => ActionKind::Todo,
                        "Calendar" => ActionKind::CalendarEvent,
                        _ => ActionKind::Reminder,
                    },
                    operation: action_operation(&action.kind).into(),
                    payload_label: action_payload_label(&action.kind).into(),
                    payload: action.title.clone(),
                    due_at_ms: action.due_at_ms,
                    scheduled_for: action.due_at_ms.map(timestamp),
                    evidence_quote: action.evidence_quote.clone(),
                    destination,
                    state,
                    state_label,
                    state_detail,
                    can_decide: action.delivered_target.is_none()
                        && matches!(
                            action.decision.as_str(),
                            "waiting for review" | "pending" | ""
                        ),
                    remove_label: match state {
                        ActionReceiptState::Delivered => Some("Undo This Change".into()),
                        ActionReceiptState::AwaitingApproval
                        | ActionReceiptState::ApprovedPendingDelivery
                        | ActionReceiptState::Processing => Some("Remove This Action".into()),
                        ActionReceiptState::Skipped | ActionReceiptState::Undone => None,
                    },
                    edit_label: "Edit and Apply…".into(),
                }
            })
            .collect::<Vec<_>>();
        let transcript = if recording.segments.is_empty() {
            vec![TranscriptSegmentRow {
                ordinal: 0,
                time_label: "0:00".into(),
                text: recording.transcript.clone(),
                corrected: false,
            }]
        } else {
            recording
                .segments
                .iter()
                .map(|segment| TranscriptSegmentRow {
                    ordinal: segment.ordinal,
                    time_label: duration(segment.start_ms),
                    text: segment.displayed_text().to_owned(),
                    corrected: segment.replacement_text.is_some(),
                })
                .collect()
        };
        let activity = recording
            .workflow_runs
            .iter()
            .enumerate()
            .map(|(run_index, run)| ActivityReceipt {
                run_index,
                workflow: humanize_identifier(&run.workflow_id),
                triggered_by: requested_by_label(&run.requested_by),
                state: workflow_state_label(&run.status),
                result: workflow_result(run),
                confirmation: if run.confirmed
                    || recording.review_outcome.as_deref() == Some("confirmed")
                {
                    "Reviewed".into()
                } else {
                    "Not reviewed yet".into()
                },
                can_adjust: run.steps.iter().any(|step| step.undoable && !step.undone)
                    || run.status != "completed",
            })
            .collect::<Vec<_>>();
        let technical_trace = recording
            .workflow_runs
            .iter()
            .map(format_run_trace)
            .collect::<Vec<_>>()
            .join("\n\n");
        Some(RecordingPresentation {
            header: RecordingHeader {
                title: self.recording_title(recording),
                metadata: timestamp(recording.recorded_at_ms),
                review_status: recording
                    .review_outcome
                    .as_deref()
                    .map(review_status_label)
                    .unwrap_or_else(|| "Needs Review".into()),
                audio_available: recording.relative_audio_path.is_some(),
            },
            review_acknowledgement: recording.needs_review().then(|| ReviewAcknowledgement {
                heading: "Waiting for Your Review".into(),
                explanation: "The app has already applied any actions shown below. This recording stays here until you say everything looks right. Correct or undo anything first if the app got it wrong."
                    .into(),
                action_label: "Looks Good — Remove from Review".into(),
            }),
            summary: if recording.summary.trim().is_empty() {
                "A summary has not been generated yet.".into()
            } else {
                recording.summary.clone()
            },
            no_action_message: actions
                .is_empty()
                .then(|| "No actions were triggered for this recording.".into()),
            actions,
            transcript,
            activity,
            technical_trace,
        })
    }

    fn recording_title(&self, recording: &AppRecording) -> String {
        recording
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| generated_title(recording))
    }

    pub fn library_query(&self) -> &str {
        &self.library_query
    }

    pub fn workflow_rows(&self) -> Vec<WorkflowRow> {
        self.workflows
            .iter()
            .enumerate()
            .map(|(index, workflow)| WorkflowRow {
                index,
                name: workflow_display_name(workflow),
                description: workflow.description.clone(),
                enabled: workflow.enabled,
                status_label: if workflow.enabled { "On" } else { "Off" }.into(),
                toggle_label: if workflow.enabled {
                    "Turn Off"
                } else {
                    "Turn On"
                }
                .into(),
                run_label: workflow_run_label(workflow),
            })
            .collect()
    }

    pub fn settings_presentation(&self) -> SettingsPresentation {
        let permission_row = |permission: Permission, state: PermissionState| PermissionRow {
            permission,
            name: permission.label().into(),
            status: match state {
                PermissionState::Allowed => "Allowed".into(),
                PermissionState::Denied | PermissionState::Restricted => "Unavailable".into(),
                PermissionState::NotRequested => "Not set up".into(),
                PermissionState::Unknown => "Checking…".into(),
            },
            action_label: match state {
                PermissionState::NotRequested => Some("Set Up…".into()),
                PermissionState::Allowed
                | PermissionState::Denied
                | PermissionState::Restricted
                | PermissionState::Unknown => None,
            },
        };
        SettingsPresentation {
            recording_transfer_status: match self.settings.recording_transfer_state {
                Some(HealthState::Ready) => "Ready".into(),
                Some(HealthState::Starting) | None => "Starting…".into(),
                Some(HealthState::Degraded | HealthState::Stopped) => "Needs Attention".into(),
            },
            recording_transfer_detail: self.settings.recording_transfer_detail.clone(),
            recording_transfer_help: "Recorder apps can send audio through this always-on local service without selecting a compatibility device.".into(),
            source_name: self.settings.source_name.clone(),
            connection: self.background_control_presentation().source_status,
            source_setup_required: !self.settings.source_adapter_configured,
            can_release_source: self.settings.source_configured,
            source_options: self.settings.source_options.clone(),
            processing: friendly_processing_name(&self.settings.local_model),
            storage: self.settings.data_lake.clone(),
            open_at_login: if self.settings.starts_at_login {
                "On".into()
            } else {
                "Off".into()
            },
            permissions: vec![
                permission_row(Permission::Notifications, self.settings.notifications),
                permission_row(Permission::Calendar, self.settings.calendar),
                permission_row(Permission::Reminders, self.settings.reminders),
            ],
        }
    }

    pub fn transcript_text(&self, ordinal: u32) -> Option<String> {
        self.selected_recording()?
            .segments
            .iter()
            .find(|segment| segment.ordinal == ordinal)
            .map(|segment| segment.displayed_text().to_owned())
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn failures(&self) -> impl Iterator<Item = &str> {
        let selected = self.selected;
        self.snapshot
            .failures
            .iter()
            .filter(move |failure| {
                failure.recording_id.is_none() || failure.recording_id == selected
            })
            .map(|failure| failure.detail.as_str())
    }

    pub fn feedback(&self) -> Option<&str> {
        self.feedback.as_deref()
    }

    pub fn replace_snapshot(&mut self, snapshot: AppSnapshot) {
        self.snapshot = snapshot;
        self.ensure_selection();
    }

    pub fn replace_configuration(
        &mut self,
        automations: Vec<Automation>,
        workflows: Vec<WorkflowDefinition>,
        settings: Settings,
    ) {
        self.automations = automations;
        self.workflows = workflows;
        self.settings = settings;
    }

    pub fn replace_permission_states(
        &mut self,
        notifications: PermissionState,
        calendar: PermissionState,
        reminders: PermissionState,
    ) -> bool {
        let changed = self.settings.notifications != notifications
            || self.settings.calendar != calendar
            || self.settings.reminders != reminders;
        self.settings.notifications = notifications;
        self.settings.calendar = calendar;
        self.settings.reminders = reminders;
        changed
    }

    pub fn replace_segments(&mut self, recording_id: Uuid, segments: Vec<AppSegment>) {
        let Some(recording) = self
            .snapshot
            .recordings
            .iter_mut()
            .find(|recording| recording.recording_id == recording_id)
        else {
            return;
        };
        if let Some(segment) = segments.first() {
            recording.transcript_run_id = Some(segment.transcript_run_id);
        }
        if !segments.is_empty() {
            recording.transcript = segments
                .iter()
                .map(AppSegment::displayed_text)
                .collect::<Vec<_>>()
                .join(" ");
        }
        recording.segments = segments;
    }

    pub fn update(&mut self, intent: Intent) -> Result<Vec<Effect>, String> {
        self.feedback = None;
        match intent {
            Intent::Navigate(route) => {
                self.route = route;
                self.ensure_selection();
                Ok(self.selected_load_effect())
            }
            Intent::Search(query) => {
                self.route = Route::Library;
                self.library_query = query;
                self.ensure_selection();
                Ok(self.selected_load_effect())
            }
            Intent::SelectVisible(index) => {
                let id = self
                    .visible_recordings()
                    .get(index)
                    .map(|recording| recording.recording_id)
                    .ok_or_else(|| format!("No visible recording {}", index + 1))?;
                self.selected = Some(id);
                Ok(vec![Effect::LoadRecording(id)])
            }
            Intent::SelectRecording(id) => {
                if !self
                    .snapshot
                    .recordings
                    .iter()
                    .any(|recording| recording.recording_id == id)
                {
                    return Err(format!("Recording {id} is not in the current model"));
                }
                self.route = Route::Review;
                self.selected = Some(id);
                Ok(vec![Effect::LoadRecording(id)])
            }
            Intent::Refresh => Ok(vec![Effect::Reload]),
            Intent::Sync => {
                if !self.settings.source_adapter_configured {
                    return Err("Choose a recording source in Settings first".into());
                }
                self.feedback = Some("Sync requested".into());
                Ok(vec![Effect::SyncSource])
            }
            Intent::SelectSourceAdapter(index) => {
                let option = self
                    .settings
                    .source_options
                    .get(index)
                    .ok_or_else(|| format!("No recording source option {}", index + 1))?;
                self.feedback = Some(format!("Setting up {}", option.name));
                Ok(vec![Effect::ConfigureSourceAdapter {
                    adapter_id: option.id.clone(),
                }])
            }
            Intent::ReleaseSource => {
                if !self.settings.source_configured {
                    return Err("No recording source is currently paired".into());
                }
                self.feedback = Some(
                    "Source release requested. Adapter recovery data remains protected.".into(),
                );
                Ok(vec![Effect::ReleaseSource])
            }
            Intent::PlayAudio => {
                let recording = self.require_selected()?;
                let relative_path = recording
                    .relative_audio_path
                    .clone()
                    .ok_or_else(|| "Audio is not available yet".to_owned())?;
                Ok(vec![Effect::PlayAudio {
                    recording_id: recording.recording_id,
                    relative_path,
                    start_ms: None,
                }])
            }
            Intent::PlayTranscriptSegment(ordinal) => {
                let recording = self.require_selected()?;
                let start_ms = recording
                    .segments
                    .iter()
                    .find(|segment| segment.ordinal == ordinal)
                    .map(|segment| segment.start_ms)
                    .ok_or_else(|| format!("Transcript segment {ordinal} is not loaded"))?;
                let relative_path = recording
                    .relative_audio_path
                    .clone()
                    .ok_or_else(|| "Audio is not available yet".to_owned())?;
                Ok(vec![Effect::PlayAudio {
                    recording_id: recording.recording_id,
                    relative_path,
                    start_ms: Some(start_ms),
                }])
            }
            Intent::RenameRecording(title) => {
                let title = require_text(title, "A title is required")?;
                let recording_id = self.require_selected()?.recording_id;
                if let Some(recording) = self.recording_mut(recording_id) {
                    recording.title = Some(title.clone());
                }
                self.feedback = Some("Title updated".into());
                Ok(vec![Effect::SaveRecordingTitle {
                    recording_id,
                    title,
                }])
            }
            Intent::ConfirmProcessing => {
                let recording = self.require_selected()?;
                let id = recording.recording_id;
                let run_ids = recording
                    .workflow_runs
                    .iter()
                    .filter(|run| !run.confirmed)
                    .map(|run| run.run_id)
                    .collect::<Vec<_>>();
                if let Some(recording) = self.recording_mut(id) {
                    recording.review_outcome = Some("confirmed".into());
                    for run in &mut recording.workflow_runs {
                        run.confirmed = true;
                    }
                }
                self.feedback = Some("Removed from Review".into());
                self.ensure_selection();
                let mut effects = vec![Effect::ReviewRecording {
                    recording_id: id,
                    looks_right: true,
                    note: None,
                }];
                effects.extend(
                    run_ids
                        .into_iter()
                        .map(|run_id| Effect::ConfirmWorkflowRun { run_id }),
                );
                Ok(effects)
            }
            Intent::CorrectTranscript {
                ordinal,
                replacement,
            } => {
                let replacement = require_text(replacement, "Replacement text is required")?;
                let recording = self.require_selected()?;
                let segment = recording
                    .segments
                    .iter()
                    .find(|segment| segment.ordinal == ordinal)
                    .ok_or_else(|| format!("Transcript segment {ordinal} is not loaded"))?;
                let effect = Effect::CorrectTranscript {
                    recording_id: recording.recording_id,
                    transcript_run_id: segment.transcript_run_id,
                    ordinal,
                    replacement: replacement.clone(),
                };
                let recording_id = recording.recording_id;
                if let Some(recording) = self.recording_mut(recording_id) {
                    if let Some(segment) = recording
                        .segments
                        .iter_mut()
                        .find(|segment| segment.ordinal == ordinal)
                    {
                        segment.replacement_text = Some(replacement);
                    }
                    recording.transcript = recording
                        .segments
                        .iter()
                        .map(AppSegment::displayed_text)
                        .collect::<Vec<_>>()
                        .join(" ");
                }
                self.feedback = Some("Transcript correction saved as an overlay".into());
                Ok(vec![effect])
            }
            Intent::ApproveAction(index) => self.decide_action(index, true, None),
            Intent::RejectAction { index, reason } => {
                let reason = require_text(reason, "A rejection reason is required")?;
                self.decide_action(index, false, Some(reason))
            }
            Intent::RemoveAction(index) => self.remove_action(index),
            Intent::AddAction {
                kind,
                title,
                due_at_ms,
            } => self.add_action(kind, title, due_at_ms),
            Intent::EditAction {
                index,
                kind,
                title,
                due_at_ms,
            } => self.edit_action(index, kind, title, due_at_ms),
            Intent::RunWorkflow { index, selection } => {
                let workflow = self
                    .workflows
                    .get(index)
                    .ok_or_else(|| format!("No workflow {}", index + 1))?;
                if !workflow.enabled {
                    return Err(format!("{} is disabled", workflow.name));
                }
                let recording = self.require_selected()?;
                let transcript_run_id = recording
                    .transcript_run_id
                    .ok_or_else(|| "The transcript detail is not loaded yet".to_owned())?;
                let effect = Effect::RunWorkflow {
                    workflow_id: workflow.id.clone(),
                    resume_state_id: workflow
                        .states
                        .iter()
                        .find(|state| {
                            matches!(
                                state.selector,
                                WorkflowSelector::LocalModel { .. } | WorkflowSelector::User
                            )
                        })
                        .map_or_else(
                            || workflow.initial_state_id.clone(),
                            |state| state.id.clone(),
                        ),
                    recording_id: recording.recording_id,
                    transcript_run_id,
                    recorded_at_ms: recording.recorded_at_ms,
                    transcript: recording.transcript.clone(),
                    selection,
                };
                self.feedback = Some(format!("Running {}", workflow.name));
                Ok(vec![effect])
            }
            Intent::RunWorkflowWithLocalModel(index) => {
                let workflow = self
                    .workflows
                    .get(index)
                    .ok_or_else(|| format!("No automation {}", index + 1))?;
                if !workflow.enabled {
                    return Err(format!("{} is turned off", workflow_display_name(workflow)));
                }
                let resume_state_id = workflow
                    .states
                    .iter()
                    .find(|state| matches!(state.selector, WorkflowSelector::LocalModel { .. }))
                    .map_or_else(
                        || workflow.initial_state_id.clone(),
                        |state| state.id.clone(),
                    );
                let recording = self.require_selected()?;
                let transcript_run_id = recording
                    .transcript_run_id
                    .ok_or_else(|| "The transcript is not ready yet".to_owned())?;
                let effect = Effect::RunWorkflowWithLocalModel {
                    workflow_id: workflow.id.clone(),
                    resume_state_id,
                    recording_id: recording.recording_id,
                    transcript_run_id,
                    recorded_at_ms: recording.recorded_at_ms,
                    transcript: recording.transcript.clone(),
                };
                self.feedback = Some(format!(
                    "Checking this recording with {}",
                    workflow_display_name(workflow)
                ));
                Ok(vec![effect])
            }
            Intent::RouteWithLocalModel => {
                let recording = self.require_selected()?;
                let transcript_run_id = recording
                    .transcript_run_id
                    .ok_or_else(|| "The transcript detail is not loaded yet".to_owned())?;
                let effect = Effect::RouteWithLocalModel {
                    recording_id: recording.recording_id,
                    transcript_run_id,
                    recorded_at_ms: recording.recorded_at_ms,
                    transcript: recording.transcript.clone(),
                };
                self.feedback = Some("Checking this recording again for actions".into());
                Ok(vec![effect])
            }
            Intent::AdjustWorkflowRun {
                run_index,
                state_id,
                transition_id,
                outputs,
            } => {
                let state_id = require_text(state_id, "A source state is required")?;
                let transition_id = require_text(transition_id, "A transition is required")?;
                let run = self
                    .require_selected()?
                    .workflow_runs
                    .get(run_index)
                    .ok_or_else(|| format!("No workflow run {}", run_index + 1))?;
                let effect = Effect::AdjustWorkflowRun {
                    run_id: run.run_id,
                    state_id,
                    selection: WorkflowSelection {
                        transition_id,
                        outputs,
                    },
                };
                self.feedback = Some("Workflow route adjusted and queued to resume".into());
                Ok(vec![effect])
            }
            Intent::UndoWorkflowStep {
                run_index,
                step_index,
            } => {
                let run = self
                    .require_selected()?
                    .workflow_runs
                    .get(run_index)
                    .ok_or_else(|| format!("No workflow run {}", run_index + 1))?;
                let step = run
                    .steps
                    .get(step_index)
                    .ok_or_else(|| format!("No step {}", step_index + 1))?;
                if !step.undoable || step.undone {
                    return Err("That workflow effect is not currently undoable".into());
                }
                let effect = Effect::UndoWorkflowStep {
                    run_id: run.run_id,
                    transition_id: step
                        .transition_id
                        .clone()
                        .ok_or_else(|| "That trace step has no transition".to_owned())?,
                };
                self.feedback = Some("Undo queued".into());
                Ok(vec![effect])
            }
            Intent::UndoLatestWorkflowEffect(run_index) => {
                let run = self
                    .require_selected()?
                    .workflow_runs
                    .get(run_index)
                    .ok_or_else(|| format!("No workflow run {}", run_index + 1))?;
                let (step_index, _) = run
                    .steps
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, step)| step.undoable && !step.undone)
                    .ok_or_else(|| "This run has no reversible effect to undo".to_owned())?;
                self.update(Intent::UndoWorkflowStep {
                    run_index,
                    step_index,
                })
            }
            Intent::ToggleAutomation(index) => {
                let automation = self
                    .automations
                    .get_mut(index)
                    .ok_or_else(|| format!("No automation {}", index + 1))?;
                automation.enabled = !automation.enabled;
                let updated = automation.clone();
                self.feedback = Some(format!(
                    "{} {}",
                    updated.name,
                    if updated.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ));
                Ok(vec![Effect::SaveAutomation(updated)])
            }
            Intent::CreateAutomation(automation) => {
                if automation.id.trim().is_empty() || automation.name.trim().is_empty() {
                    return Err("Automation id and name are required".into());
                }
                self.automations.push(automation.clone());
                self.feedback = Some(format!("Created {}", automation.name));
                Ok(vec![Effect::SaveAutomation(automation)])
            }
            Intent::ToggleWorkflow(index) => {
                let workflow = self
                    .workflows
                    .get_mut(index)
                    .ok_or_else(|| format!("No workflow {}", index + 1))?;
                workflow.enabled = !workflow.enabled;
                let updated = workflow.clone();
                self.feedback = Some(format!(
                    "{} {}",
                    updated.name,
                    if updated.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ));
                Ok(vec![Effect::SaveWorkflow(updated)])
            }
            Intent::CreateWorkflow(workflow) => {
                validate_workflow(&workflow)?;
                if self.workflows.iter().any(|item| item.id == workflow.id) {
                    return Err(format!("Workflow '{}' already exists", workflow.id));
                }
                self.workflows.push(workflow.clone());
                self.feedback = Some(format!("Created {}", workflow.name));
                Ok(vec![Effect::SaveWorkflow(workflow)])
            }
            Intent::CreateWorkflowFromDraft(draft) => {
                let workflow = workflow_from_draft(draft, &self.workflows)?;
                self.workflows.push(workflow.clone());
                self.feedback = Some(format!("Created {}", workflow.name));
                Ok(vec![Effect::SaveWorkflow(workflow)])
            }
            Intent::DeleteWorkflow(index) => {
                let workflow = self
                    .workflows
                    .get(index)
                    .cloned()
                    .ok_or_else(|| format!("No workflow {}", index + 1))?;
                self.workflows.remove(index);
                self.feedback = Some(format!("Deleted {}", workflow.name));
                Ok(vec![Effect::DeleteWorkflow(workflow.id)])
            }
            Intent::RequestPermission(permission) => {
                self.feedback = Some(format!("Requesting {} access", permission.label()));
                Ok(vec![Effect::RequestPermission(permission)])
            }
        }
    }

    fn decide_action(
        &mut self,
        index: usize,
        approved: bool,
        reason: Option<String>,
    ) -> Result<Vec<Effect>, String> {
        let recording_id = self.require_selected()?.recording_id;
        let action = self
            .recording_mut(recording_id)
            .and_then(|recording| recording.actions.get_mut(index))
            .ok_or_else(|| format!("No action {} on the selected recording", index + 1))?;
        if let Some(target) = &action.delivered_target {
            return Err(format!("This action was already delivered to {target}"));
        }
        if !matches!(
            action.decision.as_str(),
            "waiting for review" | "pending" | ""
        ) {
            return Err(format!("This action is already {}", action.decision));
        }
        action.decision = if approved { "approved" } else { "not added" }.into();
        let action_id = action.action_id;
        self.feedback = Some(if approved {
            "Action approved".into()
        } else {
            "Action rejected".into()
        });
        Ok(vec![Effect::DecideAction {
            action_id,
            approved,
            reason,
        }])
    }

    fn remove_action(&mut self, index: usize) -> Result<Vec<Effect>, String> {
        let recording_id = self.require_selected()?.recording_id;
        let action = self
            .recording_mut(recording_id)
            .and_then(|recording| recording.actions.get_mut(index))
            .ok_or_else(|| format!("No action {} on the selected recording", index + 1))?;
        let action_id = action.action_id;
        if action.delivered_target.is_some() {
            action.decision = "removed".into();
            action.delivered_target = None;
            self.feedback = Some("Undoing the change on your Mac".into());
            return Ok(vec![Effect::UndoAction { action_id }]);
        }
        if matches!(action.decision.as_str(), "not added" | "removed") {
            return Err("This action has already been removed".into());
        }
        action.decision = "not added".into();
        self.feedback = Some("Action removed".into());
        Ok(vec![Effect::DecideAction {
            action_id,
            approved: false,
            reason: Some("Removed in Captain’s Log before delivery".into()),
        }])
    }

    pub fn action_editor(&self, index: Option<usize>) -> Result<ActionEditor, String> {
        let Some(index) = index else {
            return Ok(ActionEditor {
                heading: "Add an action".into(),
                explanation: "Choose exactly what the app should add. It will be applied immediately and remain undoable from this recording.".into(),
                submit_label: "Add to My Mac".into(),
                kind: ActionKind::Reminder,
                title: String::new(),
                due_at_ms: None,
            });
        };
        let action = self
            .recording_presentation()
            .and_then(|detail| detail.actions.get(index).cloned())
            .ok_or_else(|| format!("No action {} on the selected recording", index + 1))?;
        Ok(ActionEditor {
            heading: "Change this action".into(),
            explanation: "Change what the app did. If the item already exists on your Mac, The app will update it in place.".into(),
            submit_label: "Apply Changes".into(),
            kind: action.kind,
            title: action.payload,
            due_at_ms: action.due_at_ms,
        })
    }

    fn add_action(
        &mut self,
        kind: ActionKind,
        title: String,
        due_at_ms: Option<i64>,
    ) -> Result<Vec<Effect>, String> {
        let title = require_text(title, "Action text is required")?;
        if title.chars().count() > 300 {
            return Err("Action text must be 300 characters or fewer".into());
        }
        if kind == ActionKind::CalendarEvent && due_at_ms.is_none() {
            return Err("Calendar events require a date and time".into());
        }
        let recording = self.require_selected()?;
        let recording_id = recording.recording_id;
        let transcript_run_id = recording
            .transcript_run_id
            .ok_or_else(|| "The transcript detail is not loaded yet".to_owned())?;
        let action_id = Uuid::new_v4();
        let display_kind = match kind {
            ActionKind::Todo => "Todo",
            ActionKind::Reminder => "Reminder",
            ActionKind::CalendarEvent => "Calendar",
        };
        self.recording_mut(recording_id)
            .expect("selected recording exists")
            .actions
            .push(AppAction {
                action_id,
                kind: display_kind.into(),
                title: title.clone(),
                due_at_ms,
                evidence_quote: None,
                decision: "approved".into(),
                delivered_target: None,
            });
        self.feedback = Some("Adding your action to this Mac".into());
        Ok(vec![Effect::CreateAction {
            action_id,
            recording_id,
            transcript_run_id,
            kind,
            title,
            due_at_ms,
        }])
    }

    fn edit_action(
        &mut self,
        index: usize,
        kind: ActionKind,
        title: String,
        due_at_ms: Option<i64>,
    ) -> Result<Vec<Effect>, String> {
        let title = require_text(title, "Action text is required")?;
        if title.chars().count() > 300 {
            return Err("Action text must be 300 characters or fewer".into());
        }
        let recording_id = self.require_selected()?.recording_id;
        let action = self
            .recording_mut(recording_id)
            .and_then(|recording| recording.actions.get_mut(index))
            .ok_or_else(|| format!("No action {} on the selected recording", index + 1))?;
        if kind == ActionKind::CalendarEvent && due_at_ms.is_none() {
            return Err("Calendar events require a date and time".into());
        }
        action.kind = match kind {
            ActionKind::Todo => "Todo",
            ActionKind::Reminder => "Reminder",
            ActionKind::CalendarEvent => "Calendar",
        }
        .into();
        action.title = title.clone();
        action.due_at_ms = due_at_ms;
        let action_id = action.action_id;
        self.feedback = Some("Applying your corrected action on this Mac".into());
        Ok(vec![Effect::ReviseAction {
            action_id,
            kind,
            title,
            due_at_ms,
        }])
    }

    fn require_selected(&self) -> Result<&AppRecording, String> {
        self.selected_recording()
            .ok_or_else(|| "No recording is selected".to_owned())
    }

    fn recording_mut(&mut self, id: Uuid) -> Option<&mut AppRecording> {
        self.snapshot
            .recordings
            .iter_mut()
            .find(|recording| recording.recording_id == id)
    }

    fn selected_load_effect(&self) -> Vec<Effect> {
        self.selected
            .map(Effect::LoadRecording)
            .into_iter()
            .collect()
    }

    fn ensure_selection(&mut self) {
        let selected_is_visible = self.selected.is_some_and(|selected| {
            self.visible_recordings()
                .iter()
                .any(|item| item.recording_id == selected)
        });
        if !selected_is_visible {
            self.selected = self
                .visible_recordings()
                .first()
                .map(|recording| recording.recording_id);
        }
    }
}

fn workflow_display_name(workflow: &WorkflowDefinition) -> String {
    match workflow.id.as_str() {
        "todos" => "To-dos".into(),
        _ => workflow.name.clone(),
    }
}

fn workflow_run_label(workflow: &WorkflowDefinition) -> String {
    match workflow.id.as_str() {
        "reminders" => "Check for a Reminder".into(),
        "todos" => "Check for a To-do".into(),
        "calendar" => "Check for a Calendar Event".into(),
        _ => format!("Run {}", workflow_display_name(workflow)),
    }
}

fn friendly_processing_name(model: &str) -> String {
    if model.starts_with("mistral-small3.2") {
        "On this Mac with Mistral Small 3.2 (24B)".into()
    } else if model.trim().is_empty() {
        "On this Mac".into()
    } else {
        format!("On this Mac with {model}")
    }
}

fn review_status_label(status: &str) -> String {
    match status {
        "confirmed" => "Reviewed".into(),
        "needs_attention" => "Needs Review".into(),
        value => sentence_case(value),
    }
}

fn action_operation(kind: &str) -> &'static str {
    match kind {
        "Todo" => "Create a to-do",
        "Reminder" => "Create a reminder",
        "Calendar" => "Add a calendar event",
        _ => "Run an integration action",
    }
}

fn action_payload_label(kind: &str) -> &'static str {
    match kind {
        "Todo" => "To-do text",
        "Reminder" => "Reminder text",
        "Calendar" => "Event title",
        _ => "Action input",
    }
}

fn action_destination(action: &AppAction) -> String {
    action.delivered_target.as_ref().map_or_else(
        || match action.kind.as_str() {
            "Todo" | "Reminder" => "Apple Reminders".into(),
            "Calendar" => "Apple Calendar".into(),
            _ => "Configured destination".into(),
        },
        |target| match target.as_str() {
            "Reminders" => "Apple Reminders".into(),
            "Calendar" => "Apple Calendar".into(),
            _ => target.clone(),
        },
    )
}

fn action_receipt_state(action: &AppAction) -> ActionReceiptState {
    if action.delivered_target.is_some() {
        return ActionReceiptState::Delivered;
    }
    match action.decision.as_str() {
        "waiting for review" | "pending" | "" => ActionReceiptState::AwaitingApproval,
        "approved" => ActionReceiptState::ApprovedPendingDelivery,
        "not added" => ActionReceiptState::Skipped,
        "removed" => ActionReceiptState::Undone,
        _ => ActionReceiptState::Processing,
    }
}

fn action_state_copy(state: ActionReceiptState, destination: &str) -> (String, String) {
    match state {
        ActionReceiptState::AwaitingApproval => (
            "Awaiting your approval".into(),
            "No external side effect has run.".into(),
        ),
        ActionReceiptState::ApprovedPendingDelivery => (
            "Waiting to be added".into(),
            format!("The app has not confirmed that this reached {destination} yet."),
        ),
        ActionReceiptState::Delivered => (
            "Completed".into(),
            format!("A matching item was created in {destination}."),
        ),
        ActionReceiptState::Skipped => ("Skipped".into(), "No external item was created.".into()),
        ActionReceiptState::Undone => (
            "Undone".into(),
            "The previously created side effect was compensated.".into(),
        ),
        ActionReceiptState::Processing => (
            "Processing".into(),
            "The app has not recorded a final delivery outcome.".into(),
        ),
    }
}

fn requested_by_label(requested_by: &str) -> String {
    match requested_by {
        "user" | "manual" => "You ran it".into(),
        "local_model" | "model" => "The app checked it".into(),
        "phrase" | "deterministic" => "Matched automatically".into(),
        "automatic" | "ingest" | "" => "When the recording arrived".into(),
        value => format!("Started by {}", humanize_identifier(value)),
    }
}

fn workflow_state_label(status: &str) -> String {
    match status {
        "complete" | "completed" => "Completed".into(),
        "awaiting_user" | "waiting_for_user" => "Waiting for you".into(),
        "failed" | "error" => "Failed".into(),
        value => sentence_case(value),
    }
}

fn workflow_result(run: &crate::read_model::AppWorkflowRun) -> String {
    if let Some(receipt) = run
        .steps
        .iter()
        .rev()
        .find_map(|step| step.receipt.as_deref())
    {
        if receipt.starts_with("queued Mac action ") {
            return "The app chose to add an item. Its delivery appears in Actions Taken.".into();
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(receipt)
            && let Some(summary) = value.get("summary").and_then(serde_json::Value::as_str)
        {
            return value
                .get("destination")
                .and_then(serde_json::Value::as_str)
                .map_or_else(
                    || summary.to_owned(),
                    |destination| format!("{summary} in {destination}"),
                );
        }
        return receipt.to_owned();
    }

    match run
        .steps
        .iter()
        .rev()
        .find_map(|step| step.transition_id.as_deref())
    {
        Some("opt_out") => "No hooks ran because the transcript explicitly opted out.".into(),
        Some("too_long") => {
            "No hooks ran automatically because this was a long conversational recording.".into()
        }
        Some("ignore" | "not_candidate") => "Nothing needed to be added.".into(),
        Some("create" | "create_reminder" | "create_todo" | "create_calendar") => {
            "The app chose an action, but its result is not available yet.".into()
        }
        _ if matches!(run.status.as_str(), "complete" | "completed") => {
            "Finished without adding anything.".into()
        }
        _ => "The result is not available yet.".into(),
    }
}

fn generated_title(recording: &AppRecording) -> String {
    let source = if recording.summary.trim().is_empty() {
        &recording.transcript
    } else {
        &recording.summary
    };
    let cleaned = source
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\''));
    let candidate = cleaned
        .split(['.', '!', '?', ';', '\n'])
        .next()
        .unwrap_or_default();
    let words = candidate.split_whitespace().take(9).collect::<Vec<_>>();
    if words.is_empty() {
        format!("Recording from {}", timestamp(recording.recorded_at_ms))
    } else {
        let mut title = words.join(" ").trim_end_matches(',').to_owned();
        if title.chars().count() > 64 {
            title = title.chars().take(63).collect::<String>();
            title.push('…');
        }
        let mut chars = title.chars();
        chars.next().map_or(title.clone(), |first| {
            first.to_uppercase().collect::<String>() + chars.as_str()
        })
    }
}

fn timestamp(milliseconds: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(milliseconds)
        .map(|value| {
            value
                .format("%e %b %Y, %H:%M")
                .to_string()
                .trim()
                .to_owned()
        })
        .unwrap_or_else(|| "Unknown time".into())
}

fn duration(milliseconds: u64) -> String {
    format!(
        "{}:{:02}",
        milliseconds / 60_000,
        (milliseconds / 1_000) % 60
    )
}

fn sentence_case(value: &str) -> String {
    let value = value.replace('_', " ");
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn humanize_identifier(value: &str) -> String {
    sentence_case(&value.replace(['-', '_'], " "))
}

fn format_run_trace(run: &crate::read_model::AppWorkflowRun) -> String {
    let mut lines = vec![format!("{} — {}", run.workflow_id, run.status)];
    for step in &run.steps {
        lines.push(format!(
            "{}: {} → {}",
            step.state_id,
            step.transition_id
                .as_deref()
                .unwrap_or("awaiting human selection"),
            step.destination_state_id.as_deref().unwrap_or("—")
        ));
        if !step.outputs.is_empty() {
            lines.push(
                step.outputs
                    .iter()
                    .map(|(key, value)| format!("  {key}: {value}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    lines.join("\n")
}

fn require_text(value: String, error: &str) -> Result<String, String> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        Err(error.into())
    } else {
        Ok(value)
    }
}

fn workflow_from_draft(
    draft: AutomationDraft,
    existing: &[WorkflowDefinition],
) -> Result<WorkflowDefinition, String> {
    let name = require_text(draft.name, "Give the automation a name")?;
    let description = require_text(draft.description, "Describe when the automation should run")?;
    let target = match draft.connection {
        AutomationConnection::Shortcut | AutomationConnection::Webhook => {
            require_text(draft.target, "Choose what the automation should run")?
        }
        _ => draft.target.trim().to_owned(),
    };
    let mut id = slug(&name);
    if id.is_empty() {
        id = "automation".into();
    }
    let base = id.clone();
    let mut suffix = 2;
    while existing.iter().any(|workflow| workflow.id == id) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    let (adapter, output_fields, fill_instruction) = match draft.connection {
        AutomationConnection::Notes => (
            WorkflowAdapter::Document {
                kind: DocumentKind::Notes,
                title_field: "note_title".into(),
                content_field: Some("note_body".into()),
                target_field: Some("notes_folder".into()),
            },
            vec![
                natural_language_field("note_title", "A concise title for the note", 200, true),
                natural_language_field("note_body", "The complete note text", 4_000, true),
                natural_language_field(
                    "notes_folder",
                    "An existing Apple Notes folder, or empty for the default folder",
                    200,
                    false,
                ),
            ],
            "Fill the note title and body from the recording. Use only information present in the transcript. The folder is optional.".into(),
        ),
        AutomationConnection::MailDraft => (
            WorkflowAdapter::Document {
                kind: DocumentKind::MailDraft,
                title_field: "email_subject".into(),
                content_field: Some("email_body".into()),
                target_field: Some("email_recipient".into()),
            },
            vec![
                natural_language_field("email_subject", "The email subject", 300, true),
                natural_language_field("email_body", "The complete email draft body", 8_000, true),
                natural_language_field(
                    "email_recipient",
                    "A recipient email address only when stated or unambiguous, otherwise empty",
                    320,
                    false,
                ),
            ],
            "Draft an email from the recording. Do not invent facts or a recipient. The app saves a draft and never sends it.".into(),
        ),
        AutomationConnection::Contacts => (
            WorkflowAdapter::Document {
                kind: DocumentKind::Contacts,
                title_field: "contact_name".into(),
                content_field: Some("contact_note".into()),
                target_field: Some("contact_method".into()),
            },
            vec![
                natural_language_field("contact_name", "The person's full name", 300, true),
                natural_language_field(
                    "contact_note",
                    "Useful context to store in the contact's notes",
                    2_000,
                    false,
                ),
                natural_language_field(
                    "contact_method",
                    "One email address or phone number from the recording, otherwise empty",
                    320,
                    false,
                ),
            ],
            "Extract a contact only from details in the recording. Never invent an email address or phone number.".into(),
        ),
        AutomationConnection::TextFile => (
            WorkflowAdapter::Document {
                kind: DocumentKind::TextFile,
                title_field: "file_title".into(),
                content_field: Some("file_content".into()),
                target_field: Some("file_name".into()),
            },
            vec![
                natural_language_field("file_title", "A concise title", 200, true),
                natural_language_field("file_content", "The complete text to save", 12_000, true),
                natural_language_field(
                    "file_name",
                    "A short filename without a directory or extension",
                    120,
                    false,
                ),
            ],
            "Create a faithful plain-text document from the recording. Do not invent facts.".into(),
        ),
        AutomationConnection::Shortcut => (
            WorkflowAdapter::Shortcut {
                target: target.clone(),
            },
            vec![],
            String::new(),
        ),
        AutomationConnection::Webhook => (
            WorkflowAdapter::Webhook {
                target: target.clone(),
            },
            vec![],
            String::new(),
        ),
    };
    let undo_adapter = match draft.connection {
        AutomationConnection::Notes
        | AutomationConnection::MailDraft
        | AutomationConnection::Contacts
        | AutomationConnection::TextFile => Some(WorkflowAdapter::DocumentUndo),
        AutomationConnection::Shortcut | AutomationConnection::Webhook => draft
            .undo_target
            .filter(|target| !target.trim().is_empty())
            .map(|target| match draft.connection {
                AutomationConnection::Shortcut => WorkflowAdapter::Shortcut { target },
                AutomationConnection::Webhook => WorkflowAdapter::Webhook { target },
                _ => unreachable!("native integrations use DocumentUndo"),
            }),
    };
    let run = WorkflowTransition {
        id: "create".into(),
        description: format!("Run {name}"),
        destination_state_id: "done".into(),
        output_fields,
        action: WorkflowAction::Effect {
            adapter,
            undo_adapter,
        },
    };
    let relevance_selector = match draft.timing {
        AutomationTiming::EveryRecording => WorkflowSelector::Deterministic {
            transition_id: "relevant".into(),
        },
        AutomationTiming::WhenRelevant => WorkflowSelector::LocalModel {
            instruction: format!(
                "Choose relevant only when this recording is relevant to the following automation. Otherwise choose ignore. Automation: {name}. Purpose: {description}"
            ),
        },
    };
    let mut relevance_transitions = vec![WorkflowTransition {
        id: "relevant".into(),
        description: "Continue because this recording is relevant".into(),
        destination_state_id: "prepare".into(),
        output_fields: vec![],
        action: WorkflowAction::None,
    }];
    if draft.timing == AutomationTiming::WhenRelevant {
        relevance_transitions.push(WorkflowTransition {
            id: "ignore".into(),
            description: "Do nothing because this recording is not relevant".into(),
            destination_state_id: "done".into(),
            output_fields: vec![],
            action: WorkflowAction::None,
        });
    }
    Ok(WorkflowDefinition {
        id,
        name,
        description,
        enabled: true,
        subscriptions: vec!["transcript_completed".into()],
        initial_state_id: "message_ingested".into(),
        states: vec![
            WorkflowState {
                id: "message_ingested".into(),
                description: "A new recording arrived".into(),
                selector: relevance_selector,
                transitions: relevance_transitions,
            },
            WorkflowState {
                id: "prepare".into(),
                description: "Prepare the integration payload".into(),
                selector: if fill_instruction.is_empty() {
                    WorkflowSelector::Deterministic {
                        transition_id: "create".into(),
                    }
                } else {
                    WorkflowSelector::LocalModel {
                        instruction: fill_instruction,
                    }
                },
                transitions: vec![run],
            },
            WorkflowState {
                id: "done".into(),
                description: "Automation finished".into(),
                selector: WorkflowSelector::Terminal {
                    outcome: "complete".into(),
                },
                transitions: vec![],
            },
        ],
    })
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

fn slug(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn validate_workflow(workflow: &WorkflowDefinition) -> Result<(), String> {
    if workflow.id.is_empty()
        || !workflow
            .id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("Workflow id may contain only letters, numbers, '-' and '_'".into());
    }
    if workflow.name.trim().is_empty()
        || workflow.initial_state_id.trim().is_empty()
        || workflow.states.is_empty()
    {
        return Err("Workflow name, start node, and at least one node are required".into());
    }
    if workflow.initial_state_id != "message_ingested" {
        return Err("Workflow start node must be message_ingested".into());
    }
    if !workflow
        .states
        .iter()
        .any(|state| state.id == workflow.initial_state_id)
    {
        return Err("Workflow start node does not exist".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_model::{AppFailure, AppWorkflowRun, AppWorkflowStep, fixture_snapshot};
    use personal_audio_lake_contracts::{WorkflowAction, WorkflowState, WorkflowTransition};

    fn model() -> ViewModel {
        ViewModel::new(
            fixture_snapshot(),
            Vec::new(),
            Vec::new(),
            Settings::default(),
        )
    }

    fn todo_workflow() -> WorkflowDefinition {
        WorkflowDefinition {
            id: "todos".into(),
            name: "Todos".into(),
            description: "Adds tasks automatically".into(),
            enabled: true,
            subscriptions: vec!["transcript_completed".into()],
            initial_state_id: "message_ingested".into(),
            states: vec![
                WorkflowState {
                    id: "message_ingested".into(),
                    description: "Check for a task".into(),
                    selector: WorkflowSelector::LocalModel {
                        instruction: "Choose create or ignore".into(),
                    },
                    transitions: vec![WorkflowTransition {
                        id: "ignore".into(),
                        description: "Nothing to add".into(),
                        destination_state_id: "done".into(),
                        output_fields: vec![],
                        action: WorkflowAction::None,
                    }],
                },
                WorkflowState {
                    id: "done".into(),
                    description: "Finished".into(),
                    selector: WorkflowSelector::Terminal {
                        outcome: "complete".into(),
                    },
                    transitions: vec![],
                },
            ],
        }
    }

    #[test]
    fn native_automation_draft_becomes_a_bounded_undoable_state_machine() {
        let mut model = model();
        let effects = model
            .update(Intent::CreateWorkflowFromDraft(AutomationDraft {
                name: "Send Expenses".into(),
                description: "Send recordings that mention a business expense".into(),
                timing: AutomationTiming::WhenRelevant,
                connection: AutomationConnection::Shortcut,
                target: "File Expense".into(),
                undo_target: Some("Remove Filed Expense".into()),
            }))
            .unwrap();
        let Effect::SaveWorkflow(workflow) = &effects[0] else {
            panic!("expected a saved workflow")
        };
        assert_eq!(workflow.id, "send-expenses");
        assert!(matches!(
            workflow.states[0].selector,
            WorkflowSelector::LocalModel { .. }
        ));
        assert_eq!(
            workflow.states[0]
                .transitions
                .iter()
                .map(|transition| transition.id.as_str())
                .collect::<Vec<_>>(),
            vec!["relevant", "ignore"]
        );
        assert!(matches!(
            workflow.states[1].transitions[0].action,
            WorkflowAction::Effect {
                adapter: WorkflowAdapter::Shortcut { .. },
                undo_adapter: Some(WorkflowAdapter::Shortcut { .. })
            }
        ));
    }

    #[test]
    fn native_integration_catalog_builds_typed_reversible_workflows() {
        let expected = [
            (AutomationConnection::Notes, DocumentKind::Notes),
            (AutomationConnection::MailDraft, DocumentKind::MailDraft),
            (AutomationConnection::Contacts, DocumentKind::Contacts),
            (AutomationConnection::TextFile, DocumentKind::TextFile),
        ];
        for (connection, expected_kind) in expected {
            let workflow = workflow_from_draft(
                AutomationDraft {
                    name: format!("{:?} capture", connection),
                    description: "Create an item when relevant".into(),
                    timing: AutomationTiming::WhenRelevant,
                    connection,
                    target: String::new(),
                    undo_target: None,
                },
                &[],
            )
            .unwrap();
            assert_eq!(workflow.states.len(), 3);
            assert!(matches!(
                workflow.states[1].selector,
                WorkflowSelector::LocalModel { .. }
            ));
            let transition = &workflow.states[1].transitions[0];
            assert!(!transition.output_fields.is_empty());
            assert!(transition.output_fields.iter().any(|field| field.required));
            assert!(matches!(
                transition.action,
                WorkflowAction::Effect {
                    adapter: WorkflowAdapter::Document { kind, .. },
                    undo_adapter: Some(WorkflowAdapter::DocumentUndo),
                } if kind == expected_kind
            ));
        }
    }

    #[test]
    fn integration_catalog_explains_shortcuts_as_the_extensibility_bridge() {
        let catalog = ViewModel::integration_catalog();
        assert!(catalog.iter().any(|item| {
            item.connection == AutomationConnection::Shortcut
                && item.description.contains("Messages")
                && item.description.contains("Home")
        }));
        assert!(catalog.iter().any(|item| {
            item.connection == AutomationConnection::MailDraft
                && item.description.contains("never sends")
        }));
    }

    #[test]
    fn configuration_refresh_recovers_normal_ui_after_an_empty_startup() {
        let mut model = model();
        assert!(model.workflow_rows().is_empty());
        let settings = Settings {
            recording_transfer_state: Some(HealthState::Ready),
            source_name: "Portable recorder".into(),
            source_state: Some(SourceState::Ready),
            source_adapter_configured: true,
            source_configured: true,
            data_lake: "/private/lake".into(),
            notifications: PermissionState::Allowed,
            ..Settings::default()
        };

        model.replace_configuration(Vec::new(), vec![todo_workflow()], settings);

        let row = &model.workflow_rows()[0];
        assert_eq!(row.name, "To-dos");
        assert_eq!(row.status_label, "On");
        assert_eq!(row.run_label, "Check for a To-do");
        let settings = model.settings_presentation();
        assert_eq!(settings.connection, "Portable recorder · Ready");
        assert!(settings.can_release_source);
        assert!(settings.processing.starts_with("On this Mac"));
        assert_eq!(settings.permissions[0].action_label, None);
    }

    #[test]
    fn release_is_available_only_for_a_paired_recorder() {
        let mut unpaired = model();
        assert!(unpaired.update(Intent::ReleaseSource).is_err());

        let mut paired = ViewModel::new(
            fixture_snapshot(),
            Vec::new(),
            Vec::new(),
            Settings {
                source_state: Some(SourceState::Bound),
                source_configured: true,
                ..Settings::default()
            },
        );
        assert_eq!(
            paired.update(Intent::ReleaseSource).unwrap(),
            vec![Effect::ReleaseSource]
        );
    }

    #[test]
    fn background_control_presentation_keeps_navigation_and_source_state_unambiguous() {
        let mut model = model();
        let menu = model.background_control_presentation();
        assert_eq!(menu.primary_route, Route::Review);
        assert_eq!(menu.primary_action, "Review 2 Recordings");
        assert_eq!(menu.source_status, "Recorder App Transfer · Starting");
        assert_eq!(menu.sync_action, "Add Compatibility Device…");
        assert!(menu.can_sync);
        assert!(menu.source_setup_required);
        assert_eq!(menu.operational_state, BackgroundOperationalState::Active);

        model.update(Intent::ConfirmProcessing).unwrap();
        model.update(Intent::ConfirmProcessing).unwrap();
        let caught_up = model.background_control_presentation();
        assert_eq!(caught_up.primary_route, Route::Library);
        assert_eq!(caught_up.primary_action, "Open Captain’s Log");

        model.replace_configuration(
            Vec::new(),
            Vec::new(),
            Settings {
                recording_transfer_state: Some(HealthState::Ready),
                source_name: "Folder inbox".into(),
                source_state: Some(SourceState::Syncing),
                source_adapter_configured: true,
                source_configured: true,
                ..Settings::default()
            },
        );
        let menu = model.background_control_presentation();
        assert_eq!(menu.source_status, "Folder inbox · Syncing");
        assert_eq!(menu.sync_action, "Syncing…");
        assert!(!menu.can_sync);
        assert_eq!(menu.operational_state, BackgroundOperationalState::Active);
    }

    #[test]
    fn clean_install_requires_an_explicit_source_choice() {
        let mut model = ViewModel::empty(Settings {
            recording_transfer_state: Some(HealthState::Ready),
            recording_transfer_detail: "encrypted local recording transfer ready".into(),
            source_options: vec![SourceOption {
                id: "fixture-adapter".into(),
                name: "Fixture recorder".into(),
                description: "A test-only source".into(),
                selected: false,
            }],
            ..Settings::default()
        });

        let background = model.background_control_presentation();
        assert_eq!(background.source_status, "Recorder App Transfer · Ready");
        assert_eq!(background.sync_action, "Add Compatibility Device…");
        let settings = model.settings_presentation();
        assert_eq!(settings.recording_transfer_status, "Ready");
        assert!(
            settings
                .recording_transfer_detail
                .contains("recording transfer ready")
        );
        assert!(model.update(Intent::Sync).is_err());
        assert_eq!(
            model.update(Intent::SelectSourceAdapter(0)).unwrap(),
            vec![Effect::ConfigureSourceAdapter {
                adapter_id: "fixture-adapter".into(),
            }]
        );
        assert!(model.update(Intent::SelectSourceAdapter(1)).is_err());
    }

    #[test]
    fn denied_permissions_are_contained_without_a_settings_recovery_action() {
        let model = ViewModel::new(
            fixture_snapshot(),
            Vec::new(),
            Vec::new(),
            Settings {
                notifications: PermissionState::Denied,
                calendar: PermissionState::Restricted,
                reminders: PermissionState::NotRequested,
                ..Settings::default()
            },
        );
        let permissions = model.settings_presentation().permissions;
        assert_eq!(permissions[0].status, "Unavailable");
        assert_eq!(permissions[0].action_label, None);
        assert_eq!(permissions[1].status, "Unavailable");
        assert_eq!(permissions[1].action_label, None);
        assert_eq!(permissions[2].action_label.as_deref(), Some("Set Up…"));
    }

    #[test]
    fn manually_checking_an_automation_uses_the_model_without_internal_prompts() {
        let mut model = ViewModel::new(
            fixture_snapshot(),
            Vec::new(),
            vec![todo_workflow()],
            Settings::default(),
        );
        let effects = model.update(Intent::RunWorkflowWithLocalModel(0)).unwrap();
        assert!(matches!(
            &effects[0],
            Effect::RunWorkflowWithLocalModel {
                workflow_id,
                resume_state_id,
                ..
            } if workflow_id == "todos" && resume_state_id == "message_ingested"
        ));
        assert_eq!(
            model.feedback(),
            Some("Checking this recording with To-dos")
        );
    }

    #[test]
    fn navigation_and_search_are_renderer_independent() {
        let mut model = model();
        assert_eq!(model.route(), Route::Review);
        assert_eq!(model.visible_recordings().len(), 2);
        model.update(Intent::Search("second".into())).unwrap();
        assert_eq!(model.route(), Route::Library);
        assert_eq!(model.visible_recordings().len(), 1);
        assert!(
            model
                .selected_recording()
                .unwrap()
                .summary
                .contains("second")
        );
    }

    #[test]
    fn inbox_badge_tracks_only_recordings_waiting_for_review() {
        let mut model = model();
        assert_eq!(model.inbox_badge_text().as_deref(), Some("2"));

        model.update(Intent::ConfirmProcessing).unwrap();
        assert_eq!(model.inbox_badge_text().as_deref(), Some("1"));

        model.update(Intent::ConfirmProcessing).unwrap();
        assert_eq!(model.inbox_badge_text(), None);
    }

    #[test]
    fn confirming_a_recording_emits_a_semantic_effect_and_advances_review() {
        let mut model = model();
        let first = model.selected_id().unwrap();
        let prompt = model
            .recording_presentation()
            .unwrap()
            .review_acknowledgement
            .unwrap();
        assert_eq!(prompt.action_label, "Looks Good — Remove from Review");
        assert!(prompt.explanation.contains("stays here"));
        let effects = model.update(Intent::ConfirmProcessing).unwrap();
        assert_eq!(
            effects,
            vec![Effect::ReviewRecording {
                recording_id: first,
                looks_right: true,
                note: None,
            }]
        );
        assert_ne!(model.selected_id(), Some(first));
        assert_eq!(model.feedback(), Some("Removed from Review"));
    }

    #[test]
    fn transcript_corrections_are_non_destructive_overlays() {
        let mut model = model();
        let original = model.selected_recording().unwrap().segments[0]
            .raw_text
            .clone();
        let effects = model
            .update(Intent::CorrectTranscript {
                ordinal: 0,
                replacement: "Buy compost on Sunday.".into(),
            })
            .unwrap();
        assert!(matches!(effects[0], Effect::CorrectTranscript { .. }));
        let segment = &model.selected_recording().unwrap().segments[0];
        assert_eq!(segment.raw_text, original);
        assert_eq!(segment.displayed_text(), "Buy compost on Sunday.");
    }

    #[test]
    fn no_selection_cannot_accidentally_emit_a_review() {
        let mut model = ViewModel::new(
            AppSnapshot::default(),
            Vec::new(),
            Vec::new(),
            Settings::default(),
        );
        assert_eq!(model.route(), Route::Library);
        assert_eq!(
            model.update(Intent::ConfirmProcessing),
            Err("No recording is selected".into())
        );
    }

    #[test]
    fn delivered_actions_cannot_be_decided_twice() {
        let mut model = model();
        assert_eq!(
            model.update(Intent::ApproveAction(0)),
            Err("This action was already delivered to Reminders".into())
        );
    }

    #[test]
    fn delivered_actions_expose_and_dispatch_undo() {
        let mut model = model();
        let action_id = model.selected_recording().unwrap().actions[0].action_id;
        assert_eq!(
            model.recording_presentation().unwrap().actions[0].remove_label,
            Some("Undo This Change".into())
        );

        assert_eq!(
            model.update(Intent::RemoveAction(0)),
            Ok(vec![Effect::UndoAction { action_id }])
        );
        let action = &model.recording_presentation().unwrap().actions[0];
        assert_eq!(action.state, ActionReceiptState::Undone);
        assert_eq!(action.remove_label, None);
    }

    #[test]
    fn delivered_actions_can_be_manually_revised_without_internal_workflow_fields() {
        let mut model = model();
        let action_id = model.selected_recording().unwrap().actions[0].action_id;
        let effects = model
            .update(Intent::EditAction {
                index: 0,
                kind: ActionKind::Reminder,
                title: "Call the dentist".into(),
                due_at_ms: Some(1_800_000_000_000),
            })
            .unwrap();

        assert_eq!(
            effects,
            vec![Effect::ReviseAction {
                action_id,
                kind: ActionKind::Reminder,
                title: "Call the dentist".into(),
                due_at_ms: Some(1_800_000_000_000),
            }]
        );
        let action = &model.recording_presentation().unwrap().actions[0];
        assert_eq!(action.payload, "Call the dentist");
        assert_eq!(action.due_at_ms, Some(1_800_000_000_000));
        assert_eq!(action.edit_label, "Edit and Apply…");
    }

    #[test]
    fn a_human_can_add_the_action_the_model_missed() {
        let mut model = model();
        let recording_id = model.selected_id().unwrap();
        let transcript_run_id = model
            .selected_recording()
            .unwrap()
            .transcript_run_id
            .unwrap();
        model.recording_mut(recording_id).unwrap().actions.clear();

        let editor = model.action_editor(None).unwrap();
        assert_eq!(editor.heading, "Add an action");
        assert_eq!(editor.submit_label, "Add to My Mac");
        assert_eq!(editor.kind, ActionKind::Reminder);

        let effects = model
            .update(Intent::AddAction {
                kind: ActionKind::Todo,
                title: "Book the train".into(),
                due_at_ms: None,
            })
            .unwrap();
        let action_id = model.selected_recording().unwrap().actions[0].action_id;
        assert_eq!(
            effects,
            vec![Effect::CreateAction {
                action_id,
                recording_id,
                transcript_run_id,
                kind: ActionKind::Todo,
                title: "Book the train".into(),
                due_at_ms: None,
            }]
        );
        let action = &model.recording_presentation().unwrap().actions[0];
        assert_eq!(action.operation, "Create a to-do");
        assert_eq!(action.payload, "Book the train");
        assert_eq!(action.remove_label, Some("Remove This Action".into()));
    }

    #[test]
    fn manually_added_calendar_events_still_require_a_time() {
        let mut model = model();
        assert_eq!(
            model.update(Intent::AddAction {
                kind: ActionKind::CalendarEvent,
                title: "Dinner".into(),
                due_at_ms: None,
            }),
            Err("Calendar events require a date and time".into())
        );
    }

    #[test]
    fn queued_actions_expose_and_dispatch_remove() {
        let mut model = model();
        let selected = model.selected_id().unwrap();
        let action = &mut model.recording_mut(selected).unwrap().actions[0];
        action.delivered_target = None;
        action.decision = "approved".into();
        let action_id = action.action_id;
        assert_eq!(
            model.recording_presentation().unwrap().actions[0].remove_label,
            Some("Remove This Action".into())
        );

        assert_eq!(
            model.update(Intent::RemoveAction(0)),
            Ok(vec![Effect::DecideAction {
                action_id,
                approved: false,
                reason: Some("Removed in Captain’s Log before delivery".into()),
            }])
        );
        let action = &model.recording_presentation().unwrap().actions[0];
        assert_eq!(action.state, ActionReceiptState::Skipped);
        assert_eq!(action.remove_label, None);
    }

    #[test]
    fn failures_are_scoped_to_the_selected_recording() {
        let mut model = model();
        let selected = model.selected_id().unwrap();
        let unrelated = model
            .recordings()
            .iter()
            .find(|recording| recording.recording_id != selected)
            .unwrap()
            .recording_id;
        model.snapshot.failures = vec![
            AppFailure {
                recording_id: Some(selected),
                detail: "selected failure".into(),
            },
            AppFailure {
                recording_id: Some(unrelated),
                detail: "unrelated failure".into(),
            },
            AppFailure {
                recording_id: None,
                detail: "global failure".into(),
            },
        ];

        assert_eq!(
            model.failures().collect::<Vec<_>>(),
            vec!["selected failure", "global failure"]
        );
    }

    #[test]
    fn presentation_generates_concise_titles_and_human_receipts() {
        let model = model();
        let rows = model.library_rows();
        assert_eq!(
            rows[0].title,
            "Synthetic recording with a delivered reminder"
        );
        let detail = model.recording_presentation().unwrap();
        assert_eq!(detail.actions[0].operation, "Create a reminder");
        assert_eq!(detail.actions[0].destination, "Apple Reminders");
        assert_eq!(detail.actions[0].state, ActionReceiptState::Delivered);
        assert_eq!(detail.actions[0].state_label, "Completed");
        assert_eq!(
            detail.actions[0].evidence_quote.as_deref(),
            Some("Synthetic transcript evidence")
        );
        assert_eq!(
            detail.actions[0].state_detail,
            "A matching item was created in Apple Reminders."
        );
        assert_eq!(detail.transcript[0].time_label, "0:00");
        assert!(
            !detail
                .header
                .title
                .contains(&model.selected_id().unwrap().to_string())
        );
    }

    #[test]
    fn action_receipts_never_conflate_approval_with_delivery() {
        let mut action = AppAction {
            action_id: Uuid::new_v4(),
            kind: "Todo".into(),
            title: "Buy milk".into(),
            due_at_ms: None,
            evidence_quote: None,
            decision: "waiting for review".into(),
            delivered_target: None,
        };
        assert_eq!(
            action_receipt_state(&action),
            ActionReceiptState::AwaitingApproval
        );

        action.decision = "approved".into();
        assert_eq!(
            action_receipt_state(&action),
            ActionReceiptState::ApprovedPendingDelivery
        );
        let copy = action_state_copy(action_receipt_state(&action), &action_destination(&action));
        assert_eq!(copy.0, "Waiting to be added");
        assert!(copy.1.contains("has not confirmed"));

        action.delivered_target = Some("Reminders".into());
        assert_eq!(action_receipt_state(&action), ActionReceiptState::Delivered);

        action.delivered_target = None;
        action.decision = "not added".into();
        assert_eq!(action_receipt_state(&action), ActionReceiptState::Skipped);
        action.decision = "removed".into();
        assert_eq!(action_receipt_state(&action), ActionReceiptState::Undone);
        action.decision = "processing".into();
        assert_eq!(
            action_receipt_state(&action),
            ActionReceiptState::Processing
        );
    }

    #[test]
    fn workflow_receipts_expose_trigger_result_and_review_as_separate_fields() {
        let mut snapshot = fixture_snapshot();
        snapshot.recordings[0].workflow_runs.push(AppWorkflowRun {
            run_id: Uuid::new_v4(),
            workflow_id: "calendar-capture".into(),
            requested_by: "local_model".into(),
            status: "completed".into(),
            confirmed: false,
            steps: vec![AppWorkflowStep {
                state_id: "message_ingested".into(),
                available_transition_ids: vec!["create_event".into()],
                transition_id: Some("create_event".into()),
                outputs: Default::default(),
                selected_by: "local_model".into(),
                destination_state_id: Some("completed".into()),
                receipt: Some("Created an event in Apple Calendar".into()),
                undoable: true,
                undone: false,
            }],
        });
        let model = ViewModel::new(snapshot, Vec::new(), Vec::new(), Settings::default());

        let receipt = &model.recording_presentation().unwrap().activity[0];
        assert_eq!(receipt.workflow, "Calendar capture");
        assert_eq!(receipt.triggered_by, "The app checked it");
        assert_eq!(receipt.state, "Completed");
        assert_eq!(receipt.result, "Created an event in Apple Calendar");
        assert_eq!(receipt.confirmation, "Not reviewed yet");
        assert!(receipt.can_adjust);
    }

    #[test]
    fn native_app_receipts_show_human_results_without_external_ids() {
        let run = AppWorkflowRun {
            run_id: Uuid::new_v4(),
            workflow_id: "notes".into(),
            requested_by: "local_model".into(),
            status: "complete".into(),
            confirmed: false,
            steps: vec![AppWorkflowStep {
                state_id: "prepare".into(),
                available_transition_ids: vec!["create".into()],
                transition_id: Some("create".into()),
                outputs: Default::default(),
                selected_by: "local_model".into(),
                destination_state_id: Some("done".into()),
                receipt: Some(
                    serde_json::json!({
                        "kind":"notes",
                        "external_id":"x-coredata://private-object-id",
                        "destination":"Apple Notes",
                        "summary":"Created note ‘Packing list’"
                    })
                    .to_string(),
                ),
                undoable: true,
                undone: false,
            }],
        };
        let result = workflow_result(&run);
        assert_eq!(result, "Created note ‘Packing list’ in Apple Notes");
        assert!(!result.contains("x-coredata"));
    }

    #[test]
    fn native_app_actions_expose_schema_driven_human_correction_fields() {
        let workflow = workflow_from_draft(
            AutomationDraft {
                name: "Meeting Notes".into(),
                description: "Save meeting notes".into(),
                timing: AutomationTiming::WhenRelevant,
                connection: AutomationConnection::Notes,
                target: String::new(),
                undo_target: None,
            },
            &[],
        )
        .unwrap();
        let mut snapshot = fixture_snapshot();
        snapshot.recordings[0].workflow_runs.push(AppWorkflowRun {
            run_id: Uuid::new_v4(),
            workflow_id: workflow.id.clone(),
            requested_by: "local_model".into(),
            status: "complete".into(),
            confirmed: false,
            steps: vec![AppWorkflowStep {
                state_id: "prepare".into(),
                available_transition_ids: vec!["create".into()],
                transition_id: Some("create".into()),
                outputs: BTreeMap::from([
                    ("note_title".into(), "Project Aurora".into()),
                    ("note_body".into(), "Decisions and next steps".into()),
                    ("notes_folder".into(), String::new()),
                ]),
                selected_by: "local_model".into(),
                destination_state_id: Some("done".into()),
                receipt: Some("created".into()),
                undoable: true,
                undone: false,
            }],
        });
        let model = ViewModel::new(snapshot, Vec::new(), vec![workflow], Settings::default());
        let editor = model.workflow_correction_editor(0).unwrap();
        assert_eq!(editor.state_id, "prepare");
        assert_eq!(editor.transition_id, "create");
        assert!(editor.can_undo);
        assert_eq!(editor.fields.len(), 3);
        assert_eq!(editor.fields[0].label, "A concise title for the note");
        assert_eq!(editor.fields[0].value, "Project Aurora");
    }

    #[test]
    fn workflow_receipts_hide_backend_ids_and_explain_no_action_runs() {
        let mut snapshot = fixture_snapshot();
        snapshot.recordings[0].workflow_runs = vec![
            AppWorkflowRun {
                run_id: Uuid::new_v4(),
                workflow_id: "calendar".into(),
                requested_by: "automatic".into(),
                status: "complete".into(),
                confirmed: false,
                steps: vec![AppWorkflowStep {
                    state_id: "message_ingested".into(),
                    available_transition_ids: vec!["create".into(), "ignore".into()],
                    transition_id: Some("create".into()),
                    outputs: Default::default(),
                    selected_by: "local_model".into(),
                    destination_state_id: Some("done".into()),
                    receipt: Some(format!("queued Mac action {}", Uuid::new_v4())),
                    undoable: true,
                    undone: false,
                }],
            },
            AppWorkflowRun {
                run_id: Uuid::new_v4(),
                workflow_id: "reminders".into(),
                requested_by: "automatic".into(),
                status: "complete".into(),
                confirmed: false,
                steps: vec![AppWorkflowStep {
                    state_id: "message_ingested".into(),
                    available_transition_ids: vec!["create".into(), "ignore".into()],
                    transition_id: Some("ignore".into()),
                    outputs: Default::default(),
                    selected_by: "local_model".into(),
                    destination_state_id: Some("done".into()),
                    receipt: None,
                    undoable: false,
                    undone: false,
                }],
            },
        ];
        let model = ViewModel::new(snapshot, Vec::new(), Vec::new(), Settings::default());
        let detail = model.recording_presentation().unwrap();

        assert_eq!(detail.activity[0].state, "Completed");
        assert_eq!(
            detail.activity[0].result,
            "The app chose to add an item. Its delivery appears in Actions Taken."
        );
        assert!(!detail.activity[0].result.contains('-'));
        assert_eq!(detail.activity[1].result, "Nothing needed to be added.");
    }

    #[test]
    fn editable_title_is_an_effect_backed_annotation() {
        let mut model = model();
        let id = model.selected_id().unwrap();
        let effects = model
            .update(Intent::RenameRecording("Project notes".into()))
            .unwrap();
        assert_eq!(
            effects,
            vec![Effect::SaveRecordingTitle {
                recording_id: id,
                title: "Project notes".into(),
            }]
        );
        assert_eq!(model.library_rows()[0].title, "Project notes");
        assert_eq!(
            model.recording_presentation().unwrap().header.title,
            "Project notes"
        );
    }
}
