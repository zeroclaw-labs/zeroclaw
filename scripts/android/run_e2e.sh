#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
SDK_ROOT="${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}"
ADB_BIN="$SDK_ROOT/platform-tools/adb"
APK_PATH="$ROOT_DIR/mobile-app/android/app/build/outputs/apk/debug/app-debug.apk"
SERIAL="${ANDROID_SERIAL:-$($ADB_BIN devices | awk '/^emulator-[0-9]+\s+device$/ {print $1; exit}')}"

if [ ! -x "$ADB_BIN" ]; then
    echo "adb not found. Run scripts/android/setup_sdk.sh first."
    exit 1
fi

if [ ! -f "$APK_PATH" ]; then
    echo "APK not found at $APK_PATH"
    echo "Build it with: cd mobile-app && npm run android:native"
    exit 1
fi

if [ -z "$SERIAL" ]; then
    echo "No running emulator/device found. Start one with scripts/android/start_emulator.sh"
    exit 1
fi

"$ADB_BIN" devices

for device in $($ADB_BIN devices | awk '/^emulator-[0-9]+\s+device$/ {print $1}'); do
    if [ "$device" != "$SERIAL" ]; then
        "$ADB_BIN" -s "$device" emu kill || true
    fi
done

"$ADB_BIN" -s "$SERIAL" install -r "$APK_PATH"

"$ADB_BIN" -s "$SERIAL" shell am start -n com.mobileclaw.app/com.mobileclaw.app.MainActivity
sleep 3

# Emulator inbound SMS simulation.
"$ADB_BIN" -s "$SERIAL" emu sms send +15551230001 "ZeroClaw E2E inbound SMS"

cd "$ROOT_DIR/mobile-app/android"
if [ -x "./gradlew" ]; then
    ./gradlew connectedDebugAndroidTest
else
    gradle connectedDebugAndroidTest
fi

echo "Android E2E completed"
