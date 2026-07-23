# Architecture

The workspace dependency graph is a strict tree. `personal-audio-lake-contracts` is the root
abstraction crate; every executable crate depends on it and on no other workspace
crate. `tools/check-architecture.sh` enforces that graph from Cargo metadata.

```text
personal-audio-lake-contracts
├── personal-audio-lake-pocket
├── personal-audio-lake-bluetooth
├── personal-audio-lake-storage
├── personal-audio-lake-transcription
├── personal-audio-lake-workflows
├── personal-audio-lake-macos-actions
├── personal-audio-lake-macos-notifications
├── personal-audio-lake-services
├── personal-audio-lake-app
└── personal-audio-lake-cli
```

Workers communicate using versioned NDJSON envelopes on standard input/output.
Each worker tests the shared contract fixtures independently. The supervisor owns
process lifecycle and routing, but contains no leaf implementation. Thus a
source, storage, STT, extraction, or integration implementation can be replaced
without importing another concrete crate.

## Application MVVM boundary

The macOS application behavior is renderer-independent:

```text
AppKit view ─────┐
                 ├──> ViewModel <── read-model port
Reference view ─┘         │
                           v
                    semantic effects
                           │
                           v
                    controller ──> local API and platform leaves
```

`view_model.rs` is a pure reducer and presentation model. It imports no UI, filesystem, HTTP,
process, or macOS framework API. Views bind presentation values and turn input into semantic
intents; they never receive lakehouse records. `app_controller.rs` resolves effects through the
authenticated local API, versioned command inbox, and platform adapters. The deterministic text
view exercises the same ViewModel and controller as AppKit, making navigation and state changes
testable without duplicating application rules.

The Android recorder follows the same boundary. Its Activity renders presentation state and
requests permissions; its ViewModel owns record, save, queue, and send behavior through
`AudioRecorder`, `PendingAudioStore`, and `RecordingSender` ports. Android media, SQLite, files, and
Bluetooth APIs exist only in composition-root leaf implementations.

Rust relationships that remain inside a process are compile-time interfaces, not naming
conventions. The watched-folder, command-line file, and authenticated network leaves implement
`SourceAdapter<RecordingAvailable>` and are invoked through the generic
`execute_recording_source<Adapter>` driver. Worker stdin is a generic
`MessageTransport<Envelope>`, the immutable manifest implements `EventStore<Envelope>`, and the
local transcription leaf implements `Transcriber<AudioIngested, TranscriptCompleted>`. Human and
local-model state-machine choices both enter through `Oracle<Context, WorkflowSelection>`; the
generic slot owns neither implementation and deterministic validation still constrains the result.
The EventKit leaf implements `EffectAdapter<ActionProposal, NativeEffectReceipt>` and is called
through generic apply/compensating-undo functions. The native application's authenticated local-API
leaf implements `ReadModelClient<(), AppSnapshot>` and
`ReadModelClient<Uuid, Vec<AppSegment>>`; reqwest and token storage therefore remain behind the
client boundary rather than entering the controller or view model.
The process-isolated device leaf cannot share an in-memory trait object, so its stdout boundary checks
every source-status and recording envelope through the identical `validate_source_contract`
contract before emission. Device, folder, application, and network producers therefore preserve
stable identities and capability-backed lifecycle states before the lake sees their output.

A compatibility-device worker is optional. With empty device-adapter identity/binary fields, the
supervisor does not start that worker; watched-folder, command-line file, authenticated network,
and Android-recorder ingress remain independent. `tools/verify-non-pocket-ingest.sh`
runs that mode through the actual supervisor and storage process, proving a watched file reaches the
immutable manifest and content-addressed object store without loading a compatibility adapter.
Empty adapter fields are also the clean-install default. The outer application composition owns a
small catalog that maps a generic source-selection request to a packaged leaf executable. The
ViewModel receives only option identity, name, description, and selection state; the native and
text views select by index, and neither the source protocol nor executable name enters the model.
Selecting an option persists its generic adapter identity, starts the leaf, and initiates discovery.
When a configured Bluetooth leaf cannot initialize immediately, it reports a single degraded
health fact and retries inside its own process. Transient adapter startup therefore cannot crash or
couple the rest of the server tree to device availability. When both Bluetooth leaves are enabled,
the supervisor starts the transfer-ingress leaf only after the compatibility-source leaf reports its
adapter ready. The source-neutral health message serializes cold CoreBluetooth initialization; the
two sibling crates do not share a lock, import one another, or race separate manager enumeration.

