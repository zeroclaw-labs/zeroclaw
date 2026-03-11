#!/usr/bin/env bash

set -euo pipefail

MODE="correctness"
if [ "${1:-}" = "--strict" ]; then
    MODE="strict"
fi

echo "==> rust quality: cargo fmt --all -- --check"
cargo fmt --all -- --check

if [ "$MODE" = "strict" ]; then
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D warnings"
    cargo clippy --locked --all-targets -- -D warnings
else
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D clippy::correctness"
    cargo clippy --locked --all-targets -- -D clippy::correctness
fi
