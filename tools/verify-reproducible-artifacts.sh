#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
android="$repo/android"
mac_app="$repo/dist/Captain's Log.app"
android_apk="$android/app/build/outputs/apk/release/app-release-unsigned.apk"

hash_tree() {
  local root="$1"
  (
    cd "$root"
    find . -type f -print0 \
      | LC_ALL=C sort -z \
      | xargs -0 shasum -a 256
  ) | shasum -a 256 | awk '{print $1}'
}

build_once() {
  "$repo/tools/build-app.sh" >/dev/null
  mac_hash="$(hash_tree "$mac_app")"
  (cd "$android" && ./gradlew --no-daemon clean :app:assembleRelease >/dev/null)
  apk_hash="$(shasum -a 256 "$android_apk" | awk '{print $1}')"
}

build_once
first_mac="$mac_hash"
first_apk="$apk_hash"
build_once

if [ "$first_mac" != "$mac_hash" ]; then
  echo "macOS app bytes are not reproducible across consecutive builds" >&2
  exit 1
fi
if [ "$first_apk" != "$apk_hash" ]; then
  echo "Android release APK bytes are not reproducible across clean consecutive builds" >&2
  exit 1
fi

echo "reproducible artifacts: macOS app and unsigned Android release APK verified"
