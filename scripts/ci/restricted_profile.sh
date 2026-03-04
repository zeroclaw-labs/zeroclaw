#!/usr/bin/env bash
set -euo pipefail

restricted_home="${RUNNER_TEMP:-/tmp}/zeroclaw-restricted-home-${GITHUB_RUN_ID:-local}-$$"
mkdir -p "$restricted_home"
chmod 500 "$restricted_home"

cleanup() {
  chmod 700 "$restricted_home" 2>/dev/null || true
  rm -rf "$restricted_home" 2>/dev/null || true
}
trap cleanup EXIT

original_home="${HOME:-}"
if [ -z "${CARGO_HOME:-}" ] && [ -n "$original_home" ]; then
  export CARGO_HOME="${original_home}/.cargo"
fi
if [ -z "${RUSTUP_HOME:-}" ] && [ -n "$original_home" ]; then
  export RUSTUP_HOME="${original_home}/.rustup"
fi

export HOME="$restricted_home"
export ZEROCLAW_TEST_RESTRICTED=1
export RUST_TEST_THREADS=1

echo "[restricted-profile] HOME=$HOME"
echo "[restricted-profile] Running capability-aware subset"

cargo test --locked migration::tests::migrate_openclaw_dry_run_skips_missing_default_sources -- --nocapture
cargo test --locked onboard::wizard::tests::persist_workspace_selection_is_non_fatal_when_marker_root_is_invalid -- --nocapture
cargo test --locked providers::anthropic::tests::chat_with_tools_sends_full_history_and_native_tools -- --nocapture
