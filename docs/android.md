# Android Bluetooth recorder

The Android app is a recording source for Captain’s Log on a Mac. It is not a library client,
Pocket receiver, workflow editor, or action executor:

```text
microphone recording
  → app-private durable queue
  → authenticated encrypted Bluetooth LE transfer
  → Mac source inbox
  → Mac transcription, library, workflows, and integrations
```

There is no Android Internet permission, server address, API token, or LAN listener. The app asks
for microphone access when **Record** is first tapped. After a recording has been atomically saved,
**Send by Bluetooth** asks for Android’s Nearby devices access and advertises the transfer service.
These are separate native permission prompts. The app does not request location, Calendar,
notifications, contacts, tasks, or broad file access.

Bluetooth LE is the sole Android-to-Mac transport in this release. This is an explicit product
boundary rather than an automatic fallback policy: a failed Bluetooth transfer stays in the
phone's private retry queue and is never redirected through Wi-Fi, a LAN service, or the internet.
The transport remains behind the generic `RecordingSender` port so a future transport can be added
as a separate, deliberately reviewed adapter without changing recording or lakehouse behavior.

The always-running Mac service scans for the Captain’s Log Android transfer service. Its manifest and
control attributes require authenticated encrypted Bluetooth access, so the operating systems own
pairing and link encryption. The first transfer may therefore show native pairing confirmation on
the phone and Mac. The Mac initiates each transfer; the phone never scans nearby devices. The exact
UUIDs, byte layout, retry rules, and trust boundary are documented under
[Bluetooth sources](bluetooth.md#android-recorder-transfer).

Each recording announces a random recording ID, stable random phone ID, byte length, timestamp, and
SHA-256 digest. The Mac requests a starting byte offset, allowing an interrupted transfer to resume
from its private partial file. Frames carry explicit offsets. The Mac rejects gaps, excess bytes,
changed metadata, incorrect length, or a digest mismatch. Only after flushing, verifying, and
atomically renaming the recording into its source inbox does the Mac send a commit receipt. The
phone then marks the queue entry sent and removes its local audio file. A failed or interrupted
transfer remains private and queued for another attempt.

This first version transfers only while the Android app is open and keeps the screen awake during
recording and transfer. It deliberately does not add a foreground service or notification
permission. BLE transfer throughput varies substantially by phone, Mac, radio conditions, and
negotiated MTU, so long recordings may take noticeably longer to transfer.

The strict MVVM boundary remains: `MainActivity` renders and maps semantic permission intents to
system popups; `MainViewModel` owns the record/save/send state machine without Android imports;
`AudioRecorder`, `PendingAudioStore<Input>`, and `RecordingSender` are inward-facing ports; Android
media, SQLite/files, and GATT live in leaf adapters selected only by `AppComposition`.

## Build and verify

Install Android SDK 35 and JDK 17 or newer. The checked-in Gradle wrapper honours `ANDROID_HOME`
or `ANDROID_SDK_ROOT`; on macOS and Linux it also discovers Android Studio's standard SDK directory.
An optional untracked `android/local.properties` may define `sdk.dir` for a nonstandard location.

```sh
cd android
./gradlew :app:testDebugUnitTest :app:assembleDebug :app:lintDebug
tools/verify-android-device.sh
```

Emulators can verify the private queue and UI permission boundary but cannot establish this real
BLE peripheral transfer. Public support requires a physical Android phone and the target Mac to
exercise first pairing, full transfer, interruption/resume, duplicate commit, Bluetooth-off
recovery, and multi-recording order.

Uninstalling deletes recordings still queued on the phone and its random source identity. It does
not affect the Mac lake or Pocket pairing state.
