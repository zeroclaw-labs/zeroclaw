#!/usr/bin/env bash

set -euo pipefail

MODE="correctness"
if [ "${1:-}" = "--strict" ]; then
    MODE="strict"
fi

ensure_rust_component() {
    local component="$1"
    if ! command -v rustup >/dev/null 2>&1; then
        return 0
    fi

    local toolchain_args=()
    if [ -n "${RUSTUP_TOOLCHAIN:-}" ]; then
        toolchain_args=(--toolchain "${RUSTUP_TOOLCHAIN}")
    fi

    if rustup component list "${toolchain_args[@]}" | grep -Eq "^${component}(-[[:alnum:]_\\-]+)? \\(installed\\)$"; then
        return 0
    fi

    echo "==> rust quality: installing missing component '${component}'"
    rustup component add "${component}" "${toolchain_args[@]}"
}

ensure_rust_component rustfmt
ensure_rust_component clippy

echo "==> rust quality: cargo fmt --all -- --check"
cargo fmt --all -- --check

if [ "$MODE" = "strict" ]; then
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D warnings"
    cargo clippy --locked --all-targets -- -D warnings
else
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D clippy::correctness"
    cargo clippy --locked --all-targets -- -D clippy::correctness
fi
