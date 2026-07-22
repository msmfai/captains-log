use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use block2::RcBlock;
use chrono::{Datelike, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use clap::{Parser, Subcommand};
use objc2::runtime::Bool;
use objc2_event_kit::{
    EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStore, EKReminder, EKSpan,
};
use objc2_foundation::{NSDate, NSDateComponents, NSError, NSString, NSTimeZone};
use personal_audio_lake_contracts::{
    ActionDelivery, ActionKind, ActionProposal, ActionRevisionRequest, ActionUndo, Envelope,
    Failure, HealthState, Message, WorkerHealth, WorkerRole, ports::EffectAdapter,
};
use uuid::Uuid;

#[derive(Parser)]
struct Cli {
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_STATE_ROOT")]
    state_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Trigger the native macOS Calendar and Reminders permission prompts.
    RequestAccess,
    /// Trigger only the native macOS Calendar permission prompt.
    RequestCalendarAccess,
    /// Trigger only the native macOS Reminders permission prompt.
    RequestRemindersAccess,
    /// Consume proposals/approvals as NDJSON and deliver approved actions through EventKit.
    Worker,
}

#[derive(Debug, Clone)]
struct NativeEffectReceipt {
    proposal: ActionProposal,
    external_id: String,
}

struct EventKitEffectAdapter;

impl EffectAdapter<ActionProposal, NativeEffectReceipt> for EventKitEffectAdapter {
    type Error = anyhow::Error;

    fn apply(&mut self, proposal: ActionProposal) -> Result<NativeEffectReceipt> {
        let external_id = deliver_eventkit(&proposal)?;
        Ok(NativeEffectReceipt {
            proposal,
            external_id,
        })
    }

    fn undo(&mut self, receipt: NativeEffectReceipt) -> Result<()> {
        remove_eventkit(&receipt.proposal, &receipt.external_id)
    }
}

fn apply_effect<Adapter, Effect, Receipt>(adapter: &mut Adapter, effect: Effect) -> Result<Receipt>
where
    Adapter: EffectAdapter<Effect, Receipt, Error = anyhow::Error>,
{
    adapter.apply(effect)
}

fn undo_effect<Adapter, Effect, Receipt>(adapter: &mut Adapter, receipt: Receipt) -> Result<()>
where
    Adapter: EffectAdapter<Effect, Receipt, Error = anyhow::Error>,
{
    adapter.undo(receipt)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let state_root = cli.state_root.unwrap_or_else(default_state_root);
    fs::create_dir_all(state_root.join("calendar/pending"))?;
    fs::create_dir_all(state_root.join("calendar/delivered"))?;
    fs::create_dir_all(state_root.join("calendar/undone"))?;
    match cli.command.unwrap_or(Command::Worker) {
        Command::RequestAccess => request_access(),
        Command::RequestCalendarAccess => request_calendar_access(),
        Command::RequestRemindersAccess => request_reminders_access(),
        Command::Worker => worker(&state_root),
    }
}

fn worker(state_root: &Path) -> Result<()> {
    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
        role: WorkerRole::Calendar,
        state: HealthState::Ready,
        detail: "macOS EventKit adapter ready; access is requested only when used".into(),
        backlog: 0,
    })))?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let envelope = Envelope::from_json(&line)?;
        match &envelope.message {
            Message::ActionProposed(proposal) => persist_proposal(state_root, proposal)?,
            Message::ActionRevisionRequested(request) => match revise(state_root, request) {
                Ok((proposal, needs_delivery)) => {
                    emit(&Envelope::caused_by(
                        &envelope,
                        Message::ActionProposed(proposal.clone()),
                    ))?;
                    if needs_delivery {
                        emit(&Envelope::caused_by(
                            &envelope,
                            Message::ActionApproved {
                                action_id: proposal.action_id,
                            },
                        ))?;
                    }
                }
                Err(error) => emit(&failure(
                    &envelope,
                    "eventkit_revision",
                    &format!("{error:#}"),
                ))?,
            },
            Message::ActionApproved { action_id } => {
                let result = approve(state_root, *action_id);
                let response = match result {
                    Ok(delivery) => {
                        Envelope::caused_by(&envelope, Message::ActionDelivered(delivery))
                    }
                    Err(error) => failure(&envelope, "eventkit_delivery", &format!("{error:#}")),
                };
                emit(&response)?;
            }
            Message::ActionUndoRequested { action_id } => {
                let result = undo(state_root, *action_id);
                let response = match result {
                    Ok(undo) => Envelope::caused_by(&envelope, Message::ActionUndone(undo)),
                    Err(error) => failure(&envelope, "eventkit_undo", &format!("{error:#}")),
                };
                emit(&response)?;
            }
            _ => emit(&failure(
                &envelope,
                "unsupported_message",
                "calendar worker accepts action proposals, revisions, approvals, and undo requests",
            ))?,
        }
    }
    Ok(())
}

