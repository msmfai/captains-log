# Release process

This repository is prepared as a source snapshot under AGPL-3.0-only with a separately negotiated
commercial-licensing path. The first public release is source only. Production signing and binary
release creation are a separate later process.

## Public-history boundary

Do not mirror or import the private development remote into GitHub. Deleted files, experimental
directions, internal discussion, and obsolete compatibility notes remain visible in Git history even
when the working tree is clean. Create the public repository from one reviewed release snapshot (or
from deliberately sanitized history), then inspect the resulting GitHub file list before publishing.

## Required checks

1. Verify the unmodified `LICENSE`, `LICENSING.md`, and both Harmony Option Five CLA variants.
2. Create the intended GitHub repository without source, make the empty repository public, enable
   private vulnerability reporting, and subscribe a maintainer to security alerts. GitHub exposes
   this setting only for public repositories; do not push the source snapshot until it is enabled
   and verified.
3. Initialize and commit the reviewed source snapshot in a disposable repository, configure only
   the intended GitHub remote, and run `tools/verify-public-history.sh` there.
4. Run `tools/verify-all.sh` from that clean source snapshot on macOS.
5. Review `git ls-files` and confirm that no ignored or generated file is included.
6. Retain the incomplete physical-device checks prominently as limitations in a source-only release.
7. Confirm the public repository URL in the README and package metadata.
8. Tag the reviewed snapshot and publish release notes that explicitly say no binaries are included.

## `0.1.0` source-release inventory

- One reviewed source-snapshot commit and the `v0.1.0` tag;
- GitHub’s generated source archives;
- release notes drawn from [CHANGELOG.md](../CHANGELOG.md), explicitly stating that no binaries are
  included and Android-to-Mac BLE lacks physical-radio validation.

Ad-hoc-signed Mac builds and unsigned Android APKs are verification artifacts and are not part of
this release. Production-signed binaries may be released later only after the signing, notarization,
clean-device installation, physical-radio, and checksum checks in
[the release checklist](release-checklist.md) pass.

## Proposed GitHub metadata

- **Description:** Local-first audio event lake and deterministic automations for macOS, with a
  BLE-only Android recorder.
- **Topics:** `local-first`, `rust`, `macos`, `android`, `bluetooth-le`, `speech-to-text`,
  `automation`, `privacy`.
- Enable Issues and private vulnerability reporting. Keep Discussions and the wiki disabled until
  there is a maintainer plan for them.
- The canonical public repository is `https://github.com/msmfai/captains-log`.
- Set the detected licence to `AGPL-3.0-only` and link GitHub’s licence display to `LICENSE`.
