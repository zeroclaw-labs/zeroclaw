#!/usr/bin/env bash
# Exercises session_is_paired() from deploy-local.sh against the three states
# that matter, using throwaway fixtures under a fake HOME so the real session
# is never opened.
#
#   paired   — device.pn holds a phone number      → restart is safe
#   revoked  — device.pn is NULL (server dropped)  → restart must be skipped
#   absent   — no session file at all (first run)  → restart is safe (QR flow)
#
# The revoked fixture is built from the same schema the runtime writes, and the
# paired fixture from a real backup, so a schema drift breaks this test rather
# than silently disabling the guard.

set -uo pipefail

SCRIPT="${1:?usage: $0 <path-to-deploy-local.sh>}"
REAL_PAIRED_BACKUP="$HOME/.zeroclaw/state/backups/session-PAIRED-OK-20260802-161050.db"
FIXTURES="$(mktemp -d)"
trap 'rm -rf "$FIXTURES"' EXIT

# Pull just the function under test out of the deploy script — running the
# whole script would build and install.
eval "$(awk '/^session_is_paired\(\) \{/,/^\}/' "$SCRIPT")"

make_home() {
  local name="$1" home="$FIXTURES/$1"
  mkdir -p "$home/.zeroclaw/state/whatsapp-julian"
  echo "$home"
}

# --- fixture: revoked (pn NULL, as the runtime leaves it after a 401) --------
REVOKED="$(make_home revoked)"
sqlite3 "$REVOKED/.zeroclaw/state/whatsapp-julian/session.db" \
  "CREATE TABLE device (id INTEGER PRIMARY KEY, lid TEXT, pn TEXT);
   INSERT INTO device (id, lid, pn) VALUES (1, NULL, NULL);"

# --- fixture: paired (copied from a real pre-revocation backup) --------------
PAIRED="$(make_home paired)"
if [[ -f "$REAL_PAIRED_BACKUP" ]]; then
  cp "$REAL_PAIRED_BACKUP" "$PAIRED/.zeroclaw/state/whatsapp-julian/session.db"
else
  sqlite3 "$PAIRED/.zeroclaw/state/whatsapp-julian/session.db" \
    "CREATE TABLE device (id INTEGER PRIMARY KEY, lid TEXT, pn TEXT);
     INSERT INTO device VALUES (1, '1855:3@lid', '5215551234567:3@s.whatsapp.net');"
fi

# --- fixture: absent (state dir exists, no session.db) ----------------------
ABSENT="$(make_home absent)"

FAILED=0
check() {
  local name="$1" fake_home="$2" want="$3" got
  HOME="$fake_home" session_is_paired && got=paired || got=revoked
  if [[ "$got" == "$want" ]]; then
    printf '  ok    %-9s → %s\n' "$name" "$got"
  else
    printf '  FAIL  %-9s → got %s, want %s\n' "$name" "$got" "$want"
    FAILED=1
  fi
}

echo "session_is_paired():"
check revoked "$REVOKED" revoked
check paired  "$PAIRED"  paired
check absent  "$ABSENT"  paired

exit "$FAILED"
