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

# 4. Nested AGENTS.md/CLAUDE.md pairing: every CLAUDE.md must have a sibling
#    AGENTS.md, and every nested CLAUDE.md must reference @AGENTS.md
while IFS= read -r claude_file; do
    dir="$(dirname "$claude_file")"
    rel="${claude_file#"$REPO_ROOT/"}"

    # Skip repo root (already checked above)
    [ "$dir" = "$REPO_ROOT" ] && continue

    if [ ! -f "$dir/AGENTS.md" ]; then
        echo "ERROR: $rel exists but $dir/AGENTS.md is missing."
        echo "  Every nested CLAUDE.md must have a sibling AGENTS.md."
        EXIT_CODE=1
    elif [ -L "$dir/AGENTS.md" ]; then
        echo "ERROR: $(dirname "$rel")/AGENTS.md is a symlink — must be a regular file."
        EXIT_CODE=1
    fi

    if ! grep -q '@AGENTS\.md' "$claude_file"; then
        echo "ERROR: $rel does not contain an @AGENTS.md reference."
        EXIT_CODE=1
    fi
done < <(find "$REPO_ROOT/src" -name 'CLAUDE.md' -type f 2>/dev/null)

# 5. Orphan check: nested AGENTS.md without a sibling CLAUDE.md
while IFS= read -r agents_file; do
    dir="$(dirname "$agents_file")"
    rel="${agents_file#"$REPO_ROOT/"}"

    [ "$dir" = "$REPO_ROOT" ] && continue

    if [ ! -f "$dir/CLAUDE.md" ]; then
        echo "WARN: $rel exists but $dir/CLAUDE.md is missing (orphan AGENTS.md)."
    fi
done < <(find "$REPO_ROOT/src" -name 'AGENTS.md' -type f 2>/dev/null)

if [ "$EXIT_CODE" -eq 0 ]; then
    echo "agents_md_gate: OK"
fi

exit "$EXIT_CODE"