fn revise(state_root: &Path, request: &ActionRevisionRequest) -> Result<(ActionProposal, bool)> {
    let title = request.title.trim();
    if title.is_empty() || title.chars().count() > 300 {
        bail!("title must contain 1 to 300 characters");
    }
    let proposal_path = state_root
        .join("calendar/pending")
        .join(format!("{}.json", request.action_id));
    let mut proposal: ActionProposal = serde_json::from_slice(
        &fs::read(&proposal_path)
            .with_context(|| format!("no pending action {}", request.action_id))?,
    )?;
    if request.kind == ActionKind::CalendarEvent && request.due_at_ms.is_none() {
        bail!("calendar events require a date and time");
    }
    let destination_changed = matches!(proposal.kind, ActionKind::CalendarEvent)
        != matches!(request.kind, ActionKind::CalendarEvent);
    let previous_proposal = proposal.clone();
    proposal.kind = request.kind;
    proposal.title = title.to_owned();
    proposal.due_at_ms = request.due_at_ms;
    proposal.original_time_phrase = None;
    proposal.confidence = 1.0;

    let delivered_path = state_root
        .join("calendar/delivered")
        .join(format!("{}.json", request.action_id));
    let undone_path = state_root
        .join("calendar/undone")
        .join(format!("{}.json", request.action_id));
    let active_delivery = delivered_path.exists() && !undone_path.exists();
    if active_delivery && destination_changed {
        let delivery: ActionDelivery = serde_json::from_slice(&fs::read(&delivered_path)?)?;
        undo_effect(
            &mut EventKitEffectAdapter,
            NativeEffectReceipt {
                proposal: previous_proposal,
                external_id: delivery.external_id,
            },
        )?;
        fs::remove_file(&delivered_path)?;
    } else if active_delivery {
        let delivery: ActionDelivery = serde_json::from_slice(&fs::read(&delivered_path)?)?;
        revise_native(&proposal, &delivery.external_id)?;
    }
    persist_proposal(state_root, &proposal)?;
    Ok((proposal, !active_delivery || destination_changed))
}

fn revise_native(proposal: &ActionProposal, external_id: &str) -> Result<()> {
    unsafe {
        let store = EKEventStore::new();
        let identifier = NSString::from_str(external_id);
        match proposal.kind {
            ActionKind::CalendarEvent => {
                ensure_authorized(EKEntityType::Event)?;
                let event = store
                    .eventWithIdentifier(&identifier)
                    .ok_or_else(|| anyhow!("calendar event no longer exists"))?;
                let due = proposal
                    .due_at_ms
                    .ok_or_else(|| anyhow!("calendar event has no time"))?;
                event.setTitle(Some(&NSString::from_str(&proposal.title)));
                event.setStartDate(Some(&NSDate::dateWithTimeIntervalSince1970(
                    due as f64 / 1_000.0,
                )));
                event.setEndDate(Some(&NSDate::dateWithTimeIntervalSince1970(
                    due as f64 / 1_000.0 + 1_800.0,
                )));
                store
                    .saveEvent_span_error(&event, EKSpan::ThisEvent)
                    .map_err(|error| anyhow!("EventKit save event failed: {error:?}"))?;
            }
            ActionKind::Todo | ActionKind::Reminder => {
                ensure_authorized(EKEntityType::Reminder)?;
                let item = store
                    .calendarItemWithIdentifier(&identifier)
                    .ok_or_else(|| anyhow!("reminder no longer exists"))?;
                let reminder = item
                    .downcast::<EKReminder>()
                    .map_err(|_| anyhow!("calendar item is not a reminder"))?;
                reminder.setTitle(Some(&NSString::from_str(&proposal.title)));
                let due = proposal
                    .due_at_ms
                    .map(|due| date_components(due, &proposal.timezone))
                    .transpose()?;
                reminder.setDueDateComponents(due.as_deref());
                store
                    .saveReminder_commit_error(&reminder, true)
                    .map_err(|error| anyhow!("EventKit save reminder failed: {error:?}"))?;
            }
        }
    }
    Ok(())
}

