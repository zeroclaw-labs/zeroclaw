#!/usr/bin/env bash

set -euo pipefail

MODE="correctness"
if [ "${1:-}" = "--strict" ]; then
    MODE="strict"
fi

echo "==> rust quality: cargo fmt --all -- --check"
cargo fmt --all -- --check

CLIPPY_WORKSPACE_ARGS=(--workspace --exclude zeroclaw-desktop --all-targets)

if [ "$MODE" = "strict" ]; then
    echo "==> rust quality: cargo clippy --locked --workspace --exclude zeroclaw-desktop --all-targets --features ci-all -- -D warnings"
    cargo clippy --locked "${CLIPPY_WORKSPACE_ARGS[@]}" --features ci-all -- -D warnings
else
    echo "==> rust quality: cargo clippy --locked --workspace --exclude zeroclaw-desktop --all-targets -- -D clippy::correctness"
    cargo clippy --locked "${CLIPPY_WORKSPACE_ARGS[@]}" -- -D clippy::correctness
fi
