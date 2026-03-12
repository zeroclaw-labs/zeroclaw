#!/usr/bin/env bash

set -euo pipefail

MODE="correctness"
if [ "${1:-}" = "--strict" ]; then
    MODE="strict"
fi

echo "==> rust quality: cargo fmt --all -- --check"
cargo fmt --all -- --check

if [ "$MODE" = "strict" ]; then
    echo "==> rust quality: cargo clippy --locked --all-targets --features channel-lark -- -D warnings"
    cargo clippy --locked --all-targets --features channel-lark -- -D warnings
else
    echo "==> rust quality: cargo clippy --locked --all-targets --features channel-lark -- -D clippy::correctness"
    cargo clippy --locked --all-targets --features channel-lark -- -D clippy::correctness
fi
