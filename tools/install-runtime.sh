#!/usr/bin/env bash
set -euo pipefail

state_root="$HOME/Library/Application Support/PersonalAudioLake"
runtime_root="$state_root/runtime"
model="${PERSONAL_AUDIO_LAKE_MODEL:-mistral-small3.2:latest}"
clickhouse_version="${CAPTAINS_LOG_CLICKHOUSE_VERSION:-26.6.2.63}"
mkdir -p "$HOME/.local/bin" "$state_root/models" "$runtime_root"

for required in curl jq nix; do
  if ! command -v "$required" >/dev/null 2>&1; then
    echo "$required is required to install the local runtime." >&2
    exit 1
  fi
done
if ! command -v ollama >/dev/null 2>&1; then
  echo "Ollama is required for the local 24B routing and summary slots." >&2
  echo "Install and start Ollama, then rerun this command." >&2
  exit 1
fi
if ! ollama show "$model" >/dev/null 2>&1; then
  ollama pull "$model"
fi
show_request="$(jq -cn --arg model "$model" '{model:$model}')"
show_response="$(curl -fsS -H 'Content-Type: application/json' \
  --data "$show_request" http://127.0.0.1:11434/api/show)"
parameter_size="$(jq -r '.details.parameter_size // empty' <<<"$show_response")"
normalized_parameter_size="$(printf '%s' "$parameter_size" | tr '[:lower:]' '[:upper:]')"
case "$normalized_parameter_size" in
  24B|24.0B) ;;
  *)
    echo "Model $model reports parameter size '${parameter_size:-missing}', not the required 24B." >&2
    exit 1
    ;;
esac

if [ ! -x "$HOME/.local/bin/chctl" ]; then
  installer="$(mktemp)"
  curl -fsSL https://clickhouse.com/cli -o "$installer"
  sh "$installer"
  rm -f "$installer"
fi
"$HOME/.local/bin/chctl" local use "$clickhouse_version"
selected_clickhouse="$("$HOME/.local/bin/chctl" local which | jq -r '.version // empty')"
if [ "$selected_clickhouse" != "$clickhouse_version" ]; then
  echo "ClickHouse selected '${selected_clickhouse:-unknown}', not $clickhouse_version." >&2
  exit 1
fi

whisper_link="$runtime_root/whisper-cpp"
nix build --out-link "$whisper_link" nixpkgs#whisper-cpp
ln -sfn "$whisper_link/bin/whisper-cli" "$HOME/.local/bin/whisper-cli"
"$whisper_link/bin/whisper-cpp-download-ggml-model" base.en "$state_root/models"

echo "ClickHouse $clickhouse_version, whisper.cpp, base.en, and the exact 24B model are ready."
