#!/usr/bin/env bash
# Check binary file size against safeguard thresholds.
#
# Usage: check_binary_size.sh <binary_path> [label]
#
# Arguments:
#   binary_path  Path to the binary to check (required)
#   label        Optional label for step summary (e.g. target triple)
#
# Environment:
#   BINARY_SIZE_LIMIT_MB  Override hard-error threshold (default: 50)
#
# Thresholds (defaults):
#   >50MB  — hard error (safeguard)
#   >40MB  — warning (advisory)
#   >20MB  — warning (target)
#
# Writes to GITHUB_STEP_SUMMARY when the variable is set and label is provided.

set -euo pipefail

BIN="${1:?Usage: check_binary_size.sh <binary_path> [label]}"
LABEL="${2:-}"

if [ ! -f "$BIN" ]; then
  echo "::error::Binary not found at $BIN"
  exit 1
fi

# Configurable hard limit (in MB), default 50MB
LIMIT_MB="${BINARY_SIZE_LIMIT_MB:-50}"
LIMIT_BYTES=$((LIMIT_MB * 1024 * 1024))
WARN_BYTES=$((40 * 1024 * 1024))
TARGET_BYTES=$((20 * 1024 * 1024))

# macOS stat uses -f%z, Linux stat uses -c%s
SIZE=$(stat -f%z "$BIN" 2>/dev/null || stat -c%s "$BIN")
SIZE_MB=$((SIZE / 1024 / 1024))
echo "Binary size: ${SIZE_MB}MB ($SIZE bytes)"

if [ -n "$LABEL" ] && [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  echo "### Binary Size: $LABEL" >> "$GITHUB_STEP_SUMMARY"
  echo "- Size: ${SIZE_MB}MB ($SIZE bytes)" >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$SIZE" -gt "$LIMIT_BYTES" ]; then
  echo "::error::Binary exceeds ${LIMIT_MB}MB safeguard (${SIZE_MB}MB)"
  exit 1
elif [ "$SIZE" -gt "$WARN_BYTES" ]; then
  echo "::warning::Binary exceeds 40MB advisory target (${SIZE_MB}MB)"
elif [ "$SIZE" -gt "$TARGET_BYTES" ]; then
  echo "::warning::Binary exceeds 20MB target (${SIZE_MB}MB)"
else
  echo "Binary size within target."
fi