fn undo(state_root: &Path, action_id: Uuid) -> Result<ActionUndo> {
    let undone_path = state_root
        .join("calendar/undone")
        .join(format!("{action_id}.json"));
    if undone_path.exists() {
        return Ok(serde_json::from_slice(&fs::read(undone_path)?)?);
    }
    let delivery: ActionDelivery = serde_json::from_slice(
        &fs::read(
            state_root
                .join("calendar/delivered")
                .join(format!("{action_id}.json")),
        )
        .with_context(|| format!("action {action_id} has not been delivered"))?,
    )?;
    let proposal: ActionProposal = serde_json::from_slice(&fs::read(
        state_root
            .join("calendar/pending")
            .join(format!("{action_id}.json")),
    )?)?;
    undo_effect(
        &mut EventKitEffectAdapter,
        NativeEffectReceipt {
            proposal,
            external_id: delivery.external_id.clone(),
        },
    )?;
    let undo = ActionUndo {
        action_id,
        target: delivery.target,
        undone_at_ms: Utc::now().timestamp_millis(),
    };
    atomic_write(&undone_path, &serde_json::to_vec_pretty(&undo)?)?;
    Ok(undo)
}

fn remove_eventkit(proposal: &ActionProposal, external_id: &str) -> Result<()> {
    unsafe {
        let store = EKEventStore::new();
        let identifier = NSString::from_str(external_id);
        match proposal.kind {
            ActionKind::CalendarEvent => {
                ensure_authorized(EKEntityType::Event)?;
                let event = store
                    .eventWithIdentifier(&identifier)
                    .ok_or_else(|| anyhow!("calendar event no longer exists"))?;
                store
                    .removeEvent_span_error(&event, EKSpan::ThisEvent)
                    .map_err(|error| anyhow!("EventKit remove event failed: {error:?}"))?;
            }
            ActionKind::Todo | ActionKind::Reminder => {
                ensure_authorized(EKEntityType::Reminder)?;
                let item = store
                    .calendarItemWithIdentifier(&identifier)
                    .ok_or_else(|| anyhow!("reminder no longer exists"))?;
                let reminder = item
                    .downcast::<EKReminder>()
                    .map_err(|_| anyhow!("calendar item is not a reminder"))?;
                store
                    .removeReminder_commit_error(&reminder, true)
                    .map_err(|error| anyhow!("EventKit remove reminder failed: {error:?}"))?;
            }
        }
    }
    Ok(())
}

fn persist_proposal(state_root: &Path, proposal: &ActionProposal) -> Result<()> {
    let target = state_root
        .join("calendar/pending")
        .join(format!("{}.json", proposal.action_id));
    atomic_write(&target, &serde_json::to_vec_pretty(proposal)?)
}

fn approve(state_root: &Path, action_id: Uuid) -> Result<ActionDelivery> {
    let receipt_path = state_root
        .join("calendar/delivered")
        .join(format!("{action_id}.json"));
    let undone_path = state_root
        .join("calendar/undone")
        .join(format!("{action_id}.json"));
    if receipt_path.exists() && !undone_path.exists() {
        return Ok(serde_json::from_slice(&fs::read(receipt_path)?)?);
    }
    if undone_path.exists() {
        fs::remove_file(&receipt_path).ok();
        fs::remove_file(&undone_path)?;
    }
    let proposal_path = state_root
        .join("calendar/pending")
        .join(format!("{action_id}.json"));
    let proposal: ActionProposal = serde_json::from_slice(
        &fs::read(&proposal_path).with_context(|| format!("no pending action {action_id}"))?,
    )?;
    let receipt = apply_effect(&mut EventKitEffectAdapter, proposal.clone())?;
    let external_id = receipt.external_id;
    let delivery = ActionDelivery {
        action_id,
        external_id,
        delivered_at_ms: Utc::now().timestamp_millis(),
        target: match proposal.kind {
            ActionKind::CalendarEvent => "macos_calendar",
            ActionKind::Todo | ActionKind::Reminder => "macos_reminders",
        }
        .to_owned(),
    };
    atomic_write(&receipt_path, &serde_json::to_vec_pretty(&delivery)?)?;
    Ok(delivery)
}

