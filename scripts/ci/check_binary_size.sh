#!/usr/bin/env bash
# Check binary file size against safeguard thresholds.
#
# Usage: check_binary_size.sh <binary_path> [label]
#
# Arguments:
#   binary_path  Path to the binary to check (required)
#   label        Optional label for step summary (e.g. target triple)
#
# Thresholds:
#   macOS / default host:
#     >20MB  — hard error (safeguard)
#     >15MB  — warning (advisory)
#   Linux host:
#     >23MB  — hard error (safeguard)
#     >20MB  — warning (advisory)
#   All hosts:
#     >5MB   — warning (target)
#
# Overrides:
#   BINARY_SIZE_HARD_LIMIT_BYTES
#   BINARY_SIZE_ADVISORY_LIMIT_BYTES
#   BINARY_SIZE_TARGET_LIMIT_BYTES
# Legacy compatibility:
#   BINARY_SIZE_HARD_LIMIT_MB
#   BINARY_SIZE_ADVISORY_MB
#   BINARY_SIZE_TARGET_MB
#
# Writes to GITHUB_STEP_SUMMARY when the variable is set and label is provided.

set -euo pipefail

BIN="${1:?Usage: check_binary_size.sh <binary_path> [label]}"
LABEL="${2:-}"

if [ ! -f "$BIN" ]; then
  echo "::error::Binary not found at $BIN"
  exit 1
fi

# macOS stat uses -f%z, Linux stat uses -c%s
SIZE=$(stat -f%z "$BIN" 2>/dev/null || stat -c%s "$BIN")
SIZE_MB=$((SIZE / 1024 / 1024))
echo "Binary size: ${SIZE_MB}MB ($SIZE bytes)"

# Default thresholds.
HARD_LIMIT_BYTES=20971520     # 20MB
ADVISORY_LIMIT_BYTES=15728640 # 15MB
TARGET_LIMIT_BYTES=5242880    # 5MB

# Linux host builds are typically larger than macOS builds.
HOST_OS="$(uname -s 2>/dev/null || echo "")"
HOST_OS_LC="$(printf '%s' "$HOST_OS" | tr '[:upper:]' '[:lower:]')"
if [ "$HOST_OS_LC" = "linux" ]; then
  HARD_LIMIT_BYTES=24117248     # 23MB
  ADVISORY_LIMIT_BYTES=20971520 # 20MB
fi

# Explicit env overrides always win.
if [ -n "${BINARY_SIZE_HARD_LIMIT_BYTES:-}" ]; then
  HARD_LIMIT_BYTES="$BINARY_SIZE_HARD_LIMIT_BYTES"
fi
if [ -n "${BINARY_SIZE_ADVISORY_LIMIT_BYTES:-}" ]; then
  ADVISORY_LIMIT_BYTES="$BINARY_SIZE_ADVISORY_LIMIT_BYTES"
fi
if [ -n "${BINARY_SIZE_TARGET_LIMIT_BYTES:-}" ]; then
  TARGET_LIMIT_BYTES="$BINARY_SIZE_TARGET_LIMIT_BYTES"
fi

# Backward-compatible MB overrides (used in older workflow configs).
if [ -z "${BINARY_SIZE_HARD_LIMIT_BYTES:-}" ] && [ -n "${BINARY_SIZE_HARD_LIMIT_MB:-}" ]; then
  HARD_LIMIT_BYTES=$((BINARY_SIZE_HARD_LIMIT_MB * 1024 * 1024))
fi
if [ -z "${BINARY_SIZE_ADVISORY_LIMIT_BYTES:-}" ] && [ -n "${BINARY_SIZE_ADVISORY_MB:-}" ]; then
  ADVISORY_LIMIT_BYTES=$((BINARY_SIZE_ADVISORY_MB * 1024 * 1024))
fi
if [ -z "${BINARY_SIZE_TARGET_LIMIT_BYTES:-}" ] && [ -n "${BINARY_SIZE_TARGET_MB:-}" ]; then
  TARGET_LIMIT_BYTES=$((BINARY_SIZE_TARGET_MB * 1024 * 1024))
fi

HARD_LIMIT_MB=$((HARD_LIMIT_BYTES / 1024 / 1024))
ADVISORY_LIMIT_MB=$((ADVISORY_LIMIT_BYTES / 1024 / 1024))
TARGET_LIMIT_MB=$((TARGET_LIMIT_BYTES / 1024 / 1024))

if [ -n "$LABEL" ] && [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  echo "### Binary Size: $LABEL" >> "$GITHUB_STEP_SUMMARY"
  echo "- Size: ${SIZE_MB}MB ($SIZE bytes)" >> "$GITHUB_STEP_SUMMARY"
  echo "- Limits: hard=${HARD_LIMIT_MB}MB advisory=${ADVISORY_LIMIT_MB}MB target=${TARGET_LIMIT_MB}MB" >> "$GITHUB_STEP_SUMMARY"
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
