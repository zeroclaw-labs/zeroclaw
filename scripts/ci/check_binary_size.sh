#!/usr/bin/env bash
# Check binary file size against the configured safeguard.
#
# Usage: check_binary_size.sh <binary_path> [label]
#
# Arguments:
#   binary_path  Path to the binary to check (required)
#   label        Optional label for step summary (e.g. target triple)
#
# BINARY_SIZE_HARD_LIMIT sets the hard error threshold (default: 20 MiB for CI;
# release callers supply their own cap). The former 5/15 MiB advisory targets
# no longer describe current distributions; retain measured bytes and the hard
# gate instead of warning on every release.
#
# Writes to GITHUB_STEP_SUMMARY when the variable is set and label is provided.

set -euo pipefail

BIN="${1:?Usage: check_binary_size.sh <binary_path> [label]}"
LABEL="${2:-}"
HARD_LIMIT="${BINARY_SIZE_HARD_LIMIT:-20971520}"  # default 20MB

if [ ! -f "$BIN" ]; then
  echo "::error::Binary not found at $BIN"
  exit 1
fi

# macOS stat uses -f%z, Linux stat uses -c%s
SIZE=$(stat -f%z "$BIN" 2>/dev/null || stat -c%s "$BIN")
SIZE_MB=$((SIZE / 1024 / 1024))
echo "Binary size: ${SIZE_MB}MB ($SIZE bytes)"

if [ -n "$LABEL" ] && [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  echo "### Binary Size: $LABEL" >> "$GITHUB_STEP_SUMMARY"
  echo "- Size: ${SIZE_MB}MB ($SIZE bytes)" >> "$GITHUB_STEP_SUMMARY"
fi

HARD_LIMIT_MB=$((HARD_LIMIT / 1024 / 1024))
if [ "$SIZE" -gt "$HARD_LIMIT" ]; then
  echo "::error::Binary exceeds ${HARD_LIMIT_MB}MB safeguard (${SIZE_MB}MB)"
  exit 1
else
  echo "Binary size within safeguard."
fi
