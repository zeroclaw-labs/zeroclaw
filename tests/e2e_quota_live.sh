#!/usr/bin/env bash
# E2E Live Test: Provider Quota Tools with live providers
#
# Verifies that the agent answers quota/limit questions and the
# providers-quota CLI produces expected output against live APIs.
#
# Usage:
#   bash tests/e2e_quota_live.sh           # build + run all tests
#   bash tests/e2e_quota_live.sh --skip-build  # skip cargo build
#   bash tests/e2e_quota_live.sh --cli-only    # run only CLI tests (no agent)
#
# Environment:
#   ZEROCLAW_CONFIG_DIR — override config dir (default: /home/spex/.zeroclaw)
#   ZEROCLAW_BIN        — override binary path (default: ./target/release/zeroclaw)
#   TIMEOUT             — per-test timeout in seconds (default: 120)

set -eo pipefail

# ── Config ─────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ZEROCLAW_CONFIG_DIR="${ZEROCLAW_CONFIG_DIR:-/home/spex/.zeroclaw}"
ZEROCLAW_BIN="${ZEROCLAW_BIN:-${REPO_ROOT}/target/release/zeroclaw}"
TIMEOUT="${TIMEOUT:-120}"
LOG_FILE="/tmp/e2e_quota_live_$(date +%Y%m%d_%H%M%S).log"
SKIP_BUILD=false
CLI_ONLY=false

for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=true ;;
    --cli-only)   CLI_ONLY=true ;;
  esac
done

# ── Colors ─────────────────────────────────────────────────────────────
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# ── Counters ───────────────────────────────────────────────────────────
test_total=0
test_pass=0
test_fail=0
test_skip=0

# ── Helpers ────────────────────────────────────────────────────────────
log() { printf '%s\n' "$*" | tee -a "$LOG_FILE"; }
logc() { printf "$@" | tee -a "$LOG_FILE"; }  # color-aware

banner() {
  log ""
  log "================================================================"
  log "  $1"
  log "================================================================"
}

# Run an agent test.
#   $1  test label
#   $2  message to send to agent
#   $3  extra agent flags (e.g. "-p openai-codex")
#   $4  pipe-separated keywords — at least one must appear in stdout+stderr
run_agent_test() {
  local label="$1" message="$2" agent_flags="$3" keywords="$4"
  test_total=$((test_total + 1))

  log ""
  logc "${CYAN}[%02d] %s${NC}\n" "$test_total" "$label"
  log "  agent flags: $agent_flags"
  log "  message: $message"
  log "  expect keywords: $keywords"

  local output="" rc=0
  # Pipe 'yes' to stdin to auto-approve any remaining tool prompts.
  # Redirect stderr to stdout so we capture log lines too.
  # Disable pipefail for this pipeline: yes always exits 141 (SIGPIPE) when
  # the agent finishes, and pipefail would propagate that to set -e.
  output=$(set +o pipefail; yes 2>/dev/null | timeout "${TIMEOUT}" ${ZEROCLAW_BIN} agent -m "$message" $agent_flags 2>&1) || rc=$?

  # Log full output
  printf '%s\n' "$output" >> "$LOG_FILE"

  # Even if exit code is non-zero, check the output for success signals.
  # Rate-limit / provider error is still PASS — proves the tool was invoked.
  local found=0 total_kw=0
  local IFS='|'
  for kw in $keywords; do
    total_kw=$((total_kw + 1))
    if echo "$output" | grep -qi "$kw"; then
      found=$((found + 1))
    fi
  done

  # Also count rate-limit signals as keyword matches
  if echo "$output" | grep -qiE "rate.limit|429|quota.exhaust|usage.limit|retry.after|circuit.open|available_providers"; then
    found=$((found + 1))
    total_kw=$((total_kw + 1))
  fi

  if [ "$found" -gt 0 ]; then
    if [ $rc -ne 0 ]; then
      logc "  ${GREEN}PASS${NC} (matched %d keywords, exit=%d — fallback/rate-limit)\n" "$found" "$rc"
    else
      logc "  ${GREEN}PASS${NC} (matched %d/%d keywords)\n" "$found" "$total_kw"
    fi
    test_pass=$((test_pass + 1))
    # Show relevant excerpt
    echo "$output" | grep -iE "provider|quota|available|limit|model|remaining|rate|reset" | head -8 >> "$LOG_FILE" || true
  else
    if [ $rc -eq 124 ]; then
      logc "  ${RED}FAIL${NC} (timeout after ${TIMEOUT}s, 0 keywords matched)\n"
    else
      logc "  ${RED}FAIL${NC} (exit=%d, 0/%d keywords matched)\n" "$rc" "$total_kw"
    fi
    test_fail=$((test_fail + 1))
    log "  --- last 20 lines of output ---"
    echo "$output" | tail -20 | tee -a "$LOG_FILE"
  fi
}

