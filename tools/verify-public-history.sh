#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"

if [ "$#" -ne 0 ]; then
  echo "usage: $0" >&2
  exit 2
fi

commit_count="$(git rev-list --all --count)"
root_count="$(git rev-list --all --max-parents=0 | wc -l | tr -d '[:space:]')"
if [ "$commit_count" != 1 ] || [ "$root_count" != 1 ]; then
  echo "first public history must contain exactly one reviewed source-snapshot commit" >&2
  echo "found $commit_count commits and $root_count roots; do not mirror private development history" >&2
  exit 1
fi

if git remote -v | rg -qi 'gitlab|personal-data|pocket-ingest'; then
  echo "private development remote metadata is still configured" >&2
  exit 1
fi

historical_paths="$(git log --all --name-only --format= | sort -u)"
if rg -n '\.(aac|flac|m4a|mp3|ogg|opus|wav|sqlite3?|db|parquet|lance|ndjson|jsonl|apk|aab|dmg|ipa|pkg|jks|keystore|p12|mobileprovision)$' \
    <<<"$historical_paths"; then
  echo "public history contains a private-data, credential, or generated-artifact path" >&2
  exit 1
fi

tools/check-public-release.sh
echo "public history: one reviewed snapshot with no private remote or forbidden historical paths"
