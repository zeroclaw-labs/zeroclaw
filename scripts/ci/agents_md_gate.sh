#!/usr/bin/env bash
# agents_md_gate.sh — verify AGENTS.md / CLAUDE.md pairing
#
# Checks:
#   1. AGENTS.md exists at repo root
#   2. AGENTS.md is a regular file (not a symlink)
#   3. CLAUDE.md contains an @AGENTS.md reference

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
EXIT_CODE=0

# 1. AGENTS.md must exist
if [ ! -e "$REPO_ROOT/AGENTS.md" ]; then
    echo "ERROR: AGENTS.md is missing from the repo root."
    echo "  AGENTS.md is the primary agent instruction file."
    echo "  See: https://agents.md/"
    EXIT_CODE=1
fi

# 2. AGENTS.md must be a regular file, not a symlink
if [ -L "$REPO_ROOT/AGENTS.md" ]; then
    echo "ERROR: AGENTS.md is a symlink — it must be a standalone file."
    echo "  Other tools (GitHub Copilot, Cursor) may not follow symlinks."
    EXIT_CODE=1
fi

# 3. CLAUDE.md must reference @AGENTS.md
if [ -f "$REPO_ROOT/CLAUDE.md" ]; then
    if ! grep -q '@AGENTS\.md' "$REPO_ROOT/CLAUDE.md"; then
        echo "ERROR: CLAUDE.md does not contain an @AGENTS.md reference."
        echo "  CLAUDE.md should point to AGENTS.md as the primary instruction source."
        EXIT_CODE=1
    fi
fi

if [ "$EXIT_CODE" -eq 0 ]; then
    echo "agents_md_gate: OK"
fi

exit "$EXIT_CODE"
