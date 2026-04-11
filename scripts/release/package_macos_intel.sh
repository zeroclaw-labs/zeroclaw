#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TARGET_TRIPLE="x86_64-apple-darwin"
VERSION="${VERSION:-$(git -C "$ROOT_DIR" describe --tags --always)}"
OUT_DIR="$ROOT_DIR/dist"
APP_DIR="$OUT_DIR/ClawPilot-macos-intel"
NODE_BIN="${NODE_BIN:-$(command -v node)}"

if [[ -z "$NODE_BIN" || ! -x "$NODE_BIN" ]]; then
  echo "NODE_BIN must point to an executable node binary"
  exit 1
fi

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/bin" "$APP_DIR/runtime" "$APP_DIR/mission-control"

(cd "$ROOT_DIR" && cargo build --release --target "$TARGET_TRIPLE")
cp "$ROOT_DIR/target/$TARGET_TRIPLE/release/zeroclaw" "$APP_DIR/bin/zeroclaw"
chmod +x "$APP_DIR/bin/zeroclaw"

pushd "$ROOT_DIR/mission-control" >/dev/null
npm ci
npm run build
popd >/dev/null

cp -R "$ROOT_DIR/mission-control/.next/standalone/." "$APP_DIR/mission-control/"
cp -R "$ROOT_DIR/mission-control/.next/static" "$APP_DIR/mission-control/.next/static"
cp -R "$ROOT_DIR/mission-control/public" "$APP_DIR/mission-control/public"
cp "$NODE_BIN" "$APP_DIR/bin/node"
chmod +x "$APP_DIR/bin/node"

cat > "$APP_DIR/start-clawpilot.command" <<'LAUNCH'
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_HOME="${HOME}/.clawpilot"
QUEUE_DIR="$APP_HOME/queue/default"
RESULTS_DIR="$APP_HOME/results"
MISSION_DIR="$APP_HOME/mission-control"
LOG_DIR="$APP_HOME/logs"
mkdir -p "$QUEUE_DIR" "$RESULTS_DIR" "$MISSION_DIR" "$LOG_DIR"

"$SCRIPT_DIR/bin/zeroclaw" daemon \
  --host 127.0.0.1 \
  --port 8080 \
  --job-queue "$QUEUE_DIR" \
  --results-dir "$RESULTS_DIR" \
  >"$LOG_DIR/runtime.log" 2>&1 &

RUNTIME_QUEUE_ROOT="$APP_HOME/queue" \
RUNTIME_RESULTS_ROOT="$RESULTS_DIR" \
MISSION_CONTROL_DATA_ROOT="$MISSION_DIR" \
HOSTNAME=127.0.0.1 \
PORT=4310 \
"$SCRIPT_DIR/bin/node" "$SCRIPT_DIR/mission-control/server.js" \
  >"$LOG_DIR/mission-control.log" 2>&1 &

sleep 1
open "http://127.0.0.1:4310"
echo "ClawPilot started. Logs: $LOG_DIR"
LAUNCH
chmod +x "$APP_DIR/start-clawpilot.command"

cat > "$APP_DIR/stop-clawpilot.command" <<'STOP'
#!/usr/bin/env bash
set -euo pipefail
pkill -f "mission-control/server.js" || true
pkill -f "zeroclaw daemon" || true
echo "ClawPilot processes stopped."
STOP
chmod +x "$APP_DIR/stop-clawpilot.command"

cp "$ROOT_DIR/docs/install-macos-intel.md" "$APP_DIR/README-install.md" 2>/dev/null || true

pushd "$OUT_DIR" >/dev/null
zip -qry "ClawPilot-macos-intel-${VERSION}.zip" "ClawPilot-macos-intel"
hdiutil create -volname "ClawPilot" -srcfolder "ClawPilot-macos-intel" -ov -format UDZO "ClawPilot-macos-intel-${VERSION}.dmg"
popd >/dev/null

echo "Created:"
echo "- $OUT_DIR/ClawPilot-macos-intel-${VERSION}.zip"
echo "- $OUT_DIR/ClawPilot-macos-intel-${VERSION}.dmg"
