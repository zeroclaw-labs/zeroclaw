#!/usr/bin/env bash
set -euo pipefail

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

if command -v sudo >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
    echo "C compiler not found. Installing build-essential via apt..."
    sudo apt-get update
    sudo apt-get install -y build-essential
    configure_linker "$(command -v cc)"
    exit 0
fi

echo "No usable C compiler found (cc/gcc/clang)." >&2
exit 1
