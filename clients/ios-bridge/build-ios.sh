#!/usr/bin/env bash
set -euo pipefail

# ============================================
# ZeroClaw iOS Bridge Build Script
# ============================================
# Builds the Rust static library for iOS targets,
# generates Swift bindings via UniFFI, and packages
# everything into an XCFramework.
#
# Prerequisites:
#   rustup target add aarch64-apple-ios aarch64-apple-ios-sim
#
# Usage:
#   ./build-ios.sh          # Release build (default)
#   ./build-ios.sh debug    # Debug build (faster iteration)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
IOS_APP_DIR="$SCRIPT_DIR/../ios"
FRAMEWORK_DIR="$IOS_APP_DIR/Frameworks"
GENERATED_DIR="$IOS_APP_DIR/ZeroClaw/Generated"

BUILD_MODE="${1:-release}"
CARGO_FLAGS=""
TARGET_DIR_SUFFIX="release"

if [ "$BUILD_MODE" = "debug" ]; then
    TARGET_DIR_SUFFIX="debug"
else
    CARGO_FLAGS="--release"
fi

LIB_NAME="libzeroclaw_ios.a"

echo "=== ZeroClaw iOS Bridge Build ==="
echo "Mode: $BUILD_MODE"
echo ""

# ---- Step 0: Ensure Xcode developer directory is active ----
ACTIVE_DEV_DIR=$(xcode-select -p 2>/dev/null || true)
if [[ "$ACTIVE_DEV_DIR" == */CommandLineTools* ]]; then
    echo "ERROR: Active developer directory is CommandLineTools, not Xcode."
    echo ""
    echo "  iOS SDK requires a full Xcode installation. Run this once:"
    echo ""
    echo "    sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
    echo ""
    echo "  Then re-run this script."
    exit 1
fi

# Verify iOS SDK is available
if ! xcrun --sdk iphoneos --show-sdk-path &>/dev/null; then
    echo "ERROR: iOS SDK not found. Ensure Xcode is installed with iOS platform support."
    echo "  Open Xcode → Settings → Platforms → install iOS if missing."
    exit 1
fi

echo "[0/5] Xcode developer directory: $(xcode-select -p)"

# Export DEVELOPER_DIR so rustc/cargo internal xcrun calls resolve the
# correct Xcode SDK instead of falling back to CommandLineTools.
export DEVELOPER_DIR="$(xcode-select -p)"

echo ""

# ---- Step 1: Check required Rust targets ----
echo "[1/5] Checking Rust targets..."

check_target() {
    if ! rustup target list --installed | grep -q "$1"; then
        echo "  Installing target: $1"
        rustup target add "$1"
    else
        echo "  Target ready: $1"
    fi
}

check_target "aarch64-apple-ios"
check_target "aarch64-apple-ios-sim"

# ---- Step 2: Build for all iOS targets ----
echo ""
echo "[2/5] Building Rust library..."

cd "$SCRIPT_DIR"

echo "  Building for aarch64-apple-ios (device)..."
cargo build $CARGO_FLAGS --target aarch64-apple-ios

echo "  Building for aarch64-apple-ios-sim (simulator)..."
cargo build $CARGO_FLAGS --target aarch64-apple-ios-sim

# ios-bridge has its own [workspace], so cargo outputs to the local target/ dir.
TARGET_BASE="$SCRIPT_DIR/target"
DEVICE_LIB="$TARGET_BASE/aarch64-apple-ios/$TARGET_DIR_SUFFIX/$LIB_NAME"
SIM_LIB="$TARGET_BASE/aarch64-apple-ios-sim/$TARGET_DIR_SUFFIX/$LIB_NAME"

# Verify libraries exist
for lib in "$DEVICE_LIB" "$SIM_LIB"; do
    if [ ! -f "$lib" ]; then
        echo "ERROR: Library not found: $lib"
        exit 1
    fi
done

echo "  Device library: $(du -h "$DEVICE_LIB" | cut -f1)"
echo "  Simulator library: $(du -h "$SIM_LIB" | cut -f1)"

# ---- Step 3: Generate Swift bindings ----
echo ""
echo "[3/5] Generating Swift bindings..."

# Build the uniffi-bindgen binary for the host
cargo build --bin uniffi-bindgen

HOST_LIB="$TARGET_BASE/debug/libzeroclaw_ios.a"
# For release, the host lib may not exist — use debug for bindgen
if [ ! -f "$HOST_LIB" ]; then
    echo "  Building host library for binding generation..."
    cargo build --lib
    HOST_LIB="$TARGET_BASE/debug/libzeroclaw_ios.a"
fi

mkdir -p "$GENERATED_DIR"

cargo run --bin uniffi-bindgen generate \
    --library "$HOST_LIB" \
    --language swift \
    --out-dir "$GENERATED_DIR"

echo "  Swift bindings generated in: $GENERATED_DIR"

# ---- Step 4: Create XCFramework ----
echo ""
echo "[4/5] Creating XCFramework..."

XCFRAMEWORK_DIR="$FRAMEWORK_DIR/ZeroClawCore.xcframework"

# Clean previous build
rm -rf "$XCFRAMEWORK_DIR"
mkdir -p "$FRAMEWORK_DIR"

# Extract the module map and header from generated bindings
HEADER_FILE="$GENERATED_DIR/zeroclaw_iosFFI.h"
MODULEMAP_FILE="$GENERATED_DIR/zeroclaw_iosFFI.modulemap"

if [ ! -f "$HEADER_FILE" ] || [ ! -f "$MODULEMAP_FILE" ]; then
    echo "ERROR: UniFFI header or modulemap not found in $GENERATED_DIR"
    echo "  Expected: $HEADER_FILE"
    echo "  Expected: $MODULEMAP_FILE"
    exit 1
fi

# Prepare a headers directory containing the C header and module map.
HEADERS_DIR=$(mktemp -d)
cp "$HEADER_FILE" "$HEADERS_DIR/"
cp "$MODULEMAP_FILE" "$HEADERS_DIR/module.modulemap"

# xcodebuild -create-xcframework expects library files with proper extensions.
xcodebuild -create-xcframework \
    -library "$DEVICE_LIB" \
    -headers "$HEADERS_DIR" \
    -library "$SIM_LIB" \
    -headers "$HEADERS_DIR" \
    -output "$XCFRAMEWORK_DIR"

rm -rf "$HEADERS_DIR"

echo "  XCFramework created: $XCFRAMEWORK_DIR"

# ---- Step 5: Summary ----
echo ""
echo "[5/5] Build complete!"
echo ""
echo "Output:"
echo "  XCFramework: $XCFRAMEWORK_DIR"
echo "  Swift bindings: $GENERATED_DIR"
echo ""
echo "Next steps:"
echo "  1. Open clients/ios/ZeroClaw.xcodeproj in Xcode"
echo "  2. Add ZeroClawCore.xcframework to 'Frameworks, Libraries, and Embedded Content'"
echo "  3. Build and run on simulator or device"
