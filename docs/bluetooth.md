# Bluetooth sources

Captain’s Log currently uses Bluetooth through two independent leaf adapters. The Android recorder
uses a project-owned Bluetooth Low Energy service to send recordings to the Mac. Pocket compatibility
uses that recorder’s own device-source adapter. Their identifiers, framing, trust state, and recovery
rules are unrelated; neither is part of the lake or workflow contracts.

## Android recorder transfer

The Android app is the GATT peripheral and advertises only while **Send by Bluetooth** is active. The
always-running Mac ingress leaf is the central. It scans for the Captain’s Log service, connects,
reads one manifest, requests an offset, receives offset-framed bytes, verifies the complete object,
and acknowledges the commit. The Android queue removes its private audio file only after that
acknowledgement.

The first wire version uses these project-owned UUIDs:

| Purpose | UUID | GATT contract |
|---|---|---|
| Service | `325ed7d9-14d2-4bae-ac92-ebf257933eb2` | Connectable Android advertisement |
| Control | `325ed7da-14d2-4bae-ac92-ebf257933eb2` | Mac writes start or commit commands |
| Manifest | `325ed7db-14d2-4bae-ac92-ebf257933eb2` | Mac reads the current recording metadata |
| Data | `325ed7dc-14d2-4bae-ac92-ebf257933eb2` | Android notifies offset-framed audio bytes |

The manifest is exactly 85 bytes, with all integers in network byte order:

```text
0..4    ASCII "CLB1"
4       protocol version (1)
5..21   stable random Android source UUID
21..37  random recording UUID
37..45  unsigned audio byte length
45..53  signed Unix recording time in milliseconds
53..85  SHA-256 of the complete audio object
```

A start command is byte `1`, the 16-byte recording UUID, then the requested unsigned 64-bit offset.
Each data notification is byte `1`, its unsigned 64-bit offset, then one or more audio bytes. A
commit command is byte `2`, the recording UUID, then the verified 32-byte SHA-256. Unknown versions,
sizes above 4 GiB, empty objects, changed metadata, gaps, excess bytes, and digest mismatches fail
closed. Until Android reports a negotiated ATT MTU, the sender uses the 23-byte protocol default;
larger frames are used only after a platform MTU callback. A partial is flushed below private
application state and reused on the next connection. A
corrupt complete partial is deleted so the retry begins at byte zero. The final object is renamed
atomically into the generic source inbox before `RecordingAvailable` is emitted and before the Mac
writes the commit command.

The manifest, control characteristic, and notification subscription require authenticated encrypted
Bluetooth access with man-in-the-middle protection. This means Android and macOS own pairing, link
encryption, and any native confirmation UI. The protocol has no internet account, API token, cloud
service, or Pocket credential. It does not silently designate every paired device as trusted: the
phone advertises only during a user-initiated send, and the protected GATT access may require the
user to confirm native pairing. This first release has no separate Captain’s Log application-level
allow-list, so physical-device testing must verify the exact pairing UI and bond behavior on each
supported Android/macOS combination before claiming support.

Android and Rust tests share fixed binary compatibility vectors for the manifest, start command,
data frame, and commit command. Rust additionally tests malformed frames, bounded sizes, exact
non-zero-offset resume, gap rejection without appending, corrupt-partial recovery, repeated commit
after a lost acknowledgement, and conformance of the resulting recording to the generic source
contract. Android tests enforce the safe default and negotiated-MTU payload sizes. Those tests prove
codec and state-transition behavior, not radio interoperability. See [Android](android.md) for the
remaining physical test matrix.

## Pocket compatibility sync

Pocket support is the first device-source adapter for Captain’s Log. The
background service scans for Pocket, connects with CoreBluetooth, verifies the
expected service and characteristic properties, and emits recordings through
the same source-neutral contracts that future device, application, folder, and
local-service adapters will use.
Temporary CoreBluetooth initialization failures keep the leaf in a degraded retrying state rather
than terminating the supervisor. It reports ready only after the system adapter is actually open.

On first run the app generates a random binding credential and stores it in
macOS Keychain. The ordinary application configuration never contains the key.
Put Pocket in pairing mode, then use **Sync Now**. Successful authentication
pins the recorder's CoreBluetooth identifier so later scans ignore other nearby
devices.

## Binding and removal warning

Pocket retains this key on the recorder. This is not ordinary macOS Bluetooth
pairing, and forgetting a Bluetooth device in macOS does not necessarily clear
the recorder's application-level binding. Use **Settings → Release source**
before pairing it with another client. Release first authenticates with the
Keychain credential, requests the recorder-side reset, disconnects, and clears
the locally pinned device identifier. The recovery credential remains in
Keychain so a failed or interrupted release can be retried after reinstalling
the same signed app.

Do not delete the Keychain item manually. If the recorder still does not show its
pairing indication after release, keep the app and key installed, wake the
recorder, and retry **Release source**. Losing the Mac Keychain while the recorder
is still bound can make the device appear bricked and may require vendor support.
See the full [compatibility notice](compatibility.md).

After authentication, the compatibility leaf lists recording folders, lists each folder,
and downloads files that are not already present. It does not delete recordings,
change recorder settings, contact a remote service, or expose a raw command
interface. Device release is a separate explicit lifecycle action and never
removes the saved recovery key automatically.

Each download is written to a temporary file. The app checks the announced byte
length, flushes the data, and atomically renames the completed MP3. Failed and
interrupted downloads are removed and retried later.
