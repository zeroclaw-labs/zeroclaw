#!/usr/bin/env bash

set -euo pipefail

MODE="correctness"
if [ "${1:-}" = "--strict" ]; then
    MODE="strict"
fi

ensure_rust_component() {
    local component="$1"
    # Some self-hosted runners start from partial toolchain images.
    # `rustup component add` is idempotent and guarantees host components are present.
    if ! command -v rustup >/dev/null 2>&1; then
        return 0
    fi

    local toolchain_args=()
    if [ -n "${RUSTUP_TOOLCHAIN:-}" ]; then
        toolchain_args=(--toolchain "${RUSTUP_TOOLCHAIN}")
    fi

    echo "==> rust quality: ensuring component '${component}'"
    rustup component add "${component}" "${toolchain_args[@]}"
}

ensure_rust_component rustfmt
ensure_rust_component clippy

run_cargo() {
    local cargo_bin=""

    if [ -n "${CARGO:-}" ] && [ -x "${CARGO}" ]; then
        cargo_bin="${CARGO}"
    elif command -v cargo >/dev/null 2>&1; then
        cargo_bin="$(command -v cargo)"
    fi

    if [ -z "${cargo_bin}" ]; then
        echo "error: cargo executable not found (CARGO='${CARGO:-}')" >&2
        return 127
    fi

    if command -v rustup >/dev/null 2>&1 && [ -n "${RUSTUP_TOOLCHAIN:-}" ]; then
        rustup run "${RUSTUP_TOOLCHAIN}" "${cargo_bin}" "$@"
    else
        "${cargo_bin}" "$@"
    fi
}

echo "==> rust quality: cargo fmt --all -- --check"
run_cargo fmt --all -- --check

if [ "$MODE" = "strict" ]; then
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D warnings"
    run_cargo clippy --locked --all-targets -- -D warnings
else
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D clippy::correctness"
    run_cargo clippy --locked --all-targets -- -D clippy::correctness
fi
