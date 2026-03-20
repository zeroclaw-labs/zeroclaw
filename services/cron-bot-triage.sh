#!/usr/bin/env bash
# cron-bot-triage.sh — zero-token per-room triage poster
#
# For a given Matrix room:
#   1. Check tmux pane for pending Claude Code question — notify immediately if found
#   2. Check idle state + dedup (via cron-bot-idle-check.py) — skip ticket post if not needed
#   3. Run `tk list` — collect open tickets
#   4. Format a markdown summary and POST to Matrix as cron-bot (no LLM)
#
# Usage:
#   ./services/cron-bot-triage.sh <room_id> [ticket_dir]
#
# Environment:
#   ZEROCLAW_CONFIG_DIR  — defaults to ~/.zeroclaw
#   CRON_BOT_CONFIG      — path to cron-bot.json (defaults to $ZEROCLAW_CONFIG_DIR/cron-bot.json)
#   DRY_RUN              — set to 1 to print messages without posting

set -euo pipefail

ROOM_ID="${1:-}"
if [[ -z "$ROOM_ID" ]]; then
    echo "Usage: $0 <room_id> [ticket_dir]" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZEROCLAW_CONFIG_DIR="${ZEROCLAW_CONFIG_DIR:-$HOME/.zeroclaw}"
CRON_BOT_CONFIG="${CRON_BOT_CONFIG:-$ZEROCLAW_CONFIG_DIR/cron-bot.json}"
CONFIG_TOML="${ZEROCLAW_CONFIG_DIR}/config.toml"
TICKET_DIR="${2:-$(pwd)/.tickets}"
DRY_RUN="${DRY_RUN:-0}"

if [[ ! -f "$CRON_BOT_CONFIG" ]]; then
    echo "cron-bot config not found: $CRON_BOT_CONFIG" >&2
    exit 1
fi

HOMESERVER="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['homeserver'])")"
ACCESS_TOKEN="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['access_token'])")"

# Helper: POST a message to the room as cron-bot
post_to_room() {
    local msg="$1"
    local txn_suffix="${2:-post}"
    local encoded_room
    encoded_room="$(python3 -c "import urllib.parse, sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$ROOM_ID")"
    local txn_id="cron-${txn_suffix}-$(date -u +%s)"
    local url="${HOMESERVER}/_matrix/client/v3/rooms/${encoded_room}/send/m.room.message/${txn_id}"
    local payload
    payload="$(python3 -c "
import json, sys
msg = sys.argv[1]
print(json.dumps({'msgtype': 'm.text', 'body': msg}))
" "$msg")"

    if [[ "$DRY_RUN" == "1" ]]; then
        echo "=== DRY RUN ($txn_suffix) ==="
        echo "$msg"
        return 0
    fi

    local http_status
    http_status="$(curl -s -o /tmp/cron-bot-post.json -w "%{http_code}" \
        -X PUT "$url" \
        -H "Authorization: Bearer $ACCESS_TOKEN" \
        -H "Content-Type: application/json" \
        -d "$payload")"

    if [[ "$http_status" == "200" ]]; then
        local event_id
        event_id="$(python3 -c "import json; print(json.load(open('/tmp/cron-bot-post.json')).get('event_id','?'))" 2>/dev/null || echo "?")"
        echo "Posted ($txn_suffix) to $ROOM_ID — event_id=$event_id"
    else
        echo "Failed to post ($txn_suffix) to $ROOM_ID — HTTP $http_status" >&2
        cat /tmp/cron-bot-post.json >&2 || true
    fi
}

# ── Step 1: Tmux pending-question check ────────────────────────────────────
# Look up the tmux target for this room from config.toml, then capture the
# pane and check for patterns that indicate Claude Code is waiting for input.
tmux_target=""
if [[ -f "$CONFIG_TOML" ]]; then
    tmux_target="$(python3 -c "
import re, sys
room = sys.argv[1]
in_tmux = False
for line in open(sys.argv[2]):
    line = line.rstrip()
    if re.match(r'^\[tmux_targets\]', line):
        in_tmux = True
        continue
    if in_tmux and re.match(r'^\[', line):
        break
    if in_tmux:
        m = re.match(r'^\"?' + re.escape(room) + r'\"?\s*=\s*\"([^\"]+)\"', line)
        if m:
            print(m.group(1))
            break
