#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
source_app="$repo/dist/Captain's Log.app"
install_root="$HOME/Applications"
installed_app="$install_root/Captain's Log.app"
legacy_installed_app="$install_root/Personal Audio Lake.app"
state_root="$HOME/Library/Application Support/PersonalAudioLake"
legacy_state_root="$HOME/Library/Application Support/PocketIngest"
agent="$HOME/Library/LaunchAgents/com.smfmarin.personal-audio-lake.plist"
clickhouse_agent="$HOME/Library/LaunchAgents/com.smfmarin.personal-audio-lake.clickhouse.plist"
chctl="$HOME/.local/bin/chctl"
domain="gui/$(id -u)"
lakehouse_version=1
lakehouse_marker="$state_root/lakehouse-version"
had_install=false
if [ -d "$installed_app" ] || [ -d "$legacy_installed_app" ]; then
  had_install=true
fi

"$repo/tools/build-app.sh"
if [ ! -x "$chctl" ]; then
  echo "Missing $chctl; install the official ClickHouse CLI first." >&2
  exit 1
fi
mkdir -p "$install_root" "$HOME/Library/LaunchAgents"
if [ ! -e "$state_root" ] && [ -d "$legacy_state_root" ]; then
  mv "$legacy_state_root" "$state_root"
fi
mkdir -p "$state_root/logs" "$state_root/models" "$state_root/lake"
if [ ! -e "$state_root/source-inbox" ] && [ -d "$state_root/bluetooth-inbox" ]; then
  mv "$state_root/bluetooth-inbox" "$state_root/source-inbox"
fi
if [ -f "$state_root/config.toml" ]; then
  chmod 600 "$state_root/config.toml"
fi
rm -rf "$installed_app"
cp -R "$source_app" "$installed_app"
sed \
  -e "s|__EXECUTABLE__|$installed_app/Contents/MacOS/PersonalAudioLake|g" \
  -e "s|__STATE_ROOT__|$state_root|g" \
  -e "s|__WHISPER_BIN__|$HOME/.local/bin/whisper-cli|g" \
  "$repo/packaging/com.smfmarin.personal-audio-lake.plist" > "$agent.tmp"
mv "$agent.tmp" "$agent"
sed \
  -e "s|__CHCTL__|$chctl|g" \
  -e "s|__STATE_ROOT__|$state_root|g" \
  "$repo/packaging/com.smfmarin.personal-audio-lake.clickhouse.plist" > "$clickhouse_agent.tmp"
mv "$clickhouse_agent.tmp" "$clickhouse_agent"
plutil -lint "$agent"
plutil -lint "$clickhouse_agent"

stop_agent() {
  local label="$1"
  local main_pid=""
  local process_group=""
  if launchctl print "$domain/$label" >/dev/null 2>&1; then
    main_pid="$(
      launchctl print "$domain/$label" |
        sed -n 's/^[[:space:]]*pid = \([0-9][0-9]*\)$/\1/p' |
        head -1
    )"
    if [ -n "$main_pid" ]; then
      process_group="$(ps -o pgid= -p "$main_pid" 2>/dev/null | tr -d '[:space:]')"
    fi
    launchctl bootout "$domain/$label"
    for _ in {1..50}; do
      if ! launchctl print "$domain/$label" >/dev/null 2>&1; then
        break
      fi
      sleep 0.1
    done
    if launchctl print "$domain/$label" >/dev/null 2>&1; then
      echo "LaunchAgent $label did not finish unloading" >&2
      exit 1
    fi
  fi
  if [ -n "$process_group" ] && kill -0 -- "-$process_group" 2>/dev/null; then
    kill -TERM -- "-$process_group" 2>/dev/null || true
    for _ in {1..50}; do
      if ! kill -0 -- "-$process_group" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    if kill -0 -- "-$process_group" 2>/dev/null; then
      kill -KILL -- "-$process_group" 2>/dev/null || true
    fi
  fi
}

replace_agent() {
  local label="$1"
  local plist="$2"
  stop_agent "$label"
  launchctl bootstrap "$domain" "$plist"
}

