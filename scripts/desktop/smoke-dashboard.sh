#!/usr/bin/env bash
set -euo pipefail

# smoke-dashboard.sh: prove a staged desktop kernel serves the web dashboard
# the way a freshly installed desktop app first launches it: from an empty
# working directory, with an isolated config directory, and no dashboard
# files on disk to fall back to. A kernel built without `--features
# embedded-web` answers the dashboard root with 503 and fails this check.
#
# Usage:
#   scripts/desktop/smoke-dashboard.sh <kernel-binary> [port]
#
# Runs under bash on macOS, Linux, and Windows (Git Bash). On failure it
# prints the last response and the daemon log, then exits non-zero.

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <kernel-binary> [port]" >&2
  exit 2
fi

kernel="$1"
port="${2:-42618}"
host="127.0.0.1"
origin="http://$host:$port"
ready_timeout_secs=120

if [[ ! -f "$kernel" ]]; then
  echo "smoke-dashboard: kernel not found: $kernel" >&2
  exit 2
fi
kernel="$(cd "$(dirname "$kernel")" && pwd)/$(basename "$kernel")"

# Native Windows executables need Windows paths for their arguments.
native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}

if command -v cygpath >/dev/null 2>&1; then
  smoke_root="$(mktemp -d)"
else
  # Keep the root short: the daemon's Unix socket path is length-limited.
  smoke_root="$(mktemp -d /tmp/zc-smoke.XXXXXX)"
fi
smoke_cwd="$smoke_root/cwd"
smoke_home="$smoke_root/home"
xdg_data_home="$smoke_root/xdg-data"
config_dir="$smoke_root/config"
daemon_log="$smoke_root/daemon.log"
body="$smoke_root/body.html"
mkdir -p "$smoke_cwd" "$smoke_home" "$xdg_data_home" "$config_dir"

daemon_pid=""

stop_daemon() {
  [[ -n "$daemon_pid" ]] || return 0
  kill "$daemon_pid" 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    kill -0 "$daemon_pid" 2>/dev/null || break
    sleep 1
  done
  if kill -0 "$daemon_pid" 2>/dev/null; then
    if [[ -r "/proc/$daemon_pid/winpid" ]]; then
      taskkill //F //T //PID "$(cat "/proc/$daemon_pid/winpid")" >/dev/null 2>&1 || true
    else
      kill -9 "$daemon_pid" 2>/dev/null || true
    fi
    sleep 1
  fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    wait "$daemon_pid" 2>/dev/null || true
  fi
  daemon_pid=""
}

cleanup() {
  local status=$?
  stop_daemon
  if [[ "$status" -ne 0 && -f "$daemon_log" ]]; then
    echo "--- daemon log ($daemon_log) ---" >&2
    cat "$daemon_log" >&2 || true
    echo "--- end daemon log ---" >&2
  fi
  rm -rf "$smoke_root" 2>/dev/null || true
  exit "$status"
}
trap cleanup EXIT

cd "$smoke_cwd"
HOME="$smoke_home" XDG_DATA_HOME="$xdg_data_home" \
  "$kernel" --config-dir "$(native_path "$config_dir")" daemon \
  --host "$host" --port "$port" >"$daemon_log" 2>&1 &
daemon_pid=$!

deadline=$((SECONDS + ready_timeout_secs))
status_code="000"
while (( SECONDS < deadline )); do
  status_code="$(curl --silent --connect-timeout 1 --max-time 2 \
    --output "$body" --write-out '%{http_code}' "$origin/" || true)"
  if [[ "$status_code" == "200" ]]; then
    if grep -Fq 'id="root"' "$body"; then
      echo "smoke-dashboard: $origin/ served the embedded dashboard"
      exit 0
    fi
    echo "smoke-dashboard: $origin/ answered 200 without the dashboard root element" >&2
    head -c 2000 "$body" >&2 || true
    exit 1
  fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo "smoke-dashboard: the daemon exited before serving the dashboard" >&2
    exit 1
  fi
  sleep 1
done

echo "smoke-dashboard: $origin/ did not serve the dashboard within ${ready_timeout_secs}s (last HTTP status: $status_code)" >&2
if [[ -s "$body" ]]; then
  head -c 2000 "$body" >&2 || true
  echo >&2
fi
exit 1
