# Troubleshooting

## Installation does not become ready

The installer waits for every required worker instead of treating a running process as healthy.
Read the reported worker and fix that dependency:

- **lake** — check that the ClickHouse local service starts and its configured ports are free;
- **transcription** — verify `ffmpeg`, `whisper-cli`, and the configured model file;
- **derive**, **notify**, or **services** — start Ollama and verify the configured model reports 24B;
- **source** — turn on Bluetooth or disable the optional compatibility adapter;
- **transfer_ingress** — turn on Bluetooth and retry after other Bluetooth software has finished
  initializing CoreBluetooth.

Logs are stored under `~/Library/Application Support/PersonalAudioLake/logs/`. Redact paths,
recording text, source identifiers, tokens, and credentials before sharing any excerpt.

## The Android recording remains queued

Open Captain’s Log on Android, choose **Send by Bluetooth**, and keep the app open. Confirm Nearby
devices access if Android asks. Bluetooth must be enabled on both devices and the Mac background app
must report **Recorder App Transfer — Ready**. A failed transfer remains in the private phone queue;
retrying resumes from the offset retained by the Mac.

The first release does not use Wi-Fi or the internet as a fallback. Physical Android/macOS support
is provisional until the combinations listed in the support matrix have been tested.

## A Pocket recorder will not pair or appears unavailable

Pocket uses application-level binding rather than an ordinary removable macOS Bluetooth pairing.
Do not delete the Captain’s Log Keychain recovery item. Wake the recorder and retry **Sync Now**. To
move it to another client, use **Settings → Release source** and verify the recorder returns to its
pairing indication before removing the app. See [Compatibility](compatibility.md) for the recovery
boundary.

## A recording arrived but no action ran

Open Review and inspect the transcript, accepted evidence, and workflow receipt. An action-free
recording is valid. You can correct the transcript, manually run an enabled automation, or add/edit
an action. Long or ambiguous recordings are deliberately prevented from silently creating effects.

## A native integration did not run

Captain’s Log requests each macOS permission only when the corresponding integration is used. A
denied capability remains unavailable and the recording stays in Review. The app does not direct
you into System Settings. Use another enabled integration or retry after the operating system makes
the permission available.

## Removing the application

Release any bound compatibility recorder first, then run `tools/uninstall-user.sh`. The script
removes the application and LaunchAgents while retaining the Mac lake and recovery credentials.
Android uninstall deletes recordings that have not yet transferred from that phone.