" "$ROOM_ID" "$CONFIG_TOML" 2>/dev/null || echo "")"
fi

if [[ -n "$tmux_target" ]]; then
    pane_content="$(tmux capture-pane -p -t "$tmux_target" 2>/dev/null || echo "")"
    if [[ -n "$pane_content" ]]; then
        # Check last 15 non-empty lines for question patterns
        pending="$(echo "$pane_content" | grep -v '^\s*$' | tail -15 | python3 -c "
import sys, re
lines = sys.stdin.read().lower()
patterns = [
    r'\[y/n\]', r'\[yes/no\]', r'\[y/n/s\]', r'\(y/n\)',
    r'do you want.*\?', r'would you like.*\?',
    r'should i .*\?', r'shall i .*\?',
    r'allow.*\?$',
]
for p in patterns:
    if re.search(p, lines):
        print('yes')
        sys.exit(0)
sys.exit(1)
" 2>/dev/null && echo "yes" || echo "")"

        if [[ "$pending" == "yes" ]]; then
            now_utc="$(date -u '+%Y-%m-%d %H:%M UTC')"
            notice="⚠️ **Pending question** in tmux \`${tmux_target}\` — ${now_utc}

Claude Code appears to be waiting for input. Use \`peek\` to see the current state or \`tmux <reply>\` to respond."
            post_to_room "$notice" "tmux-alert"
        fi
    fi
fi

# ── Step 2: Idle + dedup check ─────────────────────────────────────────────
IDLE_CHECK="$SCRIPT_DIR/cron-bot-idle-check.py"
if [[ ! -f "$IDLE_CHECK" ]]; then
    echo "cron-bot-idle-check.py not found at $IDLE_CHECK" >&2
    exit 1
fi

idle_json="$(python3 "$IDLE_CHECK" "$ROOM_ID" --config "$CRON_BOT_CONFIG" 2>&1)" || true
should_post="$(echo "$idle_json" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if d.get('should_post') else 'no')" 2>/dev/null || echo "no")"

if [[ "$should_post" != "yes" ]]; then
    exit 0
fi

# ── Step 3: Collect ticket state ───────────────────────────────────────────
ticket_summary=""

if command -v tk &>/dev/null && [[ -d "$TICKET_DIR" ]]; then
    open_tickets="$(cd "$(dirname "$TICKET_DIR")" && tk list 2>/dev/null | grep -v '^$' | head -40 || echo "")"
    if [[ -n "$open_tickets" ]]; then
        ticket_summary="$open_tickets"
    fi
else
    if [[ -d "$TICKET_DIR" ]]; then
        open_count=0
        open_lines=""
        while IFS= read -r -d '' f; do
            status="$(grep -m1 '^status:' "$f" 2>/dev/null | sed 's/status: *//' | tr -d '\r' || echo "")"
            if [[ "$status" != "done" && "$status" != "closed" && -n "$status" ]]; then
                title="$(grep -m1 '^title:' "$f" 2>/dev/null | sed 's/title: *//' | tr -d '\"' | tr -d '\r' || echo "$f")"
                priority="$(grep -m1 '^priority:' "$f" 2>/dev/null | sed 's/priority: *//' | tr -d '\r' || echo "")"
                open_lines="${open_lines}- [${priority}] ${title}
"
                (( open_count++ )) || true
            fi
        done < <(find "$TICKET_DIR" -name '*.md' -not -name 'README*' -print0 2>/dev/null)

        if [[ $open_count -gt 0 ]]; then
            ticket_summary="**Open Tickets (${open_count}):**
${open_lines}"
        fi
    fi
fi

if [[ -z "$ticket_summary" ]]; then
    exit 0
fi

# ── Step 4: Format and post triage summary ─────────────────────────────────
now_utc="$(date -u '+%Y-%m-%d %H:%M UTC')"
message="**Triage Summary** — ${now_utc}

${ticket_summary}

_Next check in ~4h. Reply to discuss any item._"

post_to_room "$message" "triage"
