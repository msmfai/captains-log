# Captain’s Log 0.1.0

Captain’s Log is a local-first personal audio lake and deterministic automation system. This first
public release contains source code only; it does not include downloadable macOS or Android
binaries.

## Included

- A persistent macOS host with an immutable raw lake, rebuildable ClickHouse projections, local
  transcription, a native review interface, and reversible user-defined automations.
- Source-neutral ingestion contracts with watched-folder and command-line paths.
- An optional Pocket compatibility adapter isolated at the architecture boundary.
- A strict-MVVM Android recorder that queues recordings privately and implements a resumable,
  authenticated Bluetooth LE transfer protocol without requesting Internet access.
- AGPL-3.0-only licensing, contributor terms, security reporting instructions, architecture and
  adapter documentation, and reproducible verification tooling.

## Verification and limitations

The complete local gate passes, including 129 Rust tests, Android unit/build/lint checks,
architecture and privacy checks, clean watched-folder ingestion, uninstall data preservation, and
reproducible unsigned verification artifacts.

Android-to-Mac transfer has deterministic cross-language protocol coverage and emulator coverage,
but has not yet been exercised over a physical Android-to-Mac Bluetooth radio link. Do not interpret
this source release as a claim that the Android transfer is hardware verified. The macOS and Android
verification packages are not production signed and are intentionally not attached to this release.

See the [verification report](verification-report.md), [support matrix](support-matrix.md), and
[compatibility notice](compatibility.md) for the precise evidence and boundaries.