fn deliver_eventkit(proposal: &ActionProposal) -> Result<String> {
    // SAFETY: EventKit objects remain owned for the duration of each call; all nullable Objective-C
    // values are checked before use, and this process links the native EventKit framework.
    unsafe {
        let store = EKEventStore::new();
        match proposal.kind {
            ActionKind::CalendarEvent => {
                ensure_authorized(EKEntityType::Event)?;
                let due = proposal
                    .due_at_ms
                    .ok_or_else(|| anyhow!("calendar event has no time"))?;
                let calendar = store
                    .defaultCalendarForNewEvents()
                    .ok_or_else(|| anyhow!("no writable default macOS calendar"))?;
                let event = EKEvent::eventWithEventStore(&store);
                event.setTitle(Some(&NSString::from_str(&proposal.title)));
                event.setCalendar(Some(&calendar));
                let start = NSDate::dateWithTimeIntervalSince1970(due as f64 / 1_000.0);
                let end = NSDate::dateWithTimeIntervalSince1970(due as f64 / 1_000.0 + 1_800.0);
                event.setStartDate(Some(&start));
                event.setEndDate(Some(&end));
                store
                    .saveEvent_span_error(&event, EKSpan::ThisEvent)
                    .map_err(|error| anyhow!("EventKit save event failed: {error:?}"))?;
                Ok(event
                    .eventIdentifier()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| proposal.action_id.to_string()))
            }
            ActionKind::Todo | ActionKind::Reminder => {
                ensure_authorized(EKEntityType::Reminder)?;
                let calendar = store
                    .defaultCalendarForNewReminders()
                    .ok_or_else(|| anyhow!("no writable default macOS reminders list"))?;
                let reminder = EKReminder::reminderWithEventStore(&store);
                reminder.setTitle(Some(&NSString::from_str(&proposal.title)));
                reminder.setCalendar(Some(&calendar));
                if let Some(due) = proposal.due_at_ms {
                    let components = date_components(due, &proposal.timezone)?;
                    reminder.setDueDateComponents(Some(&components));
                }
                store
                    .saveReminder_commit_error(&reminder, true)
                    .map_err(|error| anyhow!("EventKit save reminder failed: {error:?}"))?;
                Ok(reminder.calendarItemIdentifier().to_string())
            }
        }
    }
}

fn date_components(
    timestamp_ms: i64,
    timezone: &str,
) -> Result<objc2::rc::Retained<NSDateComponents>> {
    let tz: Tz = timezone
        .parse()
        .with_context(|| format!("unknown timezone {timezone}"))?;
    let local = tz
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .ok_or_else(|| anyhow!("invalid due timestamp {timestamp_ms}"))?;
    let components = NSDateComponents::new();
    components.setYear(local.year() as isize);
    components.setMonth(local.month() as isize);
    components.setDay(local.day() as isize);
    components.setHour(local.hour() as isize);
    components.setMinute(local.minute() as isize);
    components.setSecond(local.second() as isize);
    let native_tz = NSTimeZone::timeZoneWithName(&NSString::from_str(timezone))
        .ok_or_else(|| anyhow!("EventKit does not recognize timezone {timezone}"))?;
    components.setTimeZone(Some(&native_tz));
    Ok(components)
}

fn ensure_authorized(entity: EKEntityType) -> Result<()> {
    // SAFETY: Class method has no pointer parameters and only reads TCC state.
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(entity) };
    if has_reversible_access(status) {
        Ok(())
    } else {
        let feature = match entity {
            EKEntityType::Event => "Calendar",
            EKEntityType::Reminder => "Reminders",
            _ => "Calendar or Reminders",
        };
        bail!("Captain’s Log does not have access to {feature}. Nothing was added.")
    }
}

fn has_reversible_access(status: EKAuthorizationStatus) -> bool {
    status == EKAuthorizationStatus::FullAccess
}

fn request_access() -> Result<()> {
    request_calendar_access()?;
    request_reminders_access()
}

fn request_calendar_access() -> Result<()> {
    // SAFETY: The copied blocks live until each callback sends its result; EventKit owns them while
    // asynchronous permission UI is active. NSError pointers are not dereferenced.
    unsafe {
        let store = EKEventStore::new();
        let (events_tx, events_rx) = mpsc::channel();
        let events: RcBlock<dyn Fn(Bool, *mut NSError)> = RcBlock::new(move |granted: Bool, _| {
            events_tx.send(granted.as_bool()).ok().unwrap_or(())
        });
        store.requestFullAccessToEventsWithCompletion(&*events as *const _ as *mut _);
        if !events_rx
            .recv_timeout(Duration::from_secs(120))
            .context("Calendar permission timeout")?
        {
            bail!("Calendar access was denied");
        }
    }
    println!("Calendar access granted");
    Ok(())
}

