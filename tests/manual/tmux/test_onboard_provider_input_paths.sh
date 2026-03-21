#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
BIN_PATH="${1:-$ROOT_DIR/target/debug/zeroclaw}"
TMP_ROOT="/tmp/zeroclaw-tmux-onboard-$$"

cleanup() {
  tmux kill-session -t "zc_full_$$_custom" >/dev/null 2>&1 || true
  tmux kill-session -t "zc_update_$$_synthetic" >/dev/null 2>&1 || true
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required for this regression test" >&2
  exit 1
fi

if [[ ! -x "$BIN_PATH" ]]; then
  echo "Building zeroclaw..."
  cargo build --bin zeroclaw >/dev/null
fi

mkdir -p "$TMP_ROOT"

start_onboard_session() {
  local session="$1"
  local config_dir="$2"
  tmux kill-session -t "$session" >/dev/null 2>&1 || true
  tmux new-session -d -x 240 -y 60 -s "$session" \
    "bash \"$ROOT_DIR/tests/manual/tmux/onboard_wrapper.sh\" \"$config_dir\" \"$BIN_PATH\""
  sleep 1
}

paste_value() {
  local session="$1"
  local buffer_name="$2"
  local value="$3"
  tmux set-buffer -b "$buffer_name" "$value"
  tmux paste-buffer -t "$session":0.0 -b "$buffer_name" -p
}

send_enter() {
  local session="$1"
  tmux send-keys -t "$session":0.0 Enter
}

send_key() {
  local session="$1"
  local key="$2"
  tmux send-keys -t "$session":0.0 "$key"
}

capture_recent() {
  local session="$1"
  tmux capture-pane -p -S -80 -t "$session":0.0
}

assert_prompt_value_exact() {
  local session="$1"
  local prompt="$2"
  local value="$3"
  local label="$4"
  local line

  line="$(
    capture_recent "$session" |
      awk -v prompt="$prompt" 'index($0, prompt) { line = $0 } END { if (line != "") print line; else exit 1 }'
  )"

  local actual="${line#*"$prompt"}"
  if [[ "$actual" != "$value" ]]; then
    echo "Unexpected tmux paste rendering for $label" >&2
    echo "Prompt: $prompt" >&2
    echo "Expected: $value" >&2
    echo "Actual line: $line" >&2
    exit 1
  fi
}

run_full_custom_provider_flow() {
  local root="$TMP_ROOT/full"
  local config_dir="$root/config"
  local workspace_path="$root/ws"
  local session="zc_full_$$_custom"
  local base_url="https://e.invalid/v1"
  local api_key="sk-full-a1b2"
  local model="full-model-a1"

  mkdir -p "$root"
  start_onboard_session "$session" "$config_dir"

  send_key "$session" n
  sleep 1

  paste_value "$session" zc_full_workspace "$workspace_path"
  sleep 1
  assert_prompt_value_exact "$session" "  Enter workspace path: " "$workspace_path" "custom workspace path"
  send_enter "$session"
  sleep 1

  for _ in 1 2 3 4 5; do
    send_key "$session" Down
  done
  send_enter "$session"
  sleep 1

  paste_value "$session" zc_full_base_url "$base_url"
  sleep 1
  assert_prompt_value_exact \
    "$session" \
    "  API base URL (e.g. http://localhost:1234 or https://my-api.com): " \
    "$base_url" \
    "custom provider base URL"
  send_enter "$session"
  sleep 1

  paste_value "$session" zc_full_api_key "$api_key"
  sleep 1
  assert_prompt_value_exact \
    "$session" \
    "  API key (or Enter to skip if not needed): " \
    "$api_key" \
    "custom provider API key"
  send_enter "$session"
  sleep 1

  paste_value "$session" zc_full_model "$model"
  sleep 1
  assert_prompt_value_exact \
    "$session" \
    "  Model name (e.g. llama3, gpt-4o, mistral) [default]: " \
    "$model" \
    "custom provider model"
  send_enter "$session"
  sleep 1
}

run_update_custom_model_flow() {
  local root="$TMP_ROOT/update"
  local config_dir="$root/config"
  local session="zc_update_$$_synthetic"
  local api_key="sk-synth-a1b2"
  local model="synthetic-manual-a1"

  mkdir -p "$root"

  env ZEROCLAW_CONFIG_DIR="$config_dir" \
    "$BIN_PATH" onboard --provider openrouter --api-key seed-key --model openai/gpt-5-mini --force >/dev/null

  start_onboard_session "$session" "$config_dir"

  send_enter "$session"
  sleep 1
  send_enter "$session"
  sleep 1

  for _ in 1 2 3; do
    send_key "$session" Down
  done
  send_enter "$session"
  sleep 1

  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14; do
    send_key "$session" Down
  done
  send_enter "$session"
  sleep 1

  paste_value "$session" zc_update_api_key "$api_key"
  sleep 1
  assert_prompt_value_exact \
    "$session" \
    "  Paste your API key (or press Enter to skip): " \
    "$api_key" \
    "provider-only API key"
  send_enter "$session"
  sleep 1

  send_key "$session" Down
  send_enter "$session"
  sleep 1

  paste_value "$session" zc_update_model "$model"
  sleep 1
  assert_prompt_value_exact \
    "$session" \
    "  Enter custom model ID [anthropic/claude-sonnet-4.6]: " \
    "$model" \
    "custom model ID"
  send_enter "$session"
  sleep 1
}

run_full_custom_provider_flow
run_update_custom_model_flow

echo "tmux onboarding provider input paths passed"
