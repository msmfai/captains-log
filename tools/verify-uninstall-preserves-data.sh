#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d /private/tmp/personal-audio-lake-uninstall.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT

home="$scratch/home"
fake_bin="$scratch/bin"
state="$home/Library/Application Support/PersonalAudioLake"
mkdir -p \
  "$fake_bin" \
  "$home/Applications/Captain's Log.app" \
  "$home/Applications/Personal Audio Lake.app" \
  "$home/Library/LaunchAgents" \
  "$home/Library/Keychains" \
  "$state/lake/manifests"

printf '#!/usr/bin/env bash\nexit 0\n' > "$fake_bin/launchctl"
chmod 755 "$fake_bin/launchctl"
printf 'fixture agent\n' > "$home/Library/LaunchAgents/com.smfmarin.personal-audio-lake.plist"
printf 'fixture clickhouse agent\n' \
  > "$home/Library/LaunchAgents/com.smfmarin.personal-audio-lake.clickhouse.plist"
printf 'immutable personal event\n' > "$state/lake/manifests/events.ndjson"
printf 'private API token\n' > "$state/api-token"
printf 'source recovery material\n' > "$home/Library/Keychains/source-recovery.fixture"

output="$(HOME="$home" PATH="$fake_bin:$PATH" "$repo/tools/uninstall-user.sh")"

for removed in \
  "$home/Applications/Captain's Log.app" \
  "$home/Applications/Personal Audio Lake.app" \
  "$home/Library/LaunchAgents/com.smfmarin.personal-audio-lake.plist" \
  "$home/Library/LaunchAgents/com.smfmarin.personal-audio-lake.clickhouse.plist"; do
  if [ -e "$removed" ]; then
    echo "uninstaller retained application-owned path: $removed" >&2
    exit 1
  fi
done

if [ "$(cat "$state/lake/manifests/events.ndjson")" != "immutable personal event" ] ||
   [ "$(cat "$state/api-token")" != "private API token" ] ||
   [ "$(cat "$home/Library/Keychains/source-recovery.fixture")" != "source recovery material" ]; then
  echo "uninstaller changed private lake data or source recovery material" >&2
  exit 1
fi
if [[ "$output" != *"Private lake data and source recovery credentials were preserved."* ]]; then
  echo "uninstaller did not explain its preservation behavior" >&2
  exit 1
fi

echo "uninstall contract: application files removed; private lake and recovery material preserved"
