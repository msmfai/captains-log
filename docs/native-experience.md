# Captain’s Log: native experience contract

This document defines the product from the user's point of view. Backend events, databases,
workers, and APIs exist to support this experience; they do not define its information
architecture.

## Product promise

Captain’s Log quietly brings recordings onto this Mac, privately turns them into useful
information, and makes every resulting change easy to notice, understand, and correct. The app follows an
ask-forgiveness policy: bounded local-model decisions execute immediately, and their receipts
offer correction and undo instead of asking for approval first.

At any moment the user must be able to answer four questions without understanding the pipeline:

1. Did my recording arrive?
2. What did the app understand?
3. What did the app do because of it?
4. How do I correct it?

## Daily journey

### While the Mac is awake

The background service transfers and processes recordings without opening windows or menus. When
one recording finishes processing, the app posts one standard macOS notification containing
its concise summary and a deterministic action receipt. The receipt explicitly says when no action
was taken. Clicking the notification opens that exact recording in the app.

### Returning to the Mac

When the app starts and there are unseen recordings, it opens the native Review window once with a
"While you were away" queue. It does not replay a pile of notifications. If nothing is waiting, it
stays quiet in the menu bar.

### Reviewing a recording

The selected recording shows the audio, transcript, inferred meaning, and action receipt together.
Every unseen recording remains in Review until the user explicitly chooses
**Looks Good — Remove from Review**.
This acknowledges the result; it does not gate effects, which have already run. Before marking it
reviewed, the user can change what an automation chose, edit the text it supplied, manually check
an enabled automation, ask the local model to check for actions again, or undo a reversible
change. Transcript corrections are
non-destructive overlays; the raw lake remains immutable.

Each action receipt exposes **Edit and Apply…** without requiring workflow vocabulary. The user
can change the action type, user-facing text, and optional date/time. A correction to an existing
Calendar or Reminders item is applied in place when its native destination is unchanged; changing
between those destinations replaces the native item through one explicit revision message.

## Information architecture

The native window has four destinations:

- **Review** — unseen and flagged recordings that need attention.
- **Library** — all recordings, searchable by transcript and date.
- **Automations** — user-owned rules that inspect new recordings and may make changes. Advanced
  definitions use fixed control topology, deterministic transitions, optional local-model
  selectors, and declared undo actions. Every slot the model may fill can instead be filled by the
  user through the identical transition/field schema.
- **Settings** — always-on recorder-app transfer, optional compatibility device, local processing,
  storage, notifications, Calendar, Reminders, and startup behavior. Transfer readiness and paired
  device lifecycle are separate rows; the absence of a compatibility device never implies recorder
  apps cannot send. Notes, Mail, and Contacts request their separate macOS Automation permission
  when an enabled automation first uses that destination.

Review is the default destination when work is waiting. Library is the default otherwise.

The window uses a native sidebar, a scrollable recording list, and a scrollable detail view. Each
recording has an editable short title generated deterministically from its local summary when no
title annotation exists. Detail, Automations, and Settings content use intrinsic AppKit arranged
views: controls reserve their own space, multiline text reports its natural height, and the fitted
stack determines the scrollable document size. Child controls are never positioned with
recording-dependent coordinates. Automations and Settings use the full content area rather than
retaining an empty recording-list column.
The recording detail view contains three focused sections:

- **Overview** — recording time, transfer/review state, audio playback, summary, outcome-first
  change receipts and manual automation controls. Every receipt labels the
  operation, user-facing payload, destination, lifecycle state, and recorded external effect;
  model selection is never presented as successful delivery. New evidence-bound actions also show
  **Why this action was chosen** with the verbatim transcript quote accepted by deterministic
  validation. Every queued or in-progress action
  has a visible **Remove This Action** control, and every delivered action has a visible
  **Undo This Change** control directly on its receipt;
- **Transcript** — the complete scrollable transcript, timestamp-linked audio, and contextual
  non-destructive correction affordances;
- **History** — structured automation receipts with separately labeled automation, start reason,
  status, outcome, review state, and contextual correct/undo controls. The full technical
  state/transition trace is secondary and collapsed by default;
- normal controls such as **Check for a Reminder** and **Check for Actions Again**;
- per-run **Correct or Undo…** controls;
- **Looks Good — Remove from Review** after checking the complete result.

Routine views use ordinary product language: **Actions Taken**, **History**, **Automation**,
**Started**, **Status**, **Outcome**, **Reviewed**, and **Nothing Added**. Terms such as workflow,
trace, transition, provider, manifest, and backend identifiers belong only in explicitly advanced
editing or diagnostic surfaces.

