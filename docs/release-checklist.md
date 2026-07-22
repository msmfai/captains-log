# Public release checklist

- [x] AGPL-3.0-only is present as the exact official text; Rust metadata, dual-licensing notice, and
      Harmony Option Five Individual and Entity CLAs name Solomonoff Consultancy Ltd.
- [x] GitHub private vulnerability reporting, dependency alerts, automated security fixes, and a
      maintainer notification subscription are enabled on `msmfai/captains-log`.
- [x] Public GitHub history starts from the reviewed release snapshot (or deliberately sanitized
      history), not by mirroring the private development remote;
      `tools/verify-public-history.sh` passes in that publication repository.
- [x] The canonical GitHub source URL is present in the README and every Rust package's metadata.
- [x] `tools/verify-all.sh` passes from a clean checkout. See the dated verification report.
- [x] Consecutive clean artifact builds pass `tools/verify-reproducible-artifacts.sh` with identical
      macOS app-tree and unsigned Android release-APK hashes.
- [x] macOS app installs, launches at login, and reports model/storage readiness.
- [x] `tools/verify-uninstall-preserves-data.sh` removes app/agent fixtures in an isolated home while
      preserving byte-checked private lake data and recovery material.
- [x] `tools/verify-non-pocket-ingest.sh` proves watched-folder ingestion reaches the immutable lake
      with the device adapter disabled.
- [x] Pocket discover/pair/transfer/release/no-reclaim/re-pair was hardware-tested on the unchanged
      compatibility leaf; discovery and sync were repeated after the source-neutral composition and
      binary rename. The release does not needlessly repeat the destructive lifecycle during each build.
- [x] Replacement Android recorder debug/release variants build and lint; a clean emulator install
      cold-launches and requests only the native microphone popup when Record is tapped.
- [ ] Before a binary release, a physical Android microphone capture pairs and transfers a real
      recording to the Mac by BLE.
      Until this passes, do not describe Android-to-Mac BLE as physically verified.
- [ ] Before a binary release, the maintainer selects the supported physical Android API/device
      policy and
      `tools/verify-android-device.sh` passes on it. The current source/storage/UI contract passed on
      all three attached API 35 arm64 emulators.
- [ ] Before a binary release, a physical Android transfer survives interruption, resumes by offset,
      rejects corruption, and
      recovers after Bluetooth is turned off and on. Deterministic codec/state tests pass but do not
      establish radio behavior.
- [x] Secrets, personal paths/data, captures, diagnostic commands, generated artifacts, and dirty
      output gates pass.
- [x] Captain’s Log is the public display name; stable legacy storage, bundle, database, and command
      identifiers are documented as internal upgrade compatibility rather than public branding.
- [x] The non-affiliation statement and recording-consent notice remain present and factually
      accurate for the release.
- [x] Documentation links and third-party notices pass.
- [x] README navigation, changelog, conduct/security/contribution policies, troubleshooting,
      release process, issue forms, pull-request template, and dependency-update configuration exist.
- [x] Release notes separate verified behavior, blockers, and maintainer decisions.
- [ ] Before a binary release, production macOS/Android signing identities are supplied and the
      final signed artifacts match the approved release inventory and published
      SHA-256 checksums.
- [ ] Before a binary release, the final macOS, Android, and web interfaces link to the public
      corresponding source and licence.
