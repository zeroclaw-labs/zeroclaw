#!/bin/bash
# Build the Skills PoC to ./pkg. npm-free: cargo + wasm-bindgen-cli only.
# Version pin: dioxus =0.7.10; wasm-bindgen CLI must match the wasm-bindgen
# version resolved in Cargo.lock.
# version resolved in Cargo.lock.
set -euo pipefail
cd "$(dirname "$0")"

TARGET=wasm32-unknown-unknown
PKG_DIR=pkg

if ! rustup target list --installed 2>/dev/null | grep -q "$TARGET"; then
    echo "MISSING TARGET: run 'rustup target add $TARGET'" >&2
    exit 1
fi

cargo build --release --target "$TARGET"

LOCK_VERSION="$(grep -A1 '^name = "wasm-bindgen"' Cargo.lock | grep version | sed 's/.*"\(.*\)".*/\1/' | head -1)"
echo "wasm-bindgen (Cargo.lock): $LOCK_VERSION"

if ! wasm-bindgen --version 2>/dev/null | grep -q "$LOCK_VERSION"; then
    echo "wasm-bindgen CLI does not match Cargo.lock ($LOCK_VERSION)." >&2
    echo "Run: cargo install wasm-bindgen-cli --version $LOCK_VERSION" >&2
    exit 1
fi

rm -rf "$PKG_DIR"
wasm-bindgen "target/$TARGET/release/skills_poc.wasm" \
    --out-dir "$PKG_DIR" --target web

echo
echo "Bundle:"
ls -la "$PKG_DIR/skills_poc_bg.wasm"
gzip -k -f "$PKG_DIR/skills_poc_bg.wasm" 2>/dev/null || true
if [ -f "$PKG_DIR/skills_poc_bg.wasm.gz" ]; then
    echo "gz: $(du -h "$PKG_DIR/skills_poc_bg.wasm.gz" | cut -f1)"
    rm -f "$PKG_DIR/skills_poc_bg.wasm.gz"
fi
echo
echo "Serve with (from web-wasm-poc root): python3 -m http.server 8080"
echo "Then open http://127.0.0.1:8080/"