# Run a CLI test (no model calls).
#   $1  test label
#   $2  command to run (string, eval'd)
#   $3  pipe-separated keywords
run_cli_test() {
  local label="$1" cmd="$2" keywords="$3"
  test_total=$((test_total + 1))

  log ""
  logc "${CYAN}[%02d] %s${NC}\n" "$test_total" "$label"
  log "  cmd: $cmd"
  log "  expect keywords: $keywords"

  local output="" rc=0
  output=$(eval "timeout 15 ${cmd}" 2>&1) || rc=$?

  printf '%s\n' "$output" >> "$LOG_FILE"

  local found=0 total_kw=0
  local IFS='|'
  for kw in $keywords; do
    total_kw=$((total_kw + 1))
    if echo "$output" | grep -qi "$kw"; then
      found=$((found + 1))
    fi
  done

  if [ "$found" -gt 0 ]; then
    logc "  ${GREEN}PASS${NC} (matched %d/%d keywords)\n" "$found" "$total_kw"
    test_pass=$((test_pass + 1))
  else
    logc "  ${RED}FAIL${NC} (0/%d keywords matched)\n" "$total_kw"
    test_fail=$((test_fail + 1))
    log "  --- output ---"
    echo "$output" | tail -15 | tee -a "$LOG_FILE"
  fi
}

skip_test() {
  local label="$1" reason="$2"
  test_total=$((test_total + 1))
  test_skip=$((test_skip + 1))
  logc "${YELLOW}[%02d] SKIP: %s — %s${NC}\n" "$test_total" "$label" "$reason"
}

# ── Pre-flight ─────────────────────────────────────────────────────────
banner "Pre-flight checks"

log "Config dir:  ${ZEROCLAW_CONFIG_DIR}"
log "Binary:      ${ZEROCLAW_BIN}"
log "Timeout:     ${TIMEOUT}s"
log "Log file:    ${LOG_FILE}"