fn request_reminders_access() -> Result<()> {
    // SAFETY: The copied block lives until the callback sends its result; EventKit owns it while
    // asynchronous permission UI is active. NSError pointers are not dereferenced.
    unsafe {
        let store = EKEventStore::new();
        let (reminders_tx, reminders_rx) = mpsc::channel();
        let reminders: RcBlock<dyn Fn(Bool, *mut NSError)> =
            RcBlock::new(move |granted: Bool, _| {
                reminders_tx.send(granted.as_bool()).ok().unwrap_or(())
            });
        store.requestFullAccessToRemindersWithCompletion(&*reminders as *const _ as *mut _);
        if !reminders_rx
            .recv_timeout(Duration::from_secs(120))
            .context("Reminders permission timeout")?
        {
            bail!("Reminders access was denied");
        }
    }
    println!("Reminders access granted");
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn failure(parent: &Envelope, code: &str, detail: &str) -> Envelope {
    Envelope::caused_by(
        parent,
        Message::Failure(Failure {
            worker: WorkerRole::Calendar,
            code: code.to_owned(),
            detail: detail.to_owned(),
            retryable: false,
            failed_message_id: Some(parent.id),
        }),
    )
}

fn emit(envelope: &Envelope) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(envelope.to_ndjson()?.as_bytes())?;
    stdout.flush()?;
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
    use personal_audio_lake_contracts::SegmentEvidence;

    #[derive(Default)]
    struct FixtureEffectAdapter {
        applied: Vec<Uuid>,
        undone: Vec<Uuid>,
    }

    struct FixtureReceipt(Uuid);

    impl EffectAdapter<ActionProposal, FixtureReceipt> for FixtureEffectAdapter {
        type Error = anyhow::Error;

        fn apply(&mut self, effect: ActionProposal) -> Result<FixtureReceipt> {
            self.applied.push(effect.action_id);
            Ok(FixtureReceipt(effect.action_id))
        }

        fn undo(&mut self, receipt: FixtureReceipt) -> Result<()> {
            self.undone.push(receipt.0);
            Ok(())
        }
    }

    #[test]
    fn contract_fixtures_are_supported() {
        personal_audio_lake_contracts::assert_contract_fixtures();
    }

    #[test]
    fn generic_effect_boundary_supports_apply_and_compensating_undo() {
        let action_id = Uuid::new_v4();
        let proposal = ActionProposal {
            action_id,
            recording_id: Uuid::new_v4(),
            transcript_run_id: Uuid::new_v4(),
            kind: ActionKind::Todo,
            title: "Buy milk".into(),
            due_at_ms: None,
            original_time_phrase: None,
            timezone: "Europe/London".into(),
            confidence: 1.0,
            evidence: Vec::new(),
            extractor_version: "fixture".into(),
        };
        let mut adapter = FixtureEffectAdapter::default();

        let receipt = apply_effect(&mut adapter, proposal).unwrap();
        undo_effect(&mut adapter, receipt).unwrap();

        assert_eq!(adapter.applied, vec![action_id]);
        assert_eq!(adapter.undone, vec![action_id]);
    }

    #[test]
    fn calendar_undo_requires_full_access_not_write_only_access() {
        assert!(has_reversible_access(EKAuthorizationStatus::FullAccess));
        assert!(!has_reversible_access(EKAuthorizationStatus::WriteOnly));
    }

    #[test]
    fn a_manual_revision_replaces_the_pending_proposal_and_requests_delivery() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["calendar/pending", "calendar/delivered", "calendar/undone"] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        let action_id = Uuid::new_v4();
        let proposal = ActionProposal {
            action_id,
            recording_id: Uuid::new_v4(),
            transcript_run_id: Uuid::new_v4(),
            kind: ActionKind::Reminder,
            title: "Wrong title".into(),
            due_at_ms: Some(1_800_000_000_000),
            original_time_phrase: Some("tomorrow".into()),
            timezone: "Europe/London".into(),
            confidence: 0.7,
            evidence: vec![SegmentEvidence {
                ordinal: 0,
                start_ms: 0,
                end_ms: 1_000,
                quote: "remind me tomorrow".into(),
            }],
            extractor_version: "test".into(),
        };
        persist_proposal(root.path(), &proposal).unwrap();

        let (revised, needs_delivery) = revise(
            root.path(),
            &ActionRevisionRequest {
                action_id,
                kind: ActionKind::Todo,
                title: "Correct title".into(),
                due_at_ms: None,
                requested_at_ms: 1,
            },
        )
        .unwrap();

        assert!(needs_delivery);
        assert_eq!(revised.title, "Correct title");
        assert_eq!(revised.kind, ActionKind::Todo);
        assert_eq!(revised.due_at_ms, None);
        assert_eq!(revised.original_time_phrase, None);
        assert_eq!(revised.confidence, 1.0);
        let stored: ActionProposal = serde_json::from_slice(
            &fs::read(
                root.path()
                    .join(format!("calendar/pending/{action_id}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(stored, revised);
    }
}
