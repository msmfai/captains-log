# Release verification

This report describes the `0.1.0` release candidate as verified on 2026-07-23. It records current
evidence, not development history. Run `tools/verify-all.sh` to reproduce the automated checks.

## Automated evidence

- Rust formatting and clippy pass with warnings denied. All 129 workspace tests pass.
- The strict crate-tree, dependency-inversion, source-contract, MVVM, permission, and transport
  architecture checks pass.
- Seven Android JVM tests pass. Debug and release variants build, lint, and package with SDK 35.
- The Gradle 8.14.4 distribution and checked-in wrapper JAR match their pinned upstream SHA-256
  values.
- Consecutive clean builds produce byte-identical ad-hoc-signed macOS app trees and unsigned Android
  release APKs.
- The macOS package structure, property list, entitlements, bundled executables, notices, and strict
  ad-hoc signature verification pass.
- Watched-folder ingestion reaches the immutable manifest and content-addressed store with no
  compatibility device configured.
- The isolated uninstall test removes application and LaunchAgent fixtures while preserving
  byte-checked lake data, API identity, and source-recovery material.
- Public-surface checks reject credentials, personal paths or data, recordings, databases, generated
  packages, forbidden diagnostic surfaces, stale product branding, broken documentation links,
  source-specific core coupling, and Android network permission or client code.
- The AGPL-3.0-only file is byte-identical to the official GNU text. Workspace packages declare the
  same SPDX expression, and the commercial-licensing and Harmony Option Five contribution paths are
  explicit.
- A disposable 116-file, one-commit source snapshot with no remote passed the public-history gate and
  the complete release gate from clean build caches. It was removed after verification.
- The reproducible unsigned Android release APK SHA-256 is
  `4f5899d81c9998d9178f4d7de4bd590cd1ef49fa5a91da2268307b14a46c1ae0`; the reproducible macOS
  app-tree SHA-256 is `3e6521b238948973c62635db2b073edb4ef7635385fcb0ae1ab9ac4fdf1064d2`.
- The complete gate leaves the caller's working-tree state unchanged.
- Before source upload, the empty public `msmfai/captains-log` repository had private vulnerability
  reporting, dependency alerts, automated security fixes, and maintainer notifications enabled;
  Issues were enabled and the wiki and Discussions were disabled.

## Runtime evidence

- The candidate installs as a per-user macOS login application without changing existing lake data
  or compatibility-source selection. Matching immutable manifests skip lakehouse replay.
- The authenticated local health endpoint reported all eight current-generation roles ready:
  optional source, recorder transfer ingress, lake, transcription, derivation, Calendar,
  notifications, and workflow services.
- Settings presents the always-on recorder-app transfer independently from the optional compatibility
  device. The transfer role reported `encrypted local recording transfer ready`.
- The runtime validates ClickHouse, ffmpeg, whisper.cpp and its model, and the exact local 24B model
  before claiming readiness.
- The rebuilt Android recorder declares only microphone and Bluetooth permissions. APK manifest
  inspection confirms that it does not request Internet access.
- Three API 35 arm64 emulators passed the Android private-storage framework contract. A clean install
  requested microphone permission only after Record was selected, captured non-empty M4A audio, and
  retained it in the private send queue.

## Compatibility evidence

- Pocket discovery, first binding, authenticated transfer, reconnect, explicit release, no automatic
  reclaim, explicit re-pair, Keychain recovery, and reinstall recovery were exercised on hardware.
  Discovery and sync were repeated after the source-neutral composition and binary rename; the leaf
  protocol and recovery behavior were unchanged.
- Pocket, Android BLE, watched-folder, command-line file, and authenticated upload producers pass the
  same generic source lifecycle and artifact conformance contract.
- Android and Rust share fixed binary vectors for the BLE manifest, start, offset-framed data, and
  commit messages. Tests cover bounded sizes, gap and corruption rejection, exact-offset resume,
  idempotent commit, single-central admission, safe MTU sizing, and durable retry state.

## Deferred physical evidence

The Android recorder has not yet transferred a recording over a real Android-to-Mac BLE link.
Pairing UI, interruption/resume over radio, Bluetooth-off recovery, corruption handling over the
link, and multiple-recording ordering therefore remain unverified on physical hardware. Codec and
state-machine tests do not replace that evidence, and release material must not claim otherwise.

## Deferred binary-release decisions

Before releasing binaries, choose the supported physical Android/macOS matrix, complete deferred
  Bluetooth radio validation, supply production signing identities, complete macOS notarization,
and link the public corresponding source and licence from each interactive application.

The first public release is source only. No production-signed artifact is claimed by this report.
