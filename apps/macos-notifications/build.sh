#!/bin/bash
set -euo pipefail

source_dir="$(cd "$(dirname "$0")" && pwd)"
app="${1:?Usage: build.sh '/absolute/path/ZeroClaw Notifications.app'}"
case "$app" in /*.app) ;; *) echo "Expected an absolute .app path" >&2; exit 2 ;; esac
if [[ -e "$app" ]] && [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist" 2>/dev/null || true)" != "ai.zeroclawlabs.notifications" ]]; then
    echo "Choose a new path or the existing ZeroClaw Notifications bundle." >&2
    exit 2
fi
mkdir -p "$app/Contents/MacOS"
install -m 644 "$source_dir/Info.plist" "$app/Contents/Info.plist"
xcrun swiftc -O -target "$(uname -m)-apple-macosx13.0" "$source_dir/Delivery.swift" "$source_dir/main.swift" \
    -o "$app/Contents/MacOS/zeroclaw-notify"
codesign --force --sign "${CODE_SIGN_IDENTITY:--}" "$app"
