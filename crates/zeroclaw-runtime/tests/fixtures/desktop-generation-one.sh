#!/bin/sh
printf 'stable-generation-one:%s\n' "$ZEROCLAW_DESKTOP_SUPERVISED"
: > "$ZEROCLAW_DESKTOP_RESTART_MARKER"
mv "$(dirname "$0")/desktop-child.next" "$0"
exit 75
