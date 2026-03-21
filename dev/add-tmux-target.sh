#!/usr/bin/env bash
# add-tmux-target.sh — add or update a tmux_target entry in config.toml
#
# Usage:
#   ./dev/add-tmux-target.sh <room_id> <tmux_target>
#
# Examples:
#   ./dev/add-tmux-target.sh '!abc123:dustinllm.local' 'main:myproject'
#   ./dev/add-tmux-target.sh --list          # show current targets + available windows

set -euo pipefail

CONFIG="${ZEROCLAW_CONFIG_DIR:-$HOME/.zeroclaw}/config.toml"

if [[ "${1:-}" == "--list" ]]; then
    echo "=== Configured tmux targets ==="
    python3 -c "
import re
in_section = False
for line in open('$CONFIG'):
    line = line.rstrip()
    if re.match(r'^\[tmux_targets\]', line):
        in_section = True; continue
    if in_section and re.match(r'^\[', line): break
    if in_section and line.strip():
        print(' ', line)
"
    echo ""
    echo "=== Available tmux windows ==="
    tmux list-windows -a 2>/dev/null | awk '{print "  " $0}' || echo "  (tmux not running)"
    exit 0
fi

ROOM_ID="${1:-}"
TMUX_TARGET="${2:-}"

if [[ -z "$ROOM_ID" || -z "$TMUX_TARGET" ]]; then
    echo "Usage: $0 <room_id> <tmux_target>" >&2
    echo "       $0 --list" >&2
    exit 1
fi

if [[ ! -f "$CONFIG" ]]; then
    echo "Config not found: $CONFIG" >&2
    exit 1
fi

# Verify the tmux target exists
if ! tmux has-session -t "$TMUX_TARGET" 2>/dev/null && \
   ! tmux list-windows -a 2>/dev/null | grep -qF "${TMUX_TARGET#*:}"; then
    echo "Warning: tmux target '$TMUX_TARGET' not found in current sessions" >&2
fi

# Check if already present
if grep -qF "\"$ROOM_ID\"" "$CONFIG"; then
    # Update existing entry
    python3 -c "
import re, sys
config = open('$CONFIG').read()
pattern = r'(\"' + re.escape('$ROOM_ID') + r'\"\s*=\s*)\"[^\"]+\"'
replacement = r'\g<1>\"$TMUX_TARGET\"'
updated = re.sub(pattern, replacement, config)
open('$CONFIG', 'w').write(updated)
print('Updated: $ROOM_ID -> $TMUX_TARGET')
"
else
    # Insert after [tmux_targets] section
    python3 -c "
lines = open('$CONFIG').readlines()
out = []
inserted = False
in_tmux = False
for line in lines:
    out.append(line)
    if line.strip() == '[tmux_targets]':
        in_tmux = True
    elif in_tmux and line.strip().startswith('['):
        if not inserted:
            out.insert(-1, '\"$ROOM_ID\" = \"$TMUX_TARGET\"\n')
            inserted = True
        in_tmux = False
if in_tmux and not inserted:
    out.append('\"$ROOM_ID\" = \"$TMUX_TARGET\"\n')
open('$CONFIG', 'w').writelines(out)
print('Added: $ROOM_ID -> $TMUX_TARGET')
"
fi

echo "Done. Restart zeroclaw to pick up the change (or send 'restart' in Matrix)."
