#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
app="$repo/dist/Captain's Log.app"
contents="$app/Contents"
macos="$contents/MacOS"

cd "$repo"
cargo build --workspace --release
rm -rf "$app"
rm -rf "$repo/dist/Personal Audio Lake.app"
rm -rf "$repo/dist/Pocket Ingest.app"
mkdir -p "$macos" "$contents/Resources"
cp packaging/Info.plist "$contents/Info.plist"
"$repo/tools/generate-third-party-notices.sh" \
  "$contents/Resources/THIRD_PARTY_NOTICES.txt"
cp target/release/personal-audio-lake-app "$macos/PersonalAudioLake"
for binary in personal-audio-lake-pocket personal-audio-lake-bluetooth personal-audio-lake-storage personal-audio-lake-transcription personal-audio-lake-workflows personal-audio-lake-macos-notifications personal-audio-lake-macos-actions personal-audio-lake-services personal-audio-lake-cli; do
  cp "target/release/$binary" "$macos/$binary"
done
chmod 755 "$macos"/*
for binary in "$macos"/*; do
  codesign --force --sign - "$binary"
done
codesign --force --sign - \
  --entitlements packaging/PersonalAudioLake.entitlements \
  "$app"
plutil -lint "$contents/Info.plist"
echo "$app"
