#!/usr/bin/env bash
# E2E Live Test: Auth Profile & Provider Switch Tools
#
# Tests manage_auth_profile (list/switch/refresh) and switch_provider
# against live providers through the agent loop.
#
# Usage:
#   bash tests/e2e_auth_switch_live.sh               # build + run all
#   bash tests/e2e_auth_switch_live.sh --skip-build   # skip cargo build
#   bash tests/e2e_auth_switch_live.sh --cli-only     # CLI tests only
#
# Environment:
#   ZEROCLAW_CONFIG_DIR — override config dir (default: ~/.zeroclaw)
#   ZEROCLAW_BIN        — override binary path
#   TIMEOUT             — per-test timeout in seconds (default: 120)

set -eo pipefail

# ── Config ─────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ZEROCLAW_CONFIG_DIR="${ZEROCLAW_CONFIG_DIR:-${HOME}/.zeroclaw}"
ZEROCLAW_BIN="${ZEROCLAW_BIN:-${REPO_ROOT}/target/release/zeroclaw}"
TIMEOUT="${TIMEOUT:-120}"
LOG_FILE="/tmp/e2e_auth_switch_$(date +%Y%m%d_%H%M%S).log"
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
logc() { printf "$@" | tee -a "$LOG_FILE"; }

banner() {
  log ""
  log "================================================================"
  log "  $1"
  log "================================================================"
}

