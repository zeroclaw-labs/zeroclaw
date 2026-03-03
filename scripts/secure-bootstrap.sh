#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Secure ZeroClaw bootstrap (production baseline)

Usage:
  ./scripts/secure-bootstrap.sh --ref <tag-or-commit> [options]

Required:
  --ref <tag-or-commit>    Pinned git tag/commit that must match current HEAD

Options:
  --skip-system-deps       Do not pass --install-system-deps
  --skip-rust              Do not pass --install-rust
  --interactive-onboard    Run interactive onboarding after install
  --onboard-env            Run non-interactive onboarding using env only:
                           ZEROCLAW_API_KEY + ZEROCLAW_PROVIDER (+ ZEROCLAW_MODEL optional)
  --config-path <path>     Secure config destination if no config exists
                           (default: ~/.zeroclaw/config.toml)
  --force-config-template  Overwrite config-path with secure template
  -h, --help               Show this help

Notes:
  - This script enforces source install via ./zeroclaw_install.sh --force-source-build.
  - It never enables prebuilt install flow.
  - For automated onboarding, pass secrets via environment variables, not CLI args.
USAGE
}

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

fail() {
  echo "error: $*" >&2
  exit 1
}

info() {
  echo "==> $*"
}

trim() {
  local value="$1"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  printf '%s' "$value"
}

SCRIPT_PATH="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" >/dev/null 2>&1 && pwd || pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd || pwd)"
INSTALLER="$REPO_ROOT/zeroclaw_install.sh"
SECURE_TEMPLATE="$REPO_ROOT/dev/config.secure.prod.toml"

PINNED_REF=""
INSTALL_SYSTEM_DEPS=true
INSTALL_RUST=true
RUN_INTERACTIVE_ONBOARD=false
RUN_ENV_ONBOARD=false
CONFIG_PATH="${HOME}/.zeroclaw/config.toml"
FORCE_CONFIG_TEMPLATE=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --ref)
      PINNED_REF="${2:-}"
      [[ -n "$PINNED_REF" ]] || fail "--ref requires a value"
      shift 2
      ;;
    --skip-system-deps)
      INSTALL_SYSTEM_DEPS=false
      shift
      ;;
    --skip-rust)
      INSTALL_RUST=false
      shift
      ;;
    --interactive-onboard)
      RUN_INTERACTIVE_ONBOARD=true
      shift
      ;;
    --onboard-env)
      RUN_ENV_ONBOARD=true
      shift
      ;;
    --config-path)
      CONFIG_PATH="${2:-}"
      [[ -n "$CONFIG_PATH" ]] || fail "--config-path requires a value"
      shift 2
      ;;
    --force-config-template)
      FORCE_CONFIG_TEMPLATE=true
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

[[ -n "$PINNED_REF" ]] || fail "--ref is required"

have_cmd git || fail "git is required"
[[ -x "$INSTALLER" ]] || fail "installer not found: $INSTALLER"
[[ -f "$SECURE_TEMPLATE" ]] || fail "secure template not found: $SECURE_TEMPLATE"

if [[ ! -d "$REPO_ROOT/.git" ]]; then
  fail "repository root does not contain .git: $REPO_ROOT"
fi

CURRENT_HEAD="$(git -C "$REPO_ROOT" rev-parse --verify HEAD)"
EXPECTED_HEAD="$(git -C "$REPO_ROOT" rev-parse --verify "${PINNED_REF}^{commit}" 2>/dev/null || true)"
[[ -n "$EXPECTED_HEAD" ]] || fail "could not resolve pinned ref: $PINNED_REF"

if [[ "$CURRENT_HEAD" != "$EXPECTED_HEAD" ]]; then
  cat >&2 <<EOF
error: pinned ref mismatch
  requested: $PINNED_REF
  expected:  $EXPECTED_HEAD
  current:   $CURRENT_HEAD

Checkout the pinned commit first, then rerun this script.
EOF
  exit 1
fi

info "Pinned revision check passed ($EXPECTED_HEAD)"
info "Running source-only installer"

INSTALL_CMD=("$INSTALLER" --force-source-build --no-guided)
if [[ "$INSTALL_SYSTEM_DEPS" == true ]]; then
  INSTALL_CMD+=(--install-system-deps)
fi
if [[ "$INSTALL_RUST" == true ]]; then
  INSTALL_CMD+=(--install-rust)
fi

"${INSTALL_CMD[@]}"

info "Source installation complete"

CONFIG_DIR="$(dirname "$CONFIG_PATH")"
mkdir -p "$CONFIG_DIR"

if [[ "$FORCE_CONFIG_TEMPLATE" == true ]]; then
  info "Writing secure config template to $CONFIG_PATH (forced)"
  install -m 600 "$SECURE_TEMPLATE" "$CONFIG_PATH"
elif [[ ! -f "$CONFIG_PATH" ]]; then
  info "No config found; seeding secure config template at $CONFIG_PATH"
  install -m 600 "$SECURE_TEMPLATE" "$CONFIG_PATH"
else
  info "Existing config detected at $CONFIG_PATH (not overwritten)"
fi

if [[ "$RUN_INTERACTIVE_ONBOARD" == true && "$RUN_ENV_ONBOARD" == true ]]; then
  fail "--interactive-onboard and --onboard-env are mutually exclusive"
fi

if [[ "$RUN_INTERACTIVE_ONBOARD" == true ]]; then
  info "Starting interactive onboarding"
  zeroclaw onboard --interactive
elif [[ "$RUN_ENV_ONBOARD" == true ]]; then
  API_KEY_VALUE="$(trim "${ZEROCLAW_API_KEY:-}")"
  PROVIDER_VALUE="$(trim "${ZEROCLAW_PROVIDER:-}")"
  MODEL_VALUE="$(trim "${ZEROCLAW_MODEL:-}")"

  [[ -n "$API_KEY_VALUE" ]] || fail "ZEROCLAW_API_KEY is required for --onboard-env"
  [[ -n "$PROVIDER_VALUE" ]] || fail "ZEROCLAW_PROVIDER is required for --onboard-env"

  if [[ -n "$MODEL_VALUE" ]]; then
    info "Running env-based onboarding (provider=$PROVIDER_VALUE, model=$MODEL_VALUE)"
    zeroclaw onboard --api-key "$API_KEY_VALUE" --provider "$PROVIDER_VALUE" --model "$MODEL_VALUE"
  else
    info "Running env-based onboarding (provider=$PROVIDER_VALUE)"
    zeroclaw onboard --api-key "$API_KEY_VALUE" --provider "$PROVIDER_VALUE"
  fi
else
  info "Onboarding skipped by default. Run: zeroclaw onboard --interactive"
fi

if [[ -x "${HOME}/.zeroclaw/.secret_key" ]]; then
  true
fi

if [[ -f "${HOME}/.zeroclaw/.secret_key" ]]; then
  chmod 600 "${HOME}/.zeroclaw/.secret_key" 2>/dev/null || true
fi

if have_cmd sha256sum; then
  info "Binary fingerprint"
  sha256sum "$(command -v zeroclaw)"
elif have_cmd shasum; then
  info "Binary fingerprint"
  shasum -a 256 "$(command -v zeroclaw)"
else
  info "No sha256 utility found (sha256sum/shasum unavailable)"
fi

cat <<'DONE'

Secure bootstrap finished.

Recommended next commands:
  zeroclaw config show
  scripts/secure-validate.sh
DONE
