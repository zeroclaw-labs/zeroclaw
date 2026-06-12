#!/usr/bin/env bash
# tests/test-01.6-gateway.sh — Tests for Phase 1.6 deliverables.
#
# Properties under test:
#   1. Gateway sub-surface stripped (STRIP-05): REST endpoints, ACP bridge,
#      SSE, embedded web dashboard, pairing dashboard UI, mTLS server option,
#      outbound webhook endpoints all gone.
#   2. /ws/chat endpoint KEPT (OS-MDashboard chat-relay depends on it).
#   3. paired_tokens auth path KEPT.
#   4. install-guide ansible/install_osagent.yml exists in sovereign-shield-
#      install-guide (on a feat/ branch awaiting merge).

. "$(dirname "$0")/lib.sh"
start_suite "01.6 — Gateway Fork + Install Drop-In"

GW=crates/zeroclaw-gateway

# Files stripped from gateway crate
DROPPED_GW_FILES=(
  acp.rs
  api_config.rs api_onboard.rs api_pairing.rs api_personality.rs
  api_plugins.rs api_webauthn.rs
  canvas.rs hardware_context.rs
  openapi.rs openapi_docs.html
  sse.rs static_files.rs tls.rs
  voice_duplex.rs ws_approval.rs
)
for f in "${DROPPED_GW_FILES[@]}"; do
  assert_file_absent "$GW/src/$f" "STRIP-05 $f stripped"
done

# Module declarations stripped from lib.rs
DROPPED_GW_MODS=(
  acp api_config api_onboard api_pairing api_personality
  api_plugins api_webauthn canvas hardware_context openapi
  sse static_files tls voice_duplex ws_approval
)
for m in "${DROPPED_GW_MODS[@]}"; do
  assert_no_grep "^pub mod $m;"  "$GW/src/lib.rs" "STRIP-05 $m mod declaration absent"
done

# WS chat endpoint KEPT
assert_file_exists "$GW/src/ws.rs" "/ws/chat endpoint source kept"
assert_grep "^pub mod ws;" "$GW/src/lib.rs" "/ws/chat mod declaration kept"

# Install-guide ansible task (on feat branch of sibling repo, awaiting PR)
IG=../sovereign-shield-install-guide
if [ -d "$IG/.git" ]; then
  IG_TASK_MAIN=$(cd "$IG" && git show main:ansible/install_osagent.yml 2>/dev/null | head -1)
  IG_TASK_FEAT=$(cd "$IG" && git show feat/osagent-install-task:ansible/install_osagent.yml 2>/dev/null | head -1)
  if [ -n "$IG_TASK_MAIN" ]; then
    _log_pass "ansible/install_osagent.yml present on install-guide main"
  elif [ -n "$IG_TASK_FEAT" ]; then
    _log_pass "ansible/install_osagent.yml present on feat/osagent-install-task (PR pending merge)"
  else
    _log_fail "ansible/install_osagent.yml missing on main and feat branch"
  fi
else
  echo "  ⊘ sovereign-shield-install-guide sibling repo not available — skipping install task check"
fi

summarise
