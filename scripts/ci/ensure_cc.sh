#!/usr/bin/env bash
set -euo pipefail

print_cc_info() {
    echo "C compiler available: $(command -v cc)"
    cc --version | head -n1 || true
}

print_ar_info() {
    echo "Archiver available: $(command -v ar)"
    ar --version 2>/dev/null | head -n1 || true
}

toolchain_ready() {
    command -v cc >/dev/null 2>&1 && command -v ar >/dev/null 2>&1
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

shim_ar_to_tool() {
    local tool="$1"
    local tool_path
    local shim_dir
    if ! command -v "${tool}" >/dev/null 2>&1; then
        return 1
    fi
    tool_path="$(command -v "${tool}")"
    shim_dir="${RUNNER_TEMP:-/tmp}/cc-shim"
    mkdir -p "${shim_dir}"
    ln -sf "${tool_path}" "${shim_dir}/ar"
    prepend_path "${shim_dir}"
    echo "::notice::Created 'ar' shim from ${tool_path}."
}

ensure_archiver() {
    if command -v ar >/dev/null 2>&1; then
        return 0
    fi
    shim_ar_to_tool llvm-ar && return 0
    shim_ar_to_tool gcc-ar && return 0
    return 1
}

finish_if_ready() {
    ensure_archiver || true
    if toolchain_ready; then
        print_cc_info
        print_ar_info
        exit 0
    fi
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
        run_as_privileged env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential binutils pkg-config
    elif command -v yum >/dev/null 2>&1; then
        run_as_privileged yum install -y gcc gcc-c++ binutils make pkgconfig
    elif command -v dnf >/dev/null 2>&1; then
        run_as_privileged dnf install -y gcc gcc-c++ binutils make pkgconf-pkg-config
    elif command -v apk >/dev/null 2>&1; then
        run_as_privileged apk add --no-cache build-base pkgconf
    else
        return 1
    fi
}

install_zig_cc_shim() {
    local zig_version="0.14.0"
    local platform
    local archive_name
    local base_dir
    local extract_dir
    local archive_path
    local download_url
    local shim_dir
    local zig_bin

    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64) platform="linux-x86_64" ;;
        Linux/aarch64 | Linux/arm64) platform="linux-aarch64" ;;
        Darwin/x86_64) platform="macos-x86_64" ;;
        Darwin/arm64) platform="macos-aarch64" ;;
        *)
            return 1
            ;;
    esac

    archive_name="zig-${platform}-${zig_version}"
    base_dir="${RUNNER_TEMP:-/tmp}/zig"
    extract_dir="${base_dir}/${archive_name}"
    archive_path="${base_dir}/${archive_name}.tar.xz"
    download_url="https://ziglang.org/download/${zig_version}/${archive_name}.tar.xz"
    zig_bin="${extract_dir}/zig"

    mkdir -p "${base_dir}"

    if [ ! -x "${zig_bin}" ]; then
        if command -v curl >/dev/null 2>&1; then
            curl -fsSL "${download_url}" -o "${archive_path}"
        elif command -v wget >/dev/null 2>&1; then
            wget -qO "${archive_path}" "${download_url}"
        else
            return 1
        fi
        tar -xJf "${archive_path}" -C "${base_dir}"
    fi

    if [ ! -x "${zig_bin}" ]; then
        return 1
    fi

    shim_dir="${RUNNER_TEMP:-/tmp}/cc-shim"
    mkdir -p "${shim_dir}"
    cat > "${shim_dir}/cc" <<EOF
#!/usr/bin/env bash
set -euo pipefail
args=()
for arg in "\$@"; do
    if [[ "\$arg" == --target=* ]]; then
        target="\${arg#--target=}"
        target="\${target//-unknown-/-}"
        target="\${target//-pc-/-}"
        args+=("--target=\${target}")
    else
        args+=("\$arg")
    fi
done
"${zig_bin}" cc "\${args[@]}"
EOF
    chmod +x "${shim_dir}/cc"
    cat > "${shim_dir}/ar" <<EOF
#!/usr/bin/env bash
set -euo pipefail
"${zig_bin}" ar "\$@"
EOF
    chmod +x "${shim_dir}/ar"
    prepend_path "${shim_dir}"
    echo "::notice::Provisioned 'cc' and 'ar' via Zig wrappers (${zig_version})."
}

if command -v cc >/dev/null 2>&1; then
    finish_if_ready
fi

if shim_cc_to_compiler clang; then
    finish_if_ready
fi

if shim_cc_to_compiler gcc; then
    finish_if_ready
fi

echo "::warning::Missing 'cc' on runner. Attempting package-manager install."
if ! install_cc_toolchain; then
    echo "::warning::Unable to install compiler via package manager (missing privilege or unsupported manager)."
fi

if command -v cc >/dev/null 2>&1; then
    finish_if_ready
fi

if install_zig_cc_shim; then
    finish_if_ready
fi

if shim_cc_to_compiler clang; then
    finish_if_ready
fi

if shim_cc_to_compiler gcc; then
    finish_if_ready
fi

echo "::error::Failed to provision 'cc' and 'ar'. Install a compiler/binutils toolchain or configure passwordless sudo on the runner."
exit 1
