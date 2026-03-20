#!/usr/bin/env bash
# Start all ZeroClaw services: Parakeet STT, Kokoro TTS, and ZeroClaw daemon.
# Usage: ./services/start-all.sh [stop|restart|status]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PARAKEET_DIR="$SCRIPT_DIR/parakeet-stt"
KOKORO_DIR="$SCRIPT_DIR/kokoro-tts"
ZEROCLAW_BIN="${ZEROCLAW_BIN:-$HOME/.cargo/bin/zeroclaw}"

PARAKEET_LOG="/tmp/parakeet-stt.log"
KOKORO_LOG="/tmp/kokoro-tts.log"
ZEROCLAW_LOG="/tmp/zeroclaw-daemon.log"

pid_for() {
    pgrep -f "$1" 2>/dev/null | head -1
}

status() {
    local label="$1" pattern="$2"
    local pid
    pid=$(pid_for "$pattern")
    if [ -n "$pid" ]; then
        printf "  %-20s \033[32mrunning\033[0m (pid %s)\n" "$label" "$pid"
    else
        printf "  %-20s \033[31mstopped\033[0m\n" "$label"
    fi
}

stop_all() {
    pkill -f "zeroclaw daemon" 2>/dev/null && echo "Stopped zeroclaw" || true
    pkill -f "kokoro-tts/server.py" 2>/dev/null && echo "Stopped kokoro" || true
    pkill -f "parakeet-stt/server.py" 2>/dev/null && echo "Stopped parakeet" || true
    sleep 1
}

start_all() {
    echo "Starting Parakeet STT..."
    if [ -z "$(pid_for 'parakeet-stt/server.py')" ]; then
        nohup "$PARAKEET_DIR/run.sh" > "$PARAKEET_LOG" 2>&1 &
    else
        echo "  Already running"
    fi

    echo "Starting Kokoro TTS..."
    if [ -z "$(pid_for 'kokoro-tts/server.py')" ]; then
        nohup "$KOKORO_DIR/run.sh" > "$KOKORO_LOG" 2>&1 &
    else
        echo "  Already running"
    fi

    # Wait for services to be ready before starting zeroclaw
    echo "Waiting for services..."
    for i in $(seq 1 30); do
        if curl -sf http://127.0.0.1:6008/health >/dev/null 2>&1; then
            break
        fi
        sleep 1
    done

    for i in $(seq 1 15); do
        if curl -sf http://127.0.0.1:6009/health >/dev/null 2>&1; then
            break
        fi
        sleep 1
    done

    echo "Starting ZeroClaw daemon..."
    if [ -z "$(pid_for 'zeroclaw daemon')" ]; then
        nohup "$ZEROCLAW_BIN" daemon > "$ZEROCLAW_LOG" 2>&1 &
    else
        echo "  Already running"
    fi

    sleep 2
    echo ""
    echo "Status:"
    status "Parakeet STT" "parakeet-stt/server.py"
    status "Kokoro TTS" "kokoro-tts/server.py"
    status "ZeroClaw" "zeroclaw daemon"
    echo ""
    echo "Logs:"
    echo "  Parakeet: $PARAKEET_LOG"
    echo "  Kokoro:   $KOKORO_LOG"
    echo "  ZeroClaw: $ZEROCLAW_LOG"
}

case "${1:-start}" in
    stop)
        stop_all
        ;;
    restart)
        stop_all
        start_all
        ;;
    status)
        status "Parakeet STT" "parakeet-stt/server.py"
        status "Kokoro TTS" "kokoro-tts/server.py"
        status "ZeroClaw" "zeroclaw daemon"
        ;;
    start|"")
        start_all
        ;;
    *)
        echo "Usage: $0 [start|stop|restart|status]"
        exit 1
        ;;
esac