**New Automation…** collects a name, purpose, when it should run, and a destination using ordinary
native prompts. Built-in destinations are Calendar, Reminders, Notes, a saved Mail draft, Contacts,
and a unique text file. Shortcuts bridge Messages, Home, Maps, Music, Safari, Finder, Focus, and
other apps without hard-coding those apps into the state-machine runner; webhooks bridge external
services. Native items carry compensating undo actions. A Shortcut or webhook can name an optional
user-owned undo action. The ViewModel turns these answers into the bounded state-machine definition;
the native view never asks the user to paste JSON or enter a pipe-delimited implementation format.

Every built-in integration has a declared output schema. The local model may fill only those text
fields, and the same schema drives **Correct or Undo…** so a person can change the model's title,
content, destination detail, or contact method. Applying a correction first compensates the prior
effect, then creates its replacement. Mail always stops at a saved draft and never sends a message.

## Menu bar contract

The menu-bar item is a compact control surface, not a transcript viewer or inbox. Its numbered
badge has one meaning: recordings waiting for the user to confirm that processing was correct.
Primary click, secondary click, and Control-click all open the same native utility menu. This keeps
the interaction discoverable and gives assistive technologies one consistent status-menu action.
When the badge is present, **Review N Recordings** is the first command; otherwise the first command
is **Open Captain’s Log** and opens Library.

The utility menu contains only:

- a concise incoming-transfer state derived from generic worker health; when a compatibility device
  is selected, its independent sync state comes from the versioned source lifecycle and is never
  inferred from a display string or described as connected merely because it is paired;
- **Review N Recordings** when review is waiting, otherwise **Open Captain’s Log**;
- **Sync Now**;
- **Settings…**;
- **Quit Captain’s Log** only when the command terminates the UI and every persistent service and
  prevents automatic relaunch. Until that lifecycle exists, the app must not present a fake Quit.

Recording text, summaries, test phrases, database health events, and disabled pseudo-controls must
never appear as menu items. A disabled source-state row is informational rather than a control.
Ready, active transfer, and attention-required states use distinct symbols; the numbered review
badge remains visible independently of operational state and does not rely on colour alone.

## Notification contract

Notifications use the single stable `com.smfmarin.personal-audio-lake` identity. There is no separate
notification helper application.

Each completed recording notification contains:

- title: "Captain’s Log received a recording";
- one concise, literal local-model summary;
- deterministic change receipt, including "Nothing was added";
- a **Confirm processing** action; corrections open the full trace in the app;
- recording identity for click-through.

Notification presentation follows macOS conventions. The app must not open its status menu to mimic
a notification.

## Permission contract

Permissions are requested from the app, one at a time, at the moment their corresponding
feature is enabled or first needed. The explanation precedes the system prompt. The app only uses
permissions for which macOS can present an in-context system prompt. It never opens, deep-links,
or instructs the user to visit System Settings. If access is denied or restricted, that destination
is shown as unavailable, nothing is added, and the recording remains in Review.

Calendar, Reminders, Notifications, Bluetooth, and Notes/Mail/Contacts Automation all use native
macOS prompts. Integrations that cannot complete setup through an in-context prompt are excluded
from the default catalogue. The user may still add permission-free Shortcuts, HTTPS webhooks, and
text-file workflows.

## Native and implementation boundaries

- AppKit owns windows, menus, application lifecycle, and user-facing state.
- UserNotifications belongs to the main app identity.
- EventKit access is reached through an explicit implementation-agnostic contract.
- Calendar and Reminders delivery executes inside the main app bundle identity, so the
  permission granted by the popup is the permission used by the background adapter.
- Rust workers emit versioned facts and commands; they never launch UI applications.
- The native app consumes a stable read model/API and emits review/configuration commands.
- The browser UI is an optional developer surface only and is never opened at login or required for
  routine use.
- Audio, transcripts, ClickHouse, worker health, and LLM details remain local and are presented in
  user language rather than pipeline terminology.

## UX acceptance scenarios

1. With no new recordings, login produces no window, browser, prompt, or notification.
2. With three unseen recordings, login opens one Review window showing three items.
3. A new action-free recording produces a standard banner saying what arrived and "Nothing was
   added"; clicking it selects that recording.
4. An inferred reminder is created without an approval step and its recording stays in Review
   until **Looks Good — Remove from Review** is selected. The receipt shows whether delivery
   succeeded or failed; the corresponding Reminders item is independently verifiable.
5. A user can correct a transcript, replace an NLT transition with another transition that was
   declared at that state, edit its declared natural-language outputs, replay safely, and undo a
   compensated side effect without touching raw data.
6. The menu bar never contains recording text or greyed-out transcript/status pseudo-options.
7. Routine use requires no browser and exposes no worker, ClickHouse, or manifest terminology.
8. A notification is emitted only after every enabled transcript workflow has recorded its result
   or reached the bounded timeout; its action receipt therefore includes user-space OSM effects.
