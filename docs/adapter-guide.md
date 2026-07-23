# Adapter guide

An adapter is a leaf implementation of an inward-facing port. The root contract exposes generic
`SourceAdapter<Artifact>`, `EventStore<Event>`, `Projection<Event, View>`,
`Transcriber<Audio, Transcript>`, `Oracle<Context, Selection>`,
`EffectAdapter<Effect, Receipt>`, and `ReadModelClient<Query, View>` interfaces. Cross-process leaves
use the equivalent versioned envelope messages.

Android applies the same rule as a source-side recorder with `AudioRecorder`,
`PendingAudioStore<Input>`, and `RecordingSender`. A new phone capture or transfer implementation is a composition change; it
must not add media, transport, or device assumptions to the ViewModel or queue contract.

A source adapter declares capabilities, accepts `SourceCommand`, and emits `SourceStatus` or a
`RecordingAvailable` artifact. Protocol identifiers, credentials, pairing rules, transport APIs,
and recovery behavior remain inside the leaf. Never add a device UUID or vendor command to the root
contract. Contract tests must cover discovery, capability negotiation, stable source identity,
sync outcomes, release, retryable failure, and artifacts. A folder adapter and a device adapter
must be interchangeable from the lakehouse's perspective.

In-process Rust leaves implement `SourceAdapter<RecordingAvailable>` and run through
`execute_recording_source<Adapter>`. The watched-folder, command-line application-file, and
authenticated network-upload leaves use that generic driver in production, not only in tests. A
process-isolated adapter carries the equivalent `SourceCommand`, `SourceStatus`, and
`RecordingAvailable` messages and validates every outbound source envelope before writing NDJSON.
The shared validator checks stable adapter/source identity, non-empty artifacts, unique
capabilities, and capability-backed lifecycle states. The Android recorder's Bluetooth leaf emits
the same `RecordingAvailable` message only after an authenticated, encrypted, hash-verified commit;
it does not replicate the server’s device lifecycle or read-model contracts.

Integrations consume semantic effects and return durable, undoable receipts. Platform leaves map
those effects to Calendar, tasks, notifications, files, shortcuts, or webhooks. A missing platform
capability is reported explicitly rather than emulated with an unrelated API.
