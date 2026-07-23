#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d /private/tmp/personal-audio-lake-folder.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT

state="$scratch/state"
watch="$scratch/watch"
lake="$state/lake"
mkdir -p "$watch"
printf 'generated non-pocket audio fixture\n' > "$watch/recording.m4a"

cat > "$scratch/config.toml" <<EOF
lake_root = "$lake"
state_root = "$state"
clickhouse_url = "http://127.0.0.1:1"
api_bind = "127.0.0.1:0"
manifest_only = true
source_adapter_id = ""
source_adapter_binary = ""
allow_source_pairing = false
scan_interval_seconds = 30
scan_duration_ms = 1000
source_idle_timeout_ms = 1000
source_sync_interval_seconds = 60
timezone = "Europe/London"
watched_folders = ["$watch"]
usb_folders = []
EOF

cd "$repo"
cargo build --release --workspace >/dev/null
target/release/personal-audio-lake-app --config "$scratch/config.toml" --once

manifest="$lake/manifests/events.ndjson"
test -s "$manifest"
recording_count="$(jq -s '[.[] | select(.type == "recording_available" and .payload.adapter_id == "watched-folder")] | length' "$manifest")"
audio_count="$(jq -s '[.[] | select(.type == "audio_ingested" and .payload.source_id == $source)] | length' \
  --arg source "$watch" "$manifest")"
if [ "$recording_count" -ne 1 ] || [ "$audio_count" -ne 1 ]; then
  echo "watched-folder ingress did not produce exactly one raw and one ingested event" >&2
  exit 1
fi
relative="$(jq -rs '[.[] | select(.type == "audio_ingested")][-1].payload.relative_path' "$manifest")"
cmp "$watch/recording.m4a" "$lake/$relative"
if ! jq -e -s 'any(.[]; .event.type == "worker_health" and .event.payload.role == "source" and
    .event.payload.state == "ready" and
    (.event.payload.detail | contains("Optional source adapter disabled")) and
    (.event.payload.detail | contains("folder")))' \
    "$state/status/events.ndjson" >/dev/null; then
  echo "folder-only runtime did not report honest source readiness" >&2
  exit 1
fi
if jq -e -s 'any(.[]; .type == "worker_health")' "$manifest" >/dev/null; then
  echo "ephemeral worker readiness leaked into the immutable user lake" >&2
  exit 1
fi

echo "non-Pocket ingress: watched folder reached the immutable lake without a device adapter"
