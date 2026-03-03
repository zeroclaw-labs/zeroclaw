#!/usr/bin/env bash
# build-ios.sh — Build the MoA iOS app with ZeroClaw static library.
#
# Usage:
#   ./scripts/build-ios.sh            # Build debug for simulator
#   ./scripts/build-ios.sh release    # Build release for device
#   ./scripts/build-ios.sh lib-only   # Build only the Rust static library
#
# Requirements:
#   - Xcode 15+ with iOS SDK
#   - Rust with aarch64-apple-ios target: rustup target add aarch64-apple-ios
#   - For simulator: rustup target add aarch64-apple-ios-sim
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
IOS_BRIDGE_DIR="$REPO_DIR/clients/ios-bridge"
IOS_APP_DIR="$REPO_DIR/clients/ios"
MODE="${1:-debug}"

echo "=== MoA iOS Build ==="
echo "Mode: $MODE"

# Step 1: Build the ZeroClaw static library for iOS
echo ""
echo "--- Step 1: Building ZeroClaw iOS static library ---"

if [ "$MODE" = "release" ]; then
    CARGO_FLAGS="--release"
    TARGET="aarch64-apple-ios"
    echo "Target: $TARGET (release, device)"
else
    CARGO_FLAGS=""
    TARGET="aarch64-apple-ios-sim"
    echo "Target: $TARGET (debug, simulator)"
fi

cd "$IOS_BRIDGE_DIR"
cargo build --target "$TARGET" $CARGO_FLAGS

LIB_PATH="$IOS_BRIDGE_DIR/target/$TARGET/$([ "$MODE" = "release" ] && echo release || echo debug)/libzeroclaw_ios.a"
if [ -f "$LIB_PATH" ]; then
    LIB_SIZE=$(du -h "$LIB_PATH" | cut -f1)
    echo "Built: $LIB_PATH ($LIB_SIZE)"
else
    echo "ERROR: Static library not found at $LIB_PATH"
    exit 1
fi

if [ "$MODE" = "lib-only" ]; then
    echo "Library-only build complete."
    exit 0
fi

# Step 2: Build the Xcode project
echo ""
echo "--- Step 2: Building MoA iOS app ---"

cd "$IOS_APP_DIR"

if [ "$MODE" = "release" ]; then
    xcodebuild \
        -project MoA.xcodeproj \
        -scheme MoA \
        -configuration Release \
        -destination "generic/platform=iOS" \
        -archivePath build/MoA.xcarchive \
        archive
    echo ""
    echo "Archive built at: $IOS_APP_DIR/build/MoA.xcarchive"
    echo "To export IPA: xcodebuild -exportArchive -archivePath build/MoA.xcarchive -exportPath build/ -exportOptionsPlist ExportOptions.plist"
else
    xcodebuild \
        -project MoA.xcodeproj \
        -scheme MoA \
        -configuration Debug \
        -destination "platform=iOS Simulator,name=iPhone 16" \
        build
    echo ""
    echo "Debug build complete. Run in Xcode or with: xcrun simctl install booted build/Debug-iphonesimulator/MoA.app"
fi

echo ""
echo "=== MoA iOS Build Complete ==="
