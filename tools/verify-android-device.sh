#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
require_physical=false
case "${1:-}" in
  "") ;;
  --require-physical) require_physical=true ;;
  -h|--help)
    echo "usage: $0 [--require-physical]"
    echo "Runs framework contract tests on every attached adb target."
    exit 0
    ;;
  *)
    echo "unknown argument: $1" >&2
    echo "usage: $0 [--require-physical]" >&2
    exit 2
    ;;
esac

adb start-server >/dev/null
devices="$(adb devices | awk 'NR > 1 && $2 == "device" { print $1 }')"
if [ -z "$devices" ]; then
  echo "Android device verification requires at least one adb device or emulator" >&2
  exit 1
fi

physical=0
while IFS= read -r serial; do
  [ -n "$serial" ] || continue
  # adb shell may otherwise consume the serial inventory backing this loop.
  model="$(adb -s "$serial" shell getprop ro.product.model </dev/null | tr -d '\r')"
  api="$(adb -s "$serial" shell getprop ro.build.version.sdk </dev/null | tr -d '\r')"
  qemu="$(adb -s "$serial" shell getprop ro.kernel.qemu </dev/null | tr -d '\r')"
  kind="physical"
  if [ "$qemu" = "1" ]; then
    kind="emulator"
  else
    physical=$((physical + 1))
  fi
  echo "android target: serial=$serial kind=$kind api=$api model=$model"
done <<< "$devices"

if $require_physical && [ "$physical" -eq 0 ]; then
  echo "physical Android target required; attached targets are emulators" >&2
  exit 1
fi

cd "$repo/android"
./gradlew --no-daemon :app:connectedDebugAndroidTest
echo "android recorder contracts: private audio queue and completion cleanup passed"
echo "scope: emulators do not verify encrypted BLE pairing, transfer, interruption/resume, or Bluetooth-off recovery"
