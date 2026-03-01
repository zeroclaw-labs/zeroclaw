#!/usr/bin/env bash
set -euo pipefail

print_cc_info() {
    echo "C compiler available: $(command -v cc)"
    cc --version | head -n1 || true
}

prepend_path() {
    local dir="$1"
    export PATH="${dir}:${PATH}"
    if [ -n "${GITHUB_PATH:-}" ]; then
        echo "${dir}" >> "${GITHUB_PATH}"
    fi
}

shim_cc_to_compiler() {
    local compiler="$1"
    local compiler_path
    local shim_dir
    if ! command -v "${compiler}" >/dev/null 2>&1; then
        return 1
    fi
    compiler_path="$(command -v "${compiler}")"
    shim_dir="${RUNNER_TEMP:-/tmp}/cc-shim"
    mkdir -p "${shim_dir}"
    ln -sf "${compiler_path}" "${shim_dir}/cc"
    prepend_path "${shim_dir}"
    echo "::notice::Created 'cc' shim from ${compiler_path}."
}

run_as_privileged() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
        return $?
    fi
    if command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
        sudo -n "$@"
        return $?
    fi
    return 1
}

install_cc_toolchain() {
    if command -v apt-get >/dev/null 2>&1; then
        run_as_privileged apt-get update
        run_as_privileged env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential pkg-config
    elif command -v yum >/dev/null 2>&1; then
        run_as_privileged yum install -y gcc gcc-c++ make pkgconfig
    elif command -v dnf >/dev/null 2>&1; then
        run_as_privileged dnf install -y gcc gcc-c++ make pkgconf-pkg-config
    elif command -v apk >/dev/null 2>&1; then
        run_as_privileged apk add --no-cache build-base pkgconf
    else
        return 1
    fi
}

if command -v cc >/dev/null 2>&1; then
    print_cc_info
    exit 0
fi

if shim_cc_to_compiler clang && command -v cc >/dev/null 2>&1; then
    print_cc_info
    exit 0
fi

if shim_cc_to_compiler gcc && command -v cc >/dev/null 2>&1; then
    print_cc_info
    exit 0
fi

echo "::warning::Missing 'cc' on runner. Attempting package-manager install."
if ! install_cc_toolchain; then
    echo "::warning::Unable to install compiler via package manager (missing privilege or unsupported manager)."
fi

if command -v cc >/dev/null 2>&1; then
    print_cc_info
    exit 0
fi

if shim_cc_to_compiler clang && command -v cc >/dev/null 2>&1; then
    print_cc_info
    exit 0
fi

if shim_cc_to_compiler gcc && command -v cc >/dev/null 2>&1; then
    print_cc_info
    exit 0
fi

echo "::error::Failed to provision 'cc'. Install a compiler toolchain or configure passwordless sudo on the runner."
exit 1
