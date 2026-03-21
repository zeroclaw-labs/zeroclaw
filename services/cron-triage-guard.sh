#!/usr/bin/env bash
# cron-triage-guard.sh — zero-token guard for the triage agent.
#
# Checks if any tickets are in the triage stage. If none, exits immediately
# without spending any tokens. If tickets exist, invokes the zeroclaw agent
# to apply judgment (advance clear tickets, annotate unclear ones).

set -euo pipefail

ZEROCLAW_BIN="${ZEROCLAW_BIN:-$(which zeroclaw 2>/dev/null || echo "$HOME/.cargo/bin/zeroclaw")}"
ZEROCLAW_CONFIG_DIR="${ZEROCLAW_CONFIG_DIR:-$HOME/.zeroclaw}"

TRIAGE_PROMPT="Proactive ticket triage: Check tk list to find tickets in triage stage. Advance up to 3 clear tickets to spec. For chore/docs tickets that are fully described, advance to implement. Add notes to unclear tickets. Skip if no triage tickets."

# Zero-token check: any tickets in triage?
triage_output="$(tk pipeline --stage triage 2>/dev/null || true)"
if echo "$triage_output" | grep -qiE "empty|no tickets|not a ticket"; then
    echo "No triage tickets — skipping agent." >&2
    exit 0
fi

count="$(echo "$triage_output" | grep -cE '^\S' || true)"
echo "Found triage tickets (${count:-some}), invoking agent..." >&2

exec "$ZEROCLAW_BIN" agent \
    --config-dir "$ZEROCLAW_CONFIG_DIR" \
    -m "$TRIAGE_PROMPT"
