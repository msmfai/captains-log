# Captain’s Log

Captain’s Log is local-first software for collecting personal event streams into
a raw data lake and running user-owned automations over them. Sources, storage, models,
integrations, and clients are replaceable adapters. The persistent macOS server
owns the raw lake, rebuildable ClickHouse lakehouse, library, and workflows. The
Android app is a recorder source that durably sends audio to that Mac server.

Captain’s Log is independently developed and is not made, sponsored,
endorsed, or supported by Open Vision Engineering, Inc. “Pocket” is used only
to identify compatibility with that company's recorder. Compatibility may vary
by device and firmware version.

No production audio, transcript, model, credential, or lake data belongs in this
repository.

The canonical public source repository is
[github.com/msmfai/captains-log](https://github.com/msmfai/captains-log).

## What is implemented

- Source-neutral recording, transcript, workflow, action, and delivery contracts
  for devices, apps, files, and local or network services. Every ingestion leaf
  is checked against the same generic source lifecycle and artifact contract.
- USB-volume and opt-in watched-folder ingestion as recovery paths.
- Folder, file, Android-recorder, and authenticated network ingestion are independent of the
  optional compatibility-device adapter and report their readiness separately.
- SHA-256 content-addressed audio, immutable transcription runs with exact
  provider payloads, and an fsynced NDJSON event manifest.
- A disposable ClickHouse projection for recordings, transcript segments,
  annotations, proposed actions, reviews, and delivery receipts.
- Local `whisper.cpp` transcription; no cloud transcription API.
- Deterministic fuzzy triggers, payload extraction, date handling, identities,
  evidence, idempotency, and policy for explicit spoken commands.
- Optional implicit/contextual routing through the installed local
  `mistral-small3.2:latest` model. Its metadata is checked to be 24B; implicit
  rules are disabled by default and, when enabled, remain bounded by the same
  deterministic delivery and undo policy.
- An authenticated local API and GUI for transcript search, synchronized audio,
  non-destructive corrections, action review, health, rules, and service creation.
- A native macOS menu-bar status item with direct access to the Inbox and app
  preferences; it lives in the persistent supervisor process.
- User-owned, bounded automations with independent checkpoints, typed local-model
  slots, human correction, receipts, and compensating undo actions.
- Reversible native delivery to Calendar, Reminders, Notes, Mail drafts, Contacts,
  and `Documents/Captain's Log`; Shortcuts connects Messages, Home, Maps,
  Music, Safari, and other apps, while HTTPS webhooks connect external services.
- A launcher app plus per-user LaunchAgents that keep the local server and its
  private ClickHouse process running at login. Opening the app opens the GUI.
- A mechanically enforced strict crate tree. See [architecture](docs/architecture.md).
- A strict-MVVM Android recorder with microphone capture, app-private atomic staging, a resumable
  authenticated encrypted Bluetooth transfer, and no Internet permission. Bluetooth LE is the
  only Android-to-Mac transport in this release; failures remain queued privately on the phone.
  It contains no Pocket receiver, library, hooks, or platform actions. See [Android](docs/android.md) and
  the [Bluetooth wire contract](docs/bluetooth.md#android-recorder-transfer), plus the factual
  [support matrix](docs/support-matrix.md).

## Build and install

The macOS host requires macOS 14 or newer, Apple command-line tools, the pinned Rust toolchain,
Nix, `jq`, and Ollama. A 24B local model requires substantially more memory than a small utility
app; verify that your Mac can run it before enabling model-backed workflows. Install and start
Ollama first. The runtime installer then obtains the exact local 24B
`mistral-small3.2:latest` model, the official per-user ClickHouse CLI and tested ClickHouse
`26.6.2.63` server, a Nix `whisper.cpp`
binary, and the English `base.en` model before building the background app:

```sh
tools/install-runtime.sh
tools/install-user.sh
```

Set `CAPTAINS_LOG_CLICKHOUSE_VERSION` only when deliberately testing another ClickHouse release;
the runtime installer verifies the selected version before continuing.

This installs the app at `~/Applications/Captain's Log.app`, LaunchAgents
under `~/Library/LaunchAgents`, and private state at:

```text
~/Library/Application Support/PersonalAudioLake/
├── config.toml
├── api-token
├── rules.json
├── services/
├── service-state/
├── source-inbox/
├── calendar/{pending,delivered}/
├── notifications/status.json
├── control/{inbox,processed}/
├── lake/
│   ├── .clickhouse/servers/personal-audio-lake/data/
│   ├── manifests/events.ndjson
│   ├── objects/audio/
│   └── objects/transcripts/
├── logs/
├── models/ggml-base.en.bin
└── status/events.ndjson
```

The app creates `config.toml` on first launch. `config.example.toml` documents
all fields. The raw manifest and object store are canonical; ClickHouse is only
the rebuildable lakehouse projection. Existing `PersonalAudioLake` state, bundle,
database, and command identifiers remain stable internal compatibility identifiers
to avoid stranding an existing lake, receipts, source recovery keys, or permission identity.

Installation succeeds only after the current process generation reports ready for Bluetooth,
storage, transcription executable/model, the exact local 24B routing/summary/workflow model,
EventKit, notifications, and services. The installer reports the failing leaf instead of showing
an optimistic ready label. Matching update installs skip lakehouse replay; mismatches rebuild from
the immutable manifest while the supervisor is stopped.

Open `~/Applications/Captain's Log.app` for the GUI, or visit
`http://127.0.0.1:32145/`. See [server API and services](docs/server-api.md) and
the [data model](docs/data-model.md).

To remove the background app and LaunchAgents without destroying the private lake or source
recovery credentials:

```sh
tools/uninstall-user.sh
```

Release any Mac-bound compatibility source first. Android uninstall affects only recordings still
queued on that phone; details are in [Android](docs/android.md) and
[Compatibility](docs/compatibility.md).

## Mac integrations

Create an automation from the native **Automations** screen, describe when it is
relevant, and choose its destination. The app uses fixed state-machine topology;
the local 24B model may only choose a declared transition or fill its declared
text fields. A person can edit the same fields after delivery.

Calendar and Reminders use EventKit. Notes, saved Mail drafts, and Contacts use
the corresponding Mac app and store the native object's identifier only in the
private workflow receipt. Text exports use unique files below
`~/Documents/Captain's Log`. These native effects are undoable from the
recording's History and are replaced transactionally when corrected. Mail
integration saves a draft and never sends it.

Use a named Apple Shortcut for Messages, Home, Maps, Music, Safari, Finder, Focus,
or another Shortcut-capable app. The app passes structured recording and workflow
data to `shortcuts run`; an optional second Shortcut can implement undo. Webhooks
are restricted to HTTPS except for explicit loopback development addresses.

macOS requests Calendar, Reminders, Notifications, or per-app Automation access
only when the corresponding capability is first used. The app remains useful when
an unrelated permission is denied: it reports that nothing was added, leaves the
recording in Review, and never sends the user to System Settings.

## Compatibility: Pocket

Pocket is the first supported hardware adapter, not the product architecture.
On a clean install, open **Settings**, choose **Pocket recorder (Compatibility)**, and then put the
recorder in pairing mode. The background service finds the
recorder, creates a random binding key for this Mac, binds it, remembers that
recorder, and downloads unseen recordings. No phone credential is required.

> **Compatibility warning:** Pocket stores the binding key on the recorder.
> The app keeps the matching recovery key in macOS Keychain, separately from its
> database and configuration. Before moving the recorder to another phone or
> computer, choose **Settings → Release source**. If the recorder is unavailable,
> uninstalling the app leaves that recovery key intact so reinstalling the same
> signed app can release it later. Losing both the Mac Keychain and the recorder's
> binding state may require vendor support. See the
> [compatibility notice](docs/compatibility.md).

```sh
"$HOME/Applications/Captain's Log.app/Contents/MacOS/personal-audio-lake-cli" scan
"$HOME/Applications/Captain's Log.app/Contents/MacOS/personal-audio-lake-cli" status --lines 100
```

macOS asks for Bluetooth permission on first use. If Pocket rejects the Mac's
key, put the recorder back in pairing mode and use **Sync Source** again. See
[Bluetooth sync](docs/bluetooth.md).

Users are responsible for giving any notice and obtaining any consent required
before recording, transcribing, or acting on another person's speech. See
[privacy](docs/privacy.md).

## Spoken actions

Default deterministic phrases include “Remind me to call Jess tomorrow at
9am,” “Add buy milk to my todo,” and “For my calendar add dentist next Tuesday
at 11am.” Minor transcription errors in the trigger are tolerated. Exact and
fuzzy matches stay in Rust; command-like near misses use the local 24B model only
as a constrained routing judge. Enable the implicit rule only for contextual
proposals; those always enter review.

## Review and correct processing

```sh
personal-audio-lake-cli pending
personal-audio-lake-cli rederive RECORDING_UUID
```

The GUI opens to a chronological Review queue when the persistent server starts at
login. Unambiguous enabled actions run immediately; they are not held waiting for
approval. Each recording remains in the Inbox until the user marks its processing report
as correct, and shows the local-24B summary, transcript state, selected hooks, delivery
receipts, undo/correction controls, and related processing failures. Every
arrival also produces a persistent native macOS alert, including action-free
recordings reported as `Hooks: none`. Confirm the processing report with the
alert's `Looks right` action, or enter a mismatch note from the alert. A bundled
UserNotifications helper requests alert and sound permission, reports the effective
macOS settings, and schedules delivery after the helper exits so macOS cannot
suppress it as a foreground notification. The model only summarizes what was
heard; hook outcomes come from durable receipts. `personal-audio-lake-cli` remains available for
recovery inspection and `personal-audio-lake-cli rederive RECORDING_UUID` replay. The Actions
screen requests Calendar and Reminders separately through the signed helper app.

## Development checks

```sh
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
tools/verify-all.sh
tools/verify-non-pocket-ingest.sh
```

The Android build is described in [docs/android.md](docs/android.md). Adapter
authors should start with the [adapter guide](docs/adapter-guide.md).

## Documentation

- Using the product: [Android recorder](docs/android.md),
  [supported capabilities](docs/support-matrix.md), [privacy](docs/privacy.md),
  [troubleshooting](docs/troubleshooting.md), and
  [Pocket compatibility and recovery](docs/compatibility.md).
- Extending it: [architecture](docs/architecture.md), [data model](docs/data-model.md),
  [server API](docs/server-api.md), [adapter guide](docs/adapter-guide.md), and
  [native experience contract](docs/native-experience.md).
- Releasing it: [changelog](CHANGELOG.md), [release process](docs/releasing.md),
  [release checklist](docs/release-checklist.md), and
  [verification evidence](docs/verification-report.md). The first source-only release is described
  in the [0.1.0 release notes](docs/release-notes-0.1.0.md).
- Project policies: [contributing](CONTRIBUTING.md), [security](SECURITY.md),
  [code of conduct](CODE_OF_CONDUCT.md), [licensing](LICENSING.md), and
  [third-party notices](THIRD_PARTY_NOTICES.txt).

## Licensing

Captain’s Log is free software under
[GNU AGPL version 3 only](LICENSE). Solomonoff Consultancy Ltd may separately negotiate commercial
licences for organisations that need different terms. Contributions require the applicable
Harmony Option Five CLA. See [LICENSING.md](LICENSING.md) for the exact boundaries.
