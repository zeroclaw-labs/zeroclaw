#!/usr/bin/env bash
set -euo pipefail

requested_toolchain="${1:-1.92.0}"
fallback_toolchain="${2:-stable}"
strict_mode_raw="${3:-${ENSURE_CARGO_COMPONENT_STRICT:-false}}"
strict_mode="$(printf '%s' "${strict_mode_raw}" | tr '[:upper:]' '[:lower:]')"

is_truthy() {
    local value="${1:-}"
    case "${value}" in
    1 | true | yes | on) return 0 ;;
    *) return 1 ;;
    esac
}

probe_cargo() {
    local toolchain="$1"
    rustup run "${toolchain}" cargo --version >/dev/null 2>&1
}

probe_rustc() {
    local toolchain="$1"
    rustup run "${toolchain}" rustc --version >/dev/null 2>&1
}

export_toolchain_for_next_steps() {
    local toolchain="$1"
    if [ -z "${GITHUB_ENV:-}" ]; then
        return 0
    fi

    {
        echo "RUSTUP_TOOLCHAIN=${toolchain}"
        cargo_path="$(rustup which --toolchain "${toolchain}" cargo 2>/dev/null || true)"
        rustc_path="$(rustup which --toolchain "${toolchain}" rustc 2>/dev/null || true)"
        if [ -n "${cargo_path}" ]; then
            echo "CARGO=${cargo_path}"
        fi
        if [ -n "${rustc_path}" ]; then
            echo "RUSTC=${rustc_path}"
        fi
    } >>"${GITHUB_ENV}"
}

assert_rustc_version_matches() {
    local toolchain="$1"
    local expected_version="$2"
    local actual_version

    if [[ ! "${expected_version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        return 0
    fi

    actual_version="$(rustup run "${toolchain}" rustc --version | awk '{print $2}')"
    if [ "${actual_version}" != "${expected_version}" ]; then
        echo "rustc version mismatch for ${toolchain}: expected ${expected_version}, got ${actual_version}" >&2
        exit 1
    fi
}

selected_toolchain="${requested_toolchain}"

echo "Ensuring cargo component is available for toolchain: ${requested_toolchain}"

if ! probe_rustc "${requested_toolchain}"; then
    echo "Requested toolchain ${requested_toolchain} is not installed; installing..."
    rustup toolchain install "${requested_toolchain}" --profile default
fi

if ! probe_cargo "${requested_toolchain}"; then
    echo "cargo is unavailable for ${requested_toolchain}; reinstalling toolchain profile..."
    rustup toolchain install "${requested_toolchain}" --profile default
    rustup component add cargo --toolchain "${requested_toolchain}" || true
fi

if ! probe_cargo "${requested_toolchain}"; then
    if is_truthy "${strict_mode}"; then
        echo "::error::Strict mode enabled; cargo is unavailable for requested toolchain ${requested_toolchain}." >&2
        rustup toolchain list || true
        exit 1
    fi
    echo "::warning::Falling back to ${fallback_toolchain} because ${requested_toolchain} cargo remains unavailable."
    rustup toolchain install "${fallback_toolchain}" --profile default
    rustup component add cargo --toolchain "${fallback_toolchain}" || true
    if ! probe_cargo "${fallback_toolchain}"; then
        echo "No usable cargo found for ${requested_toolchain} or ${fallback_toolchain}" >&2
        rustup toolchain list || true
        exit 1
    fi
    selected_toolchain="${fallback_toolchain}"
fi

if is_truthy "${strict_mode}" && [ "${selected_toolchain}" != "${requested_toolchain}" ]; then
    echo "::error::Strict mode enabled; refusing fallback toolchain ${selected_toolchain} (requested ${requested_toolchain})." >&2
    exit 1
fi

if is_truthy "${strict_mode}"; then
    assert_rustc_version_matches "${selected_toolchain}" "${requested_toolchain}"
fi

export_toolchain_for_next_steps "${selected_toolchain}"

echo "Using Rust toolchain: ${selected_toolchain}"
rustup run "${selected_toolchain}" rustc --version
rustup run "${selected_toolchain}" cargo --version
