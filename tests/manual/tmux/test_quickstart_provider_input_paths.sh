#!/usr/bin/env bash
# Exercise zeroclaw quickstart's provider input paths via its non-interactive flags.
set -euo pipefail

BIN="${BIN:-./target/debug/zeroclaw}"
TMPROOT="$(mktemp -d)"
trap 'rm -rf "$TMPROOT"' EXIT

run_case() {
  local label="$1"; shift
  local cfgdir="$TMPROOT/$label"
  mkdir -p "$cfgdir"
  echo "─── $label ───"
  env ZEROCLAW_CONFIG_DIR="$cfgdir" "$BIN" quickstart "$@"
  echo
  echo "→ resulting config.toml:"
  cat "$cfgdir/config.toml" 2>/dev/null || echo "(no config written)"
  echo
}

run_case ollama-local --model-provider ollama --model qwen2.5:7b --agent ollama
run_case openrouter-hosted --model-provider openrouter --model openrouter/auto --api-key dummy-key --agent or
