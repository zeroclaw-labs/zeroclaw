#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST_PATH="$REPO_ROOT/firmware/zeroclaw-fw-protocol/Cargo.toml"

echo "==> firmware protocol: checking formatting"
cargo fmt --manifest-path "$MANIFEST_PATH" --all -- --check

echo "==> firmware protocol: running strict Clippy"
cargo clippy --locked --manifest-path "$MANIFEST_PATH" --all-targets --all-features -- -D warnings

echo "==> firmware protocol: running locked tests"
cargo test --locked --manifest-path "$MANIFEST_PATH"
