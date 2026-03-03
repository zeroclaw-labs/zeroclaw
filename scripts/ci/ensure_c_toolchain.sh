#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

set_env_var() {
    local key="$1"
    local value="$2"
    if [ -n "${GITHUB_ENV:-}" ]; then
        echo "${key}=${value}" >>"${GITHUB_ENV}"
    fi
}

configure_linker() {
    local linker="$1"
    if [ ! -x "${linker}" ]; then
        return 1
    fi

    set_env_var "CC" "${linker}"
    set_env_var "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER" "${linker}"

    if command -v g++ >/dev/null 2>&1; then
        set_env_var "CXX" "$(command -v g++)"
    elif command -v clang++ >/dev/null 2>&1; then
        set_env_var "CXX" "$(command -v clang++)"
    fi

    echo "Using C linker: ${linker}"
    "${linker}" --version | head -n 1 || true
    return 0
}

echo "Ensuring C toolchain is available for Rust native dependencies"

if command -v cc >/dev/null 2>&1; then
    configure_linker "$(command -v cc)"
    exit 0
fi

if command -v gcc >/dev/null 2>&1; then
    configure_linker "$(command -v gcc)"
    exit 0
fi

if command -v clang >/dev/null 2>&1; then
    configure_linker "$(command -v clang)"
    exit 0
fi

resolve_cc_after_bootstrap() {
    if command -v cc >/dev/null 2>&1; then
        command -v cc
        return 0
    fi

    local shim_dir="${RUNNER_TEMP:-/tmp}/cc-shim"
    local shim_cc="${shim_dir}/cc"
    if [ -x "${shim_cc}" ]; then
        export PATH="${shim_dir}:${PATH}"
        command -v cc
        return 0
    fi

    return 1
}

# Prefer the resilient provisioning path (package manager + Zig fallback) used by CI Rust jobs.
if [ -x "${script_dir}/ensure_cc.sh" ]; then
    if bash "${script_dir}/ensure_cc.sh"; then
        if cc_path="$(resolve_cc_after_bootstrap)"; then
            configure_linker "${cc_path}"
            exit 0
        fi
        echo "::warning::C toolchain bootstrap reported success but 'cc' is still unavailable in current step."
    fi
fi

if [ "${ALLOW_MISSING_C_TOOLCHAIN:-}" = "1" ] || [ "${ALLOW_MISSING_C_TOOLCHAIN:-}" = "true" ]; then
    echo "::warning::No usable C compiler found; continuing because ALLOW_MISSING_C_TOOLCHAIN is enabled."
    exit 0
fi

echo "No usable C compiler found (cc/gcc/clang)." >&2
exit 1
