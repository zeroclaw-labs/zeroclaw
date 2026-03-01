#!/usr/bin/env bash
# Check binary file size against safeguard thresholds.
#
# Usage: check_binary_size.sh <binary_path> [label]
#
# Arguments:
#   binary_path  Path to the binary to check (required)
#   label        Optional label for step summary (e.g. target triple)
#
# Thresholds (overridable via env vars):
#   BINARY_SIZE_HARD_LIMIT_MB (default: 20)
#   BINARY_SIZE_ADVISORY_MB (default: 15)
#   BINARY_SIZE_TARGET_MB (default: 5)
#
# Writes to GITHUB_STEP_SUMMARY when the variable is set and label is provided.

set -euo pipefail

BIN="${1:?Usage: check_binary_size.sh <binary_path> [label]}"
LABEL="${2:-}"
HARD_LIMIT_MB="${BINARY_SIZE_HARD_LIMIT_MB:-20}"
ADVISORY_LIMIT_MB="${BINARY_SIZE_ADVISORY_MB:-15}"
TARGET_LIMIT_MB="${BINARY_SIZE_TARGET_MB:-5}"

# Convert MB thresholds to bytes for integer comparisons.
HARD_LIMIT_BYTES=$((HARD_LIMIT_MB * 1024 * 1024))
ADVISORY_LIMIT_BYTES=$((ADVISORY_LIMIT_MB * 1024 * 1024))
TARGET_LIMIT_BYTES=$((TARGET_LIMIT_MB * 1024 * 1024))

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

if [ "$SIZE" -gt "$HARD_LIMIT_BYTES" ]; then
  echo "::error::Binary exceeds ${HARD_LIMIT_MB}MB safeguard (${SIZE_MB}MB)"
  exit 1
elif [ "$SIZE" -gt "$ADVISORY_LIMIT_BYTES" ]; then
  echo "::warning::Binary exceeds ${ADVISORY_LIMIT_MB}MB advisory target (${SIZE_MB}MB)"
elif [ "$SIZE" -gt "$TARGET_LIMIT_BYTES" ]; then
  echo "::warning::Binary exceeds ${TARGET_LIMIT_MB}MB target (${SIZE_MB}MB)"
else
  echo "Binary size within target."
fi
