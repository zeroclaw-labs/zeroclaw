#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
APP_NAME="MoA"
APP_BUNDLE="$PROJECT_DIR/target/$APP_NAME.app"
BINARY="$PROJECT_DIR/target/release/zeroclaw"

echo "Building MoA.app..."

if [ ! -f "$BINARY" ]; then
    echo "zeroclaw binary not found. Building..."
    (cd "$PROJECT_DIR" && cargo build --release)
fi

rm -rf "$APP_BUNDLE"

mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

cp "$SCRIPT_DIR/Info.plist" "$APP_BUNDLE/Contents/"

echo "  Compiling Swift app..."
swiftc \
    -O \
    -o "$APP_BUNDLE/Contents/MacOS/$APP_NAME" \
    -framework Cocoa \
    -framework WebKit \
    "$SCRIPT_DIR/main.swift"

cp "$BINARY" "$APP_BUNDLE/Contents/MacOS/zeroclaw-engine"

# Create icon from MoA icon
ICON_SRC="$PROJECT_DIR/web/dist/MoA_icon_128px.png"
if [ -f "$ICON_SRC" ]; then
    echo "  Creating app icon..."
    ICONSET="/tmp/MoA_AppIcon.iconset"
    mkdir -p "$ICONSET"
    sips -z 16 16     "$ICON_SRC" --out "$ICONSET/icon_16x16.png"      2>/dev/null
    sips -z 32 32     "$ICON_SRC" --out "$ICONSET/icon_16x16@2x.png"   2>/dev/null
    sips -z 32 32     "$ICON_SRC" --out "$ICONSET/icon_32x32.png"      2>/dev/null
    sips -z 64 64     "$ICON_SRC" --out "$ICONSET/icon_32x32@2x.png"   2>/dev/null
    sips -z 128 128   "$ICON_SRC" --out "$ICONSET/icon_128x128.png"    2>/dev/null
    sips -z 128 128   "$ICON_SRC" --out "$ICONSET/icon_128x128@2x.png" 2>/dev/null
    sips -z 128 128   "$ICON_SRC" --out "$ICONSET/icon_256x256.png"    2>/dev/null
    sips -z 128 128   "$ICON_SRC" --out "$ICONSET/icon_256x256@2x.png" 2>/dev/null
    sips -z 128 128   "$ICON_SRC" --out "$ICONSET/icon_512x512.png"    2>/dev/null
    sips -z 128 128   "$ICON_SRC" --out "$ICONSET/icon_512x512@2x.png" 2>/dev/null
    iconutil -c icns "$ICONSET" -o "$APP_BUNDLE/Contents/Resources/AppIcon.icns" 2>/dev/null || true
    rm -rf "$ICONSET"
fi

# Also copy the profile image as resource
PROFILE_SRC="$PROJECT_DIR/web/dist/Gemini_Generated_Image_MoA_profile_resized.png"
if [ -f "$PROFILE_SRC" ]; then
    cp "$PROFILE_SRC" "$APP_BUNDLE/Contents/Resources/"
fi

echo ""
echo "Built: $APP_BUNDLE"
echo "Size: $(du -sh "$APP_BUNDLE" | cut -f1)"
echo ""
echo "Run with:"
echo "  open $APP_BUNDLE"
