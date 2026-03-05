#!/usr/bin/env bash
set -euo pipefail

# Restricted-profile CI lane:
# - isolates HOME/XDG paths into a throwaway directory
# - forces workspace/config roots away from developer machine defaults
# - runs capability-aware tests that should not require external network access

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${REPO_ROOT}"

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/zeroclaw-restricted-profile.XXXXXX")"
cleanup() {
    rm -rf "${TMP_ROOT}"
}
trap cleanup EXIT

RESTRICTED_HOME="${TMP_ROOT}/home"
RESTRICTED_WORKSPACE="${TMP_ROOT}/workspace-root"
mkdir -p "${RESTRICTED_HOME}" "${RESTRICTED_WORKSPACE}"
chmod 700 "${RESTRICTED_HOME}" "${RESTRICTED_WORKSPACE}"

ORIGINAL_HOME="${HOME:-}"
if [ -z "${RUSTUP_HOME:-}" ] && [ -n "${ORIGINAL_HOME}" ]; then
    export RUSTUP_HOME="${ORIGINAL_HOME}/.rustup"
fi
if [ -z "${CARGO_HOME:-}" ] && [ -n "${ORIGINAL_HOME}" ]; then
    export CARGO_HOME="${ORIGINAL_HOME}/.cargo"
fi
if [ -n "${CARGO_HOME:-}" ] && [ -d "${CARGO_HOME}/bin" ]; then
    case ":${PATH}:" in
    *":${CARGO_HOME}/bin:"*) ;;
    *) export PATH="${CARGO_HOME}/bin:${PATH}" ;;
    esac
fi

export HOME="${RESTRICTED_HOME}"
export USERPROFILE="${RESTRICTED_HOME}"
export XDG_CONFIG_HOME="${RESTRICTED_HOME}/.config"
export XDG_CACHE_HOME="${RESTRICTED_HOME}/.cache"
export XDG_DATA_HOME="${RESTRICTED_HOME}/.local/share"
export ZEROCLAW_WORKSPACE="${RESTRICTED_WORKSPACE}"
mkdir -p "${XDG_CONFIG_HOME}" "${XDG_CACHE_HOME}" "${XDG_DATA_HOME}"

# Keep credential/network assumptions explicit for this lane.
unset GEMINI_OAUTH_CLIENT_ID GEMINI_OAUTH_CLIENT_SECRET OPENAI_API_KEY ANTHROPIC_API_KEY
unset HTTP_PROXY HTTPS_PROXY ALL_PROXY
export NO_PROXY="127.0.0.1,localhost"

tests=(
    "skills::tests::load_skills_with_config_reads_open_skills_dir_without_network"
    "onboard::wizard::tests::run_models_refresh_uses_fresh_cache_without_network"
    "onboard::wizard::tests::quick_setup_respects_zero_claw_workspace_env_layout"
    "config::schema::tests::load_or_init_workspace_override_uses_workspace_root_for_config"
)

echo "Running restricted-profile hermetic subset (${#tests[@]} tests)"
for test_name in "${tests[@]}"; do
    echo "==> cargo test --locked --lib ${test_name}"
    cargo test --locked --lib "${test_name}"
done

echo "Restricted-profile hermetic subset completed successfully."
