#!/usr/bin/env bash
set -euo pipefail

# Build script for test WASM plugins
# Cross-compiles echo-plugin, multi-tool-plugin, and bad-actor-plugin to wasm32-wasip1
# and copies the resulting .wasm files to tests/plugins/artifacts/

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGINS_DIR="$SCRIPT_DIR/tests/plugins"
ARTIFACTS_DIR="$PLUGINS_DIR/artifacts"
TARGET="wasm32-wasip1"

PLUGINS=("echo-plugin" "multi-tool-plugin" "bad-actor-plugin" "http-plugin" "fs-plugin")

# Ensure wasm32-wasip1 target is installed
if ! rustup target list --installed | grep -q "$TARGET"; then
    echo "Installing $TARGET target..."
    rustup target add "$TARGET"
fi

# Build all plugins in release mode
echo "Building test plugins for $TARGET..."
cargo build --release --target "$TARGET" --manifest-path "$PLUGINS_DIR/Cargo.toml"

# Create artifacts directory
mkdir -p "$ARTIFACTS_DIR"

# Copy .wasm files and verify
WASM_TARGET_DIR="$PLUGINS_DIR/target/$TARGET/release"
for plugin in "${PLUGINS[@]}"; do
    # Cargo converts hyphens to underscores in output filenames
    wasm_name="${plugin//-/_}.wasm"
    src="$WASM_TARGET_DIR/$wasm_name"
    dest="$ARTIFACTS_DIR/$wasm_name"

    if [[ ! -f "$src" ]]; then
        echo "ERROR: Expected artifact not found: $src" >&2
        exit 1
    fi

    cp "$src" "$dest"
    echo "  $plugin -> artifacts/$wasm_name ($(du -h "$dest" | cut -f1))"
done

echo "All ${#PLUGINS[@]} plugins built and copied to $ARTIFACTS_DIR"
