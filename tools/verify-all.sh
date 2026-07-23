#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
before="$(mktemp)"
after="$(mktemp)"
notices="$(mktemp)"
trap 'rm -f "$before" "$after" "$notices"' EXIT

cd "$repo"
git status --porcelain=v1 > "$before"
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
tools/check-architecture.sh
tools/check-public-release.sh
tools/generate-third-party-notices.sh "$notices"
cmp THIRD_PARTY_NOTICES.txt "$notices"
tools/verify-reproducible-artifacts.sh
tools/build-app.sh >/dev/null
plutil -lint "dist/Captain's Log.app/Contents/Info.plist" >/dev/null
codesign --verify --deep --strict "dist/Captain's Log.app"
tools/verify-non-pocket-ingest.sh
tools/verify-uninstall-preserves-data.sh
(cd android && ./gradlew --no-daemon \
  :app:testDebugUnitTest :app:assembleDebug :app:assembleDebugAndroidTest :app:lintDebug \
  :app:testReleaseUnitTest :app:assembleRelease :app:lintRelease)
git status --porcelain=v1 > "$after"
cmp "$before" "$after"
echo "verification: Rust, architecture, notices, macOS package, Android package, and dirty-output gates passed"