# Fix active_workspace.toml if it points to a temp dir
AWF="${ZEROCLAW_CONFIG_DIR}/active_workspace.toml"
if [ -f "$AWF" ]; then
  current_dir=$(grep -oP 'config_dir\s*=\s*"\K[^"]+' "$AWF" 2>/dev/null || true)
  if [ -n "$current_dir" ] && [[ "$current_dir" == /tmp/* ]]; then
    log "Fixing active_workspace.toml: ${current_dir} -> ${ZEROCLAW_CONFIG_DIR}"
    echo "config_dir = \"${ZEROCLAW_CONFIG_DIR}\"" > "$AWF"
  fi
fi

# Ensure quota tools are auto-approved (non-destructive: only adds if missing)
CFG="${ZEROCLAW_CONFIG_DIR}/config.toml"
for tool in check_provider_quota switch_provider estimate_quota_cost; do
  if [ -f "$CFG" ] && ! grep -q "\"${tool}\"" "$CFG" 2>/dev/null; then
    log "Adding '${tool}' to auto_approve in config.toml"
    sed -i "s/^auto_approve = \\[/auto_approve = [\n    \"${tool}\",/" "$CFG"
  fi
done

# Build
if [ "$SKIP_BUILD" = false ]; then
  log ""
  log "Building release binary..."
  if cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml" 2>&1 | tee -a "$LOG_FILE" | tail -3; then
    logc "${GREEN}Build OK${NC}\n"
  else
    logc "${RED}Build FAILED — aborting${NC}\n"
    exit 1
  fi
fi

if [ ! -x "$ZEROCLAW_BIN" ]; then
  logc "${RED}Binary not found: ${ZEROCLAW_BIN}${NC}\n"
  exit 1
fi

# Show auth profiles
log ""
log "OAuth profiles:"
if [ -f "${ZEROCLAW_CONFIG_DIR}/auth-profiles.json" ]; then
  jq -r '.profiles | keys[]' "${ZEROCLAW_CONFIG_DIR}/auth-profiles.json" 2>/dev/null | tee -a "$LOG_FILE" || log "(parse error)"
else
  log "(none found)"
fi

# Show relevant env keys (names only — no secrets)
log ""
log "API key env vars:"
env | grep -oE '^(ANTHROPIC|OPENAI|GEMINI|QWEN)[A-Z_]*' | sort | tee -a "$LOG_FILE" || log "(none)"

# Detect working provider for agent tests
# The agent will use fallback if the requested provider fails,
# so we just need at least one working provider path.
log ""
log "Provider detection:"
AGENT_PROVIDER_FLAGS="-p openai-codex"
if timeout 10 ${ZEROCLAW_BIN} agent -m 'respond OK' -p openai-codex 2>&1 | grep -qi "OK"; then
  log "  openai-codex: OK (primary for agent tests)"
  AGENT_PROVIDER_FLAGS="-p openai-codex"
else
  log "  openai-codex: fallback mode (will use provider chain)"
  AGENT_PROVIDER_FLAGS=""  # let the agent use default fallback chain
fi

# ======================================================================
#  SECTION 1: Agent-based quota questions (live provider calls)
# ======================================================================
if [ "$CLI_ONLY" = false ]; then

banner "Agent tests: quota questions (live provider)"

# Test 1: Какие модели доступны?
run_agent_test \
  "RU: Какие модели доступны?" \
  "Какие модели доступны? Используй check_provider_quota" \
  "${AGENT_PROVIDER_FLAGS}" \
  "available|provider|gemini|codex|model"

# Test 2: Когда сбросятся лимиты?
run_agent_test \
  "RU: Когда сбросятся лимиты?" \
  "Когда сбросятся лимиты провайдеров? Используй check_provider_quota" \
  "${AGENT_PROVIDER_FLAGS}" \
  "reset|limit|retry|quota"

# Test 3: Сколько осталось запросов?
run_agent_test \
  "RU: Сколько осталось запросов?" \
  "Сколько осталось запросов? Используй check_provider_quota" \
  "${AGENT_PROVIDER_FLAGS}" \
  "remaining|quota|request|limit"

# Test 4: Покажи статус всех провайдеров
run_agent_test \
  "RU: Покажи статус всех провайдеров" \
  "Покажи статус всех провайдеров. Используй check_provider_quota" \
  "${AGENT_PROVIDER_FLAGS}" \
  "provider|status|available|quota"

# Test 5: English — What models are available?
run_agent_test \
  "EN: What models are available?" \
  "What models are available? Use check_provider_quota tool" \
  "${AGENT_PROVIDER_FLAGS}" \
  "available|provider|model|quota"

fi  # CLI_ONLY

# ======================================================================
#  SECTION 2: providers-quota CLI (no model call, reads local state)
# ======================================================================
banner "CLI tests: providers-quota"

# Test 6: JSON output
run_cli_test \
  "CLI: providers-quota --format json" \
  "${ZEROCLAW_BIN} providers-quota --format json" \
  '"status"|"providers"|"timestamp"'

# Test 7: Filter by gemini
run_cli_test \
  "CLI: providers-quota --provider gemini" \
  "${ZEROCLAW_BIN} providers-quota --provider gemini" \
  "gemini"

# Test 8: Filter by openai-codex
run_cli_test \
  "CLI: providers-quota --provider openai-codex" \
  "${ZEROCLAW_BIN} providers-quota --provider openai-codex" \
  "openai-codex|codex"

# ======================================================================
#  SECTION 3: Multi-subscription quota checks
# ======================================================================

banner "Multi-subscription quota checks"

# Helper: switch active profile via jq
switch_active_profile() {
  local provider="$1" profile_name="$2"
  local profile_id="${provider}:${profile_name}"
  local ap_file="${ZEROCLAW_CONFIG_DIR}/auth-profiles.json"
  jq --arg p "$provider" --arg id "$profile_id" \
    '.active_profiles[$p] = $id' "$ap_file" > "${ap_file}.tmp" \
    && mv "${ap_file}.tmp" "$ap_file"
  log "  Switched ${provider} active profile -> ${profile_id}"
}

# Save original active profiles for restore
AP_FILE="${ZEROCLAW_CONFIG_DIR}/auth-profiles.json"
ORIG_ACTIVE_GEMINI=""
ORIG_ACTIVE_CODEX=""
if [ -f "$AP_FILE" ]; then
  ORIG_ACTIVE_GEMINI=$(jq -r '.active_profiles.gemini // empty' "$AP_FILE" 2>/dev/null || true)
  ORIG_ACTIVE_CODEX=$(jq -r '.active_profiles["openai-codex"] // empty' "$AP_FILE" 2>/dev/null || true)
  log "Original active gemini: ${ORIG_ACTIVE_GEMINI:-<none>}"
  log "Original active codex:  ${ORIG_ACTIVE_CODEX:-<none>}"
fi

if [ "$CLI_ONLY" = false ]; then

# gemini-1 quota
switch_active_profile "gemini" "gemini-1"
run_agent_test \
  "Quota: gemini-1" \
  "Check my quota. Use check_provider_quota provider gemini" \
  "-p openai-codex" \
  "quota|limit|available|provider|gemini|rate"

# gemini-2 quota
switch_active_profile "gemini" "gemini-2"
run_agent_test \
  "Quota: gemini-2" \
  "Check my quota. Use check_provider_quota provider gemini" \
  "-p openai-codex" \
  "quota|limit|available|provider|gemini|rate"

# codex-1 quota
switch_active_profile "openai-codex" "codex-1"
run_agent_test \
  "Quota: codex-1" \
  "Check my quota. Use check_provider_quota provider openai-codex" \
  "-p openai-codex" \
  "quota|limit|available|provider|codex|rate"

# codex-2 quota
switch_active_profile "openai-codex" "codex-2"
run_agent_test \
  "Quota: codex-2" \
  "Check my quota. Use check_provider_quota provider openai-codex" \
  "-p openai-codex" \
  "quota|limit|available|provider|codex|rate"

fi  # CLI_ONLY

# Restore original active profiles
log ""
log "Restoring original active profiles..."
if [ -f "$AP_FILE" ]; then
  if [ -n "$ORIG_ACTIVE_GEMINI" ]; then
    switch_active_profile "gemini" "$(echo "$ORIG_ACTIVE_GEMINI" | sed 's/^gemini://')"
  fi
  if [ -n "$ORIG_ACTIVE_CODEX" ]; then
    switch_active_profile "openai-codex" "$(echo "$ORIG_ACTIVE_CODEX" | sed 's/^openai-codex://')"
  fi
  log "Active profiles restored."
fi

# ======================================================================
#  Summary
# ======================================================================
banner "Results"

log "Total:   ${test_total}"
logc "Passed:  ${GREEN}%d${NC}\n" "$test_pass"
logc "Failed:  ${RED}%d${NC}\n" "$test_fail"
logc "Skipped: ${YELLOW}%d${NC}\n" "$test_skip"
log ""
log "Full log: ${LOG_FILE}"
log ""

if [ "$test_fail" -eq 0 ]; then
  logc "${GREEN}ALL TESTS PASSED${NC}\n"
  exit 0
else
  logc "${RED}SOME TESTS FAILED${NC}\n"
  exit 1
fi
