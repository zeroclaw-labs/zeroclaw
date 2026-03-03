#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Validate secure production baseline for ZeroClaw.

Usage:
  ./scripts/secure-validate.sh [options]

Options:
  --config-path <path>        Config path (default: ~/.zeroclaw/config.toml)
  --trusted-skill-root <path> Expected trusted skills root
                              (default: /opt/zeroclaw/skills-trusted)
  --skip-runtime-checks       Skip zeroclaw status/doctor checks
  --skip-channel-check        Skip zeroclaw channel doctor
  --skip-skill-audit          Skip zeroclaw skills audit
  -h, --help                  Show help
USAGE
}

fail() {
  echo "error: $*" >&2
  exit 1
}

info() {
  echo "==> $*"
}

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

trim() {
  local value="$1"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  value="${value%\"}"
  value="${value#\"}"
  printf '%s' "$value"
}

CONFIG_PATH="${HOME}/.zeroclaw/config.toml"
TRUSTED_SKILL_ROOT="/opt/zeroclaw/skills-trusted"
RUN_RUNTIME_CHECKS=true
RUN_CHANNEL_CHECK=true
RUN_SKILL_AUDIT=true
FAILURES=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --config-path)
      CONFIG_PATH="${2:-}"
      [[ -n "$CONFIG_PATH" ]] || fail "--config-path requires a value"
      shift 2
      ;;
    --trusted-skill-root)
      TRUSTED_SKILL_ROOT="${2:-}"
      [[ -n "$TRUSTED_SKILL_ROOT" ]] || fail "--trusted-skill-root requires a value"
      shift 2
      ;;
    --skip-runtime-checks)
      RUN_RUNTIME_CHECKS=false
      shift
      ;;
    --skip-channel-check)
      RUN_CHANNEL_CHECK=false
      shift
      ;;
    --skip-skill-audit)
      RUN_SKILL_AUDIT=false
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown option: $1"
      ;;
  esac
done

have_cmd zeroclaw || fail "zeroclaw is not installed or not in PATH"
[[ -f "$CONFIG_PATH" ]] || fail "config file not found: $CONFIG_PATH"

check_config_equals() {
  local key="$1"
  local expected="$2"
  local actual_raw actual

  if ! actual_raw="$(zeroclaw config get "$key" 2>/dev/null)"; then
    echo "FAIL: config key '$key' could not be read"
    FAILURES=$((FAILURES + 1))
    return
  fi

  actual="$(trim "$actual_raw")"
  if [[ "$actual" == "$expected" ]]; then
    echo "PASS: $key = $expected"
  else
    echo "FAIL: $key expected '$expected' but found '$actual'"
    FAILURES=$((FAILURES + 1))
  fi
}

check_config_contains() {
  local key="$1"
  local needle="$2"
  local actual_raw

  if ! actual_raw="$(zeroclaw config get "$key" 2>/dev/null)"; then
    echo "FAIL: config key '$key' could not be read"
    FAILURES=$((FAILURES + 1))
    return
  fi

  if printf '%s' "$actual_raw" | grep -Fq "$needle"; then
    echo "PASS: $key contains '$needle'"
  else
    echo "FAIL: $key does not contain '$needle'"
    FAILURES=$((FAILURES + 1))
  fi
}

info "Checking hardening config values"
check_config_equals "secrets.encrypt" "true"
check_config_equals "gateway.host" "127.0.0.1"
check_config_equals "gateway.port" "42617"
check_config_equals "gateway.require_pairing" "true"
check_config_equals "gateway.allow_public_bind" "false"
check_config_equals "autonomy.level" "supervised"
check_config_equals "autonomy.workspace_only" "true"
check_config_equals "autonomy.block_high_risk_commands" "true"
check_config_equals "autonomy.require_approval_for_medium_risk" "true"
check_config_equals "autonomy.allow_sensitive_file_reads" "false"
check_config_equals "autonomy.allow_sensitive_file_writes" "false"
check_config_equals "skills.open_skills_enabled" "false"
check_config_equals "skills.allow_scripts" "false"
check_config_equals "skills.prompt_injection_mode" "compact"
check_config_equals "security.url_access.block_private_ip" "true"
check_config_equals "security.url_access.allow_loopback" "false"
check_config_equals "security.url_access.enforce_domain_allowlist" "true"
check_config_contains "skills.trusted_skill_roots" "$TRUSTED_SKILL_ROOT"
check_config_contains "security.url_access.domain_allowlist" "api.openai.com"
check_config_contains "security.url_access.domain_allowlist" "openrouter.ai"

if [[ -f "${HOME}/.zeroclaw/.secret_key" ]]; then
  info "Checking secret key permissions"
  if have_cmd stat; then
    secret_mode="$(stat -c '%a' "${HOME}/.zeroclaw/.secret_key" 2>/dev/null || true)"
    if [[ -z "$secret_mode" ]]; then
      secret_mode="$(stat -f '%Lp' "${HOME}/.zeroclaw/.secret_key" 2>/dev/null || true)"
    fi
    if [[ "$secret_mode" == "600" ]]; then
      echo "PASS: ~/.zeroclaw/.secret_key permissions are 600"
    else
      echo "FAIL: ~/.zeroclaw/.secret_key permissions are '$secret_mode' (expected 600)"
      FAILURES=$((FAILURES + 1))
    fi
  fi
else
  echo "WARN: ~/.zeroclaw/.secret_key not found yet (it is created when encrypted secrets are written)"
fi

if [[ "$RUN_RUNTIME_CHECKS" == true ]]; then
  info "Running runtime checks"
  if zeroclaw status; then
    echo "PASS: zeroclaw status"
  else
    echo "FAIL: zeroclaw status"
    FAILURES=$((FAILURES + 1))
  fi
  if zeroclaw doctor; then
    echo "PASS: zeroclaw doctor"
  else
    echo "FAIL: zeroclaw doctor"
    FAILURES=$((FAILURES + 1))
  fi
fi

if [[ "$RUN_CHANNEL_CHECK" == true ]]; then
  info "Running channel checks"
  if zeroclaw channel doctor; then
    echo "PASS: zeroclaw channel doctor"
  else
    echo "FAIL: zeroclaw channel doctor"
    FAILURES=$((FAILURES + 1))
  fi
fi

if [[ "$RUN_SKILL_AUDIT" == true ]]; then
  info "Running skills audit"
  if zeroclaw skills audit; then
    echo "PASS: zeroclaw skills audit"
  else
    echo "FAIL: zeroclaw skills audit"
    FAILURES=$((FAILURES + 1))
  fi
fi

if [[ "$FAILURES" -gt 0 ]]; then
  echo
  fail "secure baseline validation failed ($FAILURES issue(s))"
fi

echo
echo "Secure baseline validation passed."
