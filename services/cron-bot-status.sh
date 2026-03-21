#!/usr/bin/env bash
# cron-bot-status.sh — hourly detection status reporter
#
# Reports tmux pane state and Matrix room idle state to the room.
# Dedup: if the last cron-bot post was also idle/idle, skip posting again.
#
# Usage:
#   ./services/cron-bot-status.sh <room_id>
#
# Environment:
#   ZEROCLAW_CONFIG_DIR  — defaults to ~/.zeroclaw
#   CRON_BOT_CONFIG      — path to cron-bot.json
#   DRY_RUN              — set to 1 to print without posting

set -euo pipefail

ROOM_ID="${1:-}"
if [[ -z "$ROOM_ID" ]]; then
    echo "Usage: $0 <room_id>" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZEROCLAW_CONFIG_DIR="${ZEROCLAW_CONFIG_DIR:-$HOME/.zeroclaw}"
CRON_BOT_CONFIG="${CRON_BOT_CONFIG:-$ZEROCLAW_CONFIG_DIR/cron-bot.json}"
CONFIG_TOML="${ZEROCLAW_CONFIG_DIR}/config.toml"
DRY_RUN="${DRY_RUN:-0}"

HOMESERVER="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['homeserver'])")"
ACCESS_TOKEN="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['access_token'])")"
CRON_BOT_USER="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['user_id'])")"

IDLE_MARKER="state:idle/idle"

# ── Tmux state ──────────────────────────────────────────────────────────────
tmux_target=""
if [[ -f "$CONFIG_TOML" ]]; then
    tmux_target="$(python3 -c "
import re, sys
room = sys.argv[1]
in_tmux = False
for line in open(sys.argv[2]):
    line = line.rstrip()
    if re.match(r'^\[tmux_targets\]', line):
        in_tmux = True; continue
    if in_tmux and re.match(r'^\[', line): break
    if in_tmux:
        m = re.match(r'^\"?' + re.escape(room) + r'\"?\s*=\s*\"([^\"]+)\"', line)
        if m: print(m.group(1)); break
" "$ROOM_ID" "$CONFIG_TOML" 2>/dev/null || echo "")"
fi

tmux_state="no-target"
tmux_detail=""
if [[ -n "$tmux_target" ]]; then
    # Primary signal: what command is currently running in the pane?
    # When Claude Code is active, pane_current_command will be 'claude' or 'node',
    # not a shell. This is far more reliable than content scraping.
    pane_cmd="$(tmux display-message -t "$tmux_target" -p '#{pane_current_command}' 2>/dev/null || echo "")"
    pane="$(tmux capture-pane -p -t "$tmux_target" 2>/dev/null || echo "")"

    shell_commands="bash zsh sh fish dash ksh tcsh csh"
    is_shell=false
    for sc in $shell_commands; do
        if [[ "$pane_cmd" == "$sc" ]]; then is_shell=true; break; fi
    done

    if [[ -n "$pane_cmd" && "$is_shell" == "false" ]]; then
        # Non-shell process running — check content only for question detection
        tmux_state="$(echo "$pane" | python3 -c "
import sys, re
raw = sys.stdin.read(); lower = raw.lower()
question_patterns = [
    r'\[y/n\]', r'\[yes/no\]', r'\[y/n/s\]', r'\(y/n\)',
    r'do you want.*\?', r'would you like.*\?', r'should i .*\?', r'allow.*\?\s*\$',
]
for p in question_patterns:
    if re.search(p, lower):
        print('question'); sys.exit()
print('active')
" 2>/dev/null || echo "active")"
    else
        # Shell is running — fall back to content-based detection
        tmux_state="$(echo "$pane" | grep -v '^\s*$' | tail -20 | python3 -c "
import sys, re
raw = sys.stdin.read(); lower = raw.lower()
spinner_chars = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'
if any(c in raw for c in spinner_chars):
    print('active'); sys.exit()
active_patterns = [
    r'^\s*●\s+(running|thinking|reading|writing|searching|fetching|executing)',
    r'\besc to interrupt\b', r'auto-accept edits on',
]
for p in active_patterns:
    if re.search(p, lower, re.MULTILINE):
        print('active'); sys.exit()
question_patterns = [
    r'\[y/n\]', r'\[yes/no\]', r'\[y/n/s\]', r'\(y/n\)',
    r'do you want.*\?', r'would you like.*\?', r'should i .*\?', r'allow.*\?\s*\$',
]
for p in question_patterns:
    if re.search(p, lower):
        print('question'); sys.exit()
print('idle')
" 2>/dev/null || echo "idle")"
    fi
    tmux_detail=" (\`${tmux_target}\`)"
fi