The same dependency direction is explicit in the Android recorder. `AudioRecorder`,
`PendingAudioStore<Input>`, and `RecordingSender` live in its model-facing contract package. Android media APIs,
SQLite/files, and encrypted GATT live in leaf adapters. The ViewModel receives only those
interfaces; the Activity receives only presentation records and semantic permission intents. The
composition root is the sole location that binds implementations. Android never reads the Mac
library or executes workflows, so no read-model, review, effect, or Pocket contract is present;
Bluetooth remains entirely inside the replaceable `RecordingSender` leaf.

The Mac’s `personal-audio-lake-bluetooth` leaf independently scans the Android transfer service,
resumes offset-framed data into a private partial, verifies announced length and SHA-256, atomically
commits it to the source inbox, and only then returns a commit receipt. It emits the same generic
`RecordingAvailable` contract as every other source and contains all transport UUIDs and framing.
The core sees this leaf only as the source-neutral `transfer_ingress` health role. Initialization,
scan, transfer, and disconnect operations are bounded; degraded transport state therefore blocks
installer readiness instead of hiding behind a live process.
The native read-model leaf queries that generic health projection as well as the inbox projection.
Its ViewModel presents recorder-app transfer independently from the optional compatibility-device
lifecycle, without learning the transport UUIDs, executable name, or device protocol.

The supervisor is the always-running server and process owner. It is the only
client-facing route to source controls, raw data, lakehouse queries, rules,
reviews, and services. The UI is a disposable client with no database or device
access; its frontend calls only `/api/v1`.

The append-only path is:

```text
Source adapter
  → RecordingAvailable
  → content-addressed audio + AudioIngested
  → local STT + TranscriptCompleted
  → deterministic tolerant workflow prefilter
  → schema-bounded human or local-24B oracle slot + deterministic effect
  → semantic integration adapter + ActionDelivered
  → terminal workflow traces + receipt-aware local-24B summary + native UserNotification
  → user audit + RecordingReviewed
```

Projected messages live in `lake/manifests/events.ndjson`. Audio bytes live under
`lake/objects/audio/<hash-prefix>` and complete transcription runs under
`lake/objects/transcripts/`. These immutable objects are the data lake. ClickHouse
is the disposable lakehouse. A correction is a new annotation, never a mutation.
Periodic source probes still drive deterministic sync scheduling, but only observable source-state
changes cross into the immutable manifest. Transient discovery and identical ready heartbeats are
not user data and are not journaled. Worker readiness is process-generation state held only in the
bounded, startup-truncated status journal; it never enters the immutable personal-event lake.

User workflows independently tail the durable manifest with their own cursors and receipts. The
default **Mac Actions** workflow first bounds automatic routing to short personal voice notes, then
uses one deterministic tolerant trigger before bounded oracle slots. Long conversational
recordings remain manually routable but cannot auto-fire actions. Reminder, calendar, and todo run
as sequential model-or-human slots, so distinct clauses in one message may trigger several hooks.
Deterministic validation between slots reserves each accepted quote, preventing the same clause
from being reinterpreted by later integrations. Every effect requires a verbatim evidence quote;
code verifies the quote against the transcript and requires future due
times to be tied to a quoted time phrase before emitting an action. One broken integration cannot
couple itself to a source adapter, transcription, the GUI, or another service.

`personal-audio-lake-macos-notifications` independently consumes the transcript, workflow, action, and delivery contract
stream. It waits for the legacy terminal marker, every enabled transcript workflow, and approved
delivery receipts, with a bounded timeout. The 24B model may summarize transcript text, but
notification action names and statuses are constructed only from typed messages. The main app
AppKit process owns the macOS UserNotifications permission and delivery boundary.