launchctl bootout "$domain/com.smfmarin.pocket-ingest" 2>/dev/null || true
launchctl bootout "$domain/com.smfmarin.pocket-ingest.clickhouse" 2>/dev/null || true
rm -f \
  "$HOME/Library/LaunchAgents/com.smfmarin.pocket-ingest.plist" \
  "$HOME/Library/LaunchAgents/com.smfmarin.pocket-ingest.clickhouse.plist"
rm -rf "$HOME/Applications/Pocket Ingest.app"

stop_agent com.smfmarin.personal-audio-lake
rm -rf "$legacy_installed_app"
replace_agent com.smfmarin.personal-audio-lake.clickhouse "$clickhouse_agent"
for _ in {1..120}; do
  if curl -fsS 'http://127.0.0.1:8123/?query=SELECT%201' >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
if ! curl -fsS 'http://127.0.0.1:8123/?query=SELECT%201' >/dev/null; then
  echo "ClickHouse did not become ready on 127.0.0.1:8123" >&2
  exit 1
fi
needs_rebuild=true
manifest_count=0
if [ -f "$state_root/lake/manifests/events.ndjson" ]; then
  manifest_count="$(wc -l < "$state_root/lake/manifests/events.ndjson" | tr -d '[:space:]')"
fi
lakehouse_count="$(curl -fsS -G \
  --data-urlencode 'query=SELECT count() FROM personal_audio_lake.ingest_events' \
  'http://127.0.0.1:8123/' 2>/dev/null | tr -d '[:space:]' || true)"
marker_version="$(sed -n '1p' "$lakehouse_marker" 2>/dev/null || true)"
if [[ "$manifest_count" =~ ^[0-9]+$ ]] \
  && [ "$manifest_count" = "$lakehouse_count" ] \
  && { [ "$marker_version" = "$lakehouse_version" ] \
    || { [ -z "$marker_version" ] && [ "$had_install" = true ]; }; }; then
  needs_rebuild=false
fi
if [ "$needs_rebuild" = true ]; then
  "$installed_app/Contents/MacOS/personal-audio-lake-storage" \
    --lake-root "$state_root/lake" rebuild
else
  echo "Lakehouse already matches the immutable manifest; rebuild skipped"
fi
printf '%s\n' "$lakehouse_version" > "$lakehouse_marker.tmp"
mv "$lakehouse_marker.tmp" "$lakehouse_marker"
replace_agent com.smfmarin.personal-audio-lake "$agent"
launchctl enable "$domain/com.smfmarin.personal-audio-lake.clickhouse"
launchctl enable "$domain/com.smfmarin.personal-audio-lake"
health=""
api_bind="$(sed -n 's/^api_bind = "\([^"]*\)"$/\1/p' "$state_root/config.toml" | head -1)"
if [ -z "$api_bind" ]; then
  api_bind="127.0.0.1:32145"
fi
case "$api_bind" in
  127.0.0.1:*|localhost:*) health_url="http://$api_bind/api/v1/health" ;;
  \[::1\]:*) health_url="http://$api_bind/api/v1/health" ;;
  *)
    echo "Cannot validate non-loopback api_bind $api_bind" >&2
    exit 1
    ;;
esac
# CoreBluetooth initialization is bounded to 20 seconds per adapter attempt and retries after a
# failure. Give a cold post-upgrade process enough time for several real attempts; readiness still
# has a hard deadline and every other worker must independently report ready.
readiness_deadline=$((SECONDS + 90))
while [ "$SECONDS" -lt "$readiness_deadline" ]; do
  if [ -s "$state_root/api-token" ]; then
    IFS= read -r token < "$state_root/api-token"
    health="$(curl -fsS \
      -H "Authorization: Bearer $token" \
      "$health_url" 2>/dev/null || true)"
    if jq -e '.status == "ready" and (.workers | length == 8) and all(.workers[]; .state == "ready")' \
      >/dev/null 2>&1 <<<"$health"; then
      break
    fi
  fi
  sleep 0.5
done
if ! jq -e '.status == "ready" and all(.workers[]; .state == "ready")' \
  >/dev/null 2>&1 <<<"$health"; then
  echo "Captain's Log installed but runtime readiness validation failed:" >&2
  jq -r '.storage, .workers[] | "\(.role // "storage"): \(.state // .status) — \(.detail // .error // "unavailable")"' \
    <<<"$health" >&2 2>/dev/null || true
  exit 1
fi
echo "Installed $installed_app"