# ── Matrix room idle state ──────────────────────────────────────────────────
# Retry once on transient errors (continuwuity occasionally returns 400).
idle_json=""
for _attempt in 1 2; do
    idle_json="$(python3 "$SCRIPT_DIR/cron-bot-idle-check.py" "$ROOM_ID" --config "$CRON_BOT_CONFIG" 2>/dev/null || echo "")"
    if echo "$idle_json" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if 'idle' in d else 1)" 2>/dev/null; then
        break
    fi
    sleep 2
done

room_idle="$(echo "$idle_json" | python3 -c "
import json, sys
d = json.load(sys.stdin)
if 'idle' not in d:
    print('unknown')
else:
    print('idle' if d.get('idle') else 'active')
" 2>/dev/null || echo "unknown")"

last_human_ts="$(echo "$idle_json" | python3 -c "
import json, sys
d = json.load(sys.stdin)
ts = d.get('last_ts_human') or 'unknown'
# shorten to HH:MM UTC
print(ts[11:16] + ' UTC' if len(ts) >= 16 else ts)
" 2>/dev/null || echo "")"

last_sender="$(echo "$idle_json" | python3 -c "
import json, sys
d = json.load(sys.stdin)
s = d.get('last_sender') or ''
print(s.split(':')[0].lstrip('@') if s else '')
" 2>/dev/null || echo "")"

# ── Idle/idle dedup ─────────────────────────────────────────────────────────
# If current state is idle/idle, check if cron-bot's last message was also idle/idle.
# If so, skip — no need to repeat the same status.
if [[ "$tmux_state" == "idle" && "$room_idle" == "idle" ]]; then
    encoded_room="$(python3 -c "import urllib.parse, sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$ROOM_ID")"
    last_body="$(python3 -c "
import urllib.request, json, sys
url = sys.argv[1]
token = sys.argv[2]
bot = sys.argv[3]
req = urllib.request.Request(url, headers={'Authorization': f'Bearer {token}'})
try:
    with urllib.request.urlopen(req, timeout=10) as r:
        data = json.load(r)
    for ev in data.get('chunk', []):
        if ev.get('type') == 'm.room.message' and ev.get('sender') == bot:
            print(ev.get('content', {}).get('body', ''))
            sys.exit(0)
except Exception as e:
    pass
" "${HOMESERVER}/_matrix/client/v3/rooms/${encoded_room}/messages?dir=b&limit=30" \
  "$ACCESS_TOKEN" "$CRON_BOT_USER" 2>/dev/null || echo "")"

    if echo "$last_body" | grep -qF "$IDLE_MARKER"; then
        # Last post was also idle/idle — skip
        exit 0
    fi
fi

# ── Format status message ───────────────────────────────────────────────────
now_utc="$(date -u '+%Y-%m-%d %H:%M UTC')"

case "$tmux_state" in
    active)   tmux_icon="⚙️" ;;
    question) tmux_icon="⚠️" ;;
    idle)     tmux_icon="💤" ;;
    *)        tmux_icon="❓" ;;
esac

case "$room_idle" in
    idle)   room_icon="💤" ;;
    active) room_icon="🟢" ;;
    *)      room_icon="❓" ;;
esac

room_detail=""
if [[ -n "$last_sender" && -n "$last_human_ts" ]]; then
    room_detail=" (last: $last_sender @ $last_human_ts)"
fi

message="**Detection Status** — ${now_utc}

${tmux_icon} **tmux**${tmux_detail}: \`${tmux_state}\`
${room_icon} **room**: \`${room_idle}\`${room_detail}"

if [[ "$tmux_state" == "idle" && "$room_idle" == "idle" ]]; then
    message="${message}

_Both idle — triage would run if tickets exist. [$IDLE_MARKER]_"
fi

# ── Post ────────────────────────────────────────────────────────────────────
if [[ "$DRY_RUN" == "1" ]]; then
    echo "=== DRY RUN ==="
    echo "$message"
    exit 0
fi

encoded_room="$(python3 -c "import urllib.parse, sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$ROOM_ID")"
txn_id="cron-status-$(date -u +%s)"
url="${HOMESERVER}/_matrix/client/v3/rooms/${encoded_room}/send/m.room.message/${txn_id}"
payload="$(python3 -c "
import json, sys
print(json.dumps({'msgtype': 'm.text', 'body': sys.argv[1]}))
" "$message")"

http_status="$(curl -s -o /tmp/cron-bot-post.json -w "%{http_code}" \
    -X PUT "$url" \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$payload")"

if [[ "$http_status" == "200" ]]; then
    echo "Posted status to $ROOM_ID (tmux=$tmux_state room=$room_idle)"
else
    echo "Failed to post — HTTP $http_status" >&2
    cat /tmp/cron-bot-post.json >&2 || true
    exit 1
fi