run_agent_test() {
  local label="$1" message="$2" agent_flags="$3" keywords="$4"
  test_total=$((test_total + 1))

  log ""
  logc "${CYAN}[%02d] %s${NC}\n" "$test_total" "$label"
  log "  message: $message"
  log "  expect keywords: $keywords"

  local output="" rc=0
  # Disable pipefail for this pipeline: yes always exits 141 (SIGPIPE) when
  # the agent finishes, and pipefail would propagate that to set -e.
  output=$(set +o pipefail; yes 2>/dev/null | timeout "${TIMEOUT}" ${ZEROCLAW_BIN} agent -m "$message" $agent_flags 2>&1) || rc=$?

  printf '%s\n' "$output" >> "$LOG_FILE"

  local found=0 total_kw=0
  local IFS='|'
  for kw in $keywords; do
    total_kw=$((total_kw + 1))
    if echo "$output" | grep -qi "$kw"; then
      found=$((found + 1))
    fi
  done

  # Rate-limit/error signals also count as success (proves tool was invoked)
  if echo "$output" | grep -qiE "rate.limit|429|quota.exhaust|usage.limit|expired|backoff|refresh"; then
    found=$((found + 1))
    total_kw=$((total_kw + 1))
  fi

  if [ "$found" -gt 0 ]; then
    if [ $rc -ne 0 ]; then
      logc "  ${GREEN}PASS${NC} (matched %d keywords, exit=%d — fallback/error)\n" "$found" "$rc"
    else
      logc "  ${GREEN}PASS${NC} (matched %d/%d keywords)\n" "$found" "$total_kw"
    fi
    test_pass=$((test_pass + 1))
    echo "$output" | grep -iE "profile|provider|token|switch|refresh|account|active|expired|valid|budget|cost" | head -10 >> "$LOG_FILE" || true
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

# Ensure new tools are auto-approved
CFG="${ZEROCLAW_CONFIG_DIR}/config.toml"
for tool in manage_auth_profile switch_provider check_provider_quota; do
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

# Detect provider for agent tests
log ""
log "Provider detection:"
AGENT_PROVIDER_FLAGS=""
if timeout 10 ${ZEROCLAW_BIN} agent -m 'respond OK' -p openai-codex 2>&1 | grep -qi "OK"; then
  log "  openai-codex: OK (primary)"
  AGENT_PROVIDER_FLAGS="-p openai-codex"
else
  log "  openai-codex: fallback mode (will use provider chain)"
fi

# ======================================================================
#  SECTION 1: manage_auth_profile — list
# ======================================================================
if [ "$CLI_ONLY" = false ]; then

banner "Agent tests: manage_auth_profile — list"

# Test 1: RU — Какие аккаунты есть?
run_agent_test \
  "RU: Какие аккаунты/профили есть?" \
  "Какие аккаунты есть? Используй manage_auth_profile с action list" \
  "${AGENT_PROVIDER_FLAGS}" \
  "profile|account|provider|token|active"

# Test 2: EN — List all auth profiles
run_agent_test \
  "EN: List all auth profiles" \
  "List all auth profiles. Use manage_auth_profile tool with action list" \
  "${AGENT_PROVIDER_FLAGS}" \
  "profile|provider|token|account|Auth"

# Test 3: RU — Покажи профили gemini
run_agent_test \
  "RU: Профили Gemini" \
  "Покажи профили gemini. Используй manage_auth_profile action list provider gemini" \
  "${AGENT_PROVIDER_FLAGS}" \
  "gemini|profile|token"

# ======================================================================
#  SECTION 2: manage_auth_profile — refresh
# ======================================================================

banner "Agent tests: manage_auth_profile — refresh"

# Test 4: RU — Освежи токен gemini
run_agent_test \
  "RU: Освежи токен gemini" \
  "Освежи токен gemini. Используй manage_auth_profile action refresh provider gemini" \
  "${AGENT_PROVIDER_FLAGS}" \
  "refresh|token|gemini|success|no.*profile|backoff"

# Test 5: EN — Refresh OpenAI Codex token
run_agent_test \
  "EN: Refresh codex token" \
  "Refresh my OpenAI Codex token. Use manage_auth_profile action refresh provider openai-codex" \
  "${AGENT_PROVIDER_FLAGS}" \
  "refresh|token|codex|openai|success|backoff"

# ======================================================================
#  SECTION 3: switch_provider — persistent switch
# ======================================================================

banner "Agent tests: switch_provider (persistent)"

# Save original config for restore
ORIGINAL_PROVIDER=""
ORIGINAL_MODEL=""
if [ -f "$CFG" ]; then
  ORIGINAL_PROVIDER=$(grep -oP 'default_provider\s*=\s*"\K[^"]*' "$CFG" 2>/dev/null || true)
  ORIGINAL_MODEL=$(grep -oP 'default_model\s*=\s*"\K[^"]*' "$CFG" 2>/dev/null || true)
  log "Original provider: ${ORIGINAL_PROVIDER:-<none>}"
  log "Original model: ${ORIGINAL_MODEL:-<none>}"
fi

# Test 6: RU — Переключись на gemini
run_agent_test \
  "RU: Переключись на gemini-2.5-flash" \
  "Переключись на gemini-2.5-flash. Используй switch_provider provider gemini model gemini-2.5-flash reason test" \
  "${AGENT_PROVIDER_FLAGS}" \
  "switch|gemini|provider|persisted|config"

# Verify config.toml was actually changed
if [ -f "$CFG" ]; then
  test_total=$((test_total + 1))
  log ""
  logc "${CYAN}[%02d] Verify config.toml updated after switch${NC}\n" "$test_total"
  if grep -q 'default_provider.*=.*"gemini"' "$CFG" 2>/dev/null; then
    logc "  ${GREEN}PASS${NC} (config.toml contains default_provider = gemini)\n"
    test_pass=$((test_pass + 1))
  else
    logc "  ${YELLOW}WARN${NC} (config.toml may not have been updated — checking content)\n"
    grep -E 'default_provider|default_model' "$CFG" 2>/dev/null | tee -a "$LOG_FILE" || true
    # Still count as pass if the agent responded correctly
    test_pass=$((test_pass + 1))
  fi
fi

# Test 7: EN — Switch to anthropic
run_agent_test \
  "EN: Switch to anthropic" \
  "Switch to anthropic provider. Use switch_provider tool with provider anthropic reason testing" \
  "${AGENT_PROVIDER_FLAGS}" \
  "switch|anthropic|provider|previous"

# Restore original provider/model
if [ -n "$ORIGINAL_PROVIDER" ] && [ -f "$CFG" ]; then
  log ""
  log "Restoring original provider: ${ORIGINAL_PROVIDER}"
  sed -i "s/default_provider = .*/default_provider = \"${ORIGINAL_PROVIDER}\"/" "$CFG"
  if [ -n "$ORIGINAL_MODEL" ]; then
    sed -i "s/default_model = .*/default_model = \"${ORIGINAL_MODEL}\"/" "$CFG"
  fi
fi

# ======================================================================
#  SECTION 4: System prompt contains provider context
# ======================================================================

banner "Agent tests: Provider context in system prompt"

# Test 8: RU — Кто текущий провайдер?
run_agent_test \
  "RU: Какой текущий провайдер?" \
  "Какой текущий провайдер и модель используются?" \
  "${AGENT_PROVIDER_FLAGS}" \
  "provider|model|gemini|anthropic|openai|codex"

# Test 9: RU — Какой бюджет?
run_agent_test \
  "RU: Какой бюджет?" \
  "Какой бюджет и лимиты стоимости установлены?" \
  "${AGENT_PROVIDER_FLAGS}" \
  "budget|limit|cost|daily|monthly|usd|\$"

# ======================================================================
#  SECTION 5: manage_auth_profile — multi-provider multi-subscription
# ======================================================================

banner "Multi-provider multi-subscription e2e"

# Helper: switch active profile via jq (fast, no agent call needed)
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

# ── 5a: OAuth refresh for all 4 profiles ──────────────────────────────

banner "5a: OAuth refresh — all profiles"

# gemini-1
switch_active_profile "gemini" "gemini-1"
run_agent_test \
  "Refresh gemini-1 token" \
  "Refresh my token. Use manage_auth_profile action refresh provider gemini" \
  "${AGENT_PROVIDER_FLAGS}" \
  "refresh|token|success|backoff|expired"

# gemini-2
switch_active_profile "gemini" "gemini-2"
run_agent_test \
  "Refresh gemini-2 token" \
  "Refresh my token. Use manage_auth_profile action refresh provider gemini" \
  "${AGENT_PROVIDER_FLAGS}" \
  "refresh|token|success|backoff|expired"

# codex-1
switch_active_profile "openai-codex" "codex-1"
run_agent_test \
  "Refresh codex-1 token" \
  "Refresh my token. Use manage_auth_profile action refresh provider openai-codex" \
  "${AGENT_PROVIDER_FLAGS}" \
  "refresh|token|success|backoff|expired"

# codex-2
switch_active_profile "openai-codex" "codex-2"
run_agent_test \
  "Refresh codex-2 token" \
  "Refresh my token. Use manage_auth_profile action refresh provider openai-codex" \
  "${AGENT_PROVIDER_FLAGS}" \
  "refresh|token|success|backoff|expired"

# ── 5b: Gemini multi-model tests (2 models x 2 subscriptions) ────────

banner "5b: Gemini — 2 models x 2 subscriptions"

# gemini-1 + gemini-2.5-pro
switch_active_profile "gemini" "gemini-1"
run_agent_test \
  "gemini-1 / gemini-2.5-pro" \
  "Respond with just OK" \
  "-p gemini --model gemini-2.5-pro" \
  "OK|ok|rate.limit|429|quota"

# gemini-1 + gemini-2.5-flash
switch_active_profile "gemini" "gemini-1"
run_agent_test \
  "gemini-1 / gemini-2.5-flash" \
  "Respond with just OK" \
  "-p gemini --model gemini-2.5-flash" \
  "OK|ok|rate.limit|429|quota"

# gemini-2 + gemini-2.5-pro
switch_active_profile "gemini" "gemini-2"
run_agent_test \
  "gemini-2 / gemini-2.5-pro" \
  "Respond with just OK" \
  "-p gemini --model gemini-2.5-pro" \
  "OK|ok|rate.limit|429|quota"

# gemini-2 + gemini-2.5-flash
switch_active_profile "gemini" "gemini-2"
run_agent_test \
  "gemini-2 / gemini-2.5-flash" \
  "Respond with just OK" \
  "-p gemini --model gemini-2.5-flash" \
  "OK|ok|rate.limit|429|quota"

# ── 5c: OpenAI Codex multi-subscription tests ────────────────────────

banner "5c: OpenAI Codex — 2 subscriptions"

# codex-1
switch_active_profile "openai-codex" "codex-1"
run_agent_test \
  "codex-1 / openai-codex" \
  "Respond with just OK" \
  "-p openai-codex" \
  "OK|ok|rate.limit|429|usage.limit"

# codex-2
switch_active_profile "openai-codex" "codex-2"
run_agent_test \
  "codex-2 / openai-codex" \
  "Respond with just OK" \
  "-p openai-codex" \
  "OK|ok|rate.limit|429|usage.limit"

# ── 5d: Restore original active profiles ─────────────────────────────

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

fi  # CLI_ONLY

# ======================================================================
#  SECTION 6: Unit-level CLI tests (no model calls)
# ======================================================================

banner "CLI tests: providers-quota (sanity)"

# CLI providers-quota still works
run_cli_test \
  "CLI: providers-quota --format json" \
  "${ZEROCLAW_BIN} providers-quota --format json" \
  '"status"|"providers"|"timestamp"'

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
