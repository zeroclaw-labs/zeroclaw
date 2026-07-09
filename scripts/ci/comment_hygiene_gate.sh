#!/usr/bin/env bash

# Comment hygiene gate: rejects issue/PR refs, review-process leakage,
# gravitas phrases, dated notes, and sweep-truncation artifacts in
# source comments. String literals are out of scope by design.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# Paths exempt from the gate. Keep this list short; additions are
# reviewed via the diff on this script.
SKIP_PATHS=(
    "scripts/check-pr-title.test.sh"
    "scripts/ci/comment_hygiene_gate.sh"
)

GLOBS=(-g '*.rs' -g '*.toml' -g '*.sh' -g '*.py' -g '*.nix' -g '!target/' -g '!web/' -g '!docs/book/')
for p in "${SKIP_PATHS[@]}"; do
    GLOBS+=(-g "!${p}")
done

fail=0

report() {
    local label="$1"
    local hits="$2"
    if [ -n "$hits" ]; then
        echo "FAIL: ${label}"
        echo "$hits" | head -50
        echo
        fail=1
    fi
}

strip_strings() {
    # Drop content inside double-quoted string literals so assert
    # messages carrying issue refs stay legal, then keep only lines
    # that still contain a comment token.
    sed 's/"\([^"\\]\|\\.\)*"/""/g'
}

comment_lines() {
    # rg output -> filter to comment context, strings removed.
    strip_strings | grep -E '(//|///|//!|(^|[[:space:]])#([[:space:]]|$|[[:alpha:]]))' || true
}

echo "==> comment hygiene: issue/PR refs in comments"
hits="$(rg -n --no-heading "${GLOBS[@]}" '#[0-9]{3,}' | comment_lines | grep -vE '#\[|https?://[^ ]*#|[0-9a-fA-F]{6}' || true)"
report "issue/PR refs (#NNNN) in comments" "$hits"

echo "==> comment hygiene: tracking/see-issue phrasing"
hits="$(rg -in --no-heading "${GLOBS[@]}" '(//|#).*(tracking #|see #|see issue|see PR|fixes #|closes #|resolves #)' | comment_lines || true)"
report "tracking/see-issue phrasing in comments" "$hits"

echo "==> comment hygiene: gravitas phrases"
hits="$(rg -in --no-heading "${GLOBS[@]}" '(sole source of truth|sibling PR|blast radius:|threat model:|rollback plan|canonical contract)' | comment_lines || true)"
report "gravitas phrases in comments" "$hits"

echo "==> comment hygiene: review-process leakage"
hits="$(rg -n --no-heading "${GLOBS[@]}" '(//|#)[^\n]*(NEW in this PR|previous revision of this PR|that started this PR|audit blocker|review pass|Round [0-9]+:)' | comment_lines || true)"
report "review-process leakage in comments" "$hits"

echo "==> comment hygiene: dated notes"
hits="$(rg -in --no-heading "${GLOBS[@]}" '(//|#)[^\n]*(as of 20[0-9]{2}-[0-9]{2}|last verified:)' | comment_lines || true)"
report "dated notes in comments" "$hits"

echo "==> comment hygiene: truncation artifacts"
hits="$(rg -n --no-heading "${GLOBS[@]}" '(//|///|//!|#)[^\n]*\((see|issue|ref|tracking|regression)[[:space:]]*$' | comment_lines | while IFS=: read -r file line _; do
    next="$(sed -n "$((line + 1))p" "$file")"
    if ! printf '%s' "$next" | grep -qE '^[[:space:]]*(//|#)'; then
        echo "${file}:${line}: dangling fragment (next line is not a comment continuation)"
    fi
done || true)"
report "dangling open-paren fragments in comments" "$hits"

hits="$(rg -n --no-heading "${GLOBS[@]}" '(//|///|//!)[[:space:]]*(See|Ref|Tracking)[[:space:]]*[.,;:]?[[:space:]]*$' || true)"
report "bare See/Ref/Tracking stub comments" "$hits"

hits="$(rg -n --no-heading "${GLOBS[@]}" -g '*.rs' '^[[:space:]]*//[/!]?  [a-z]' | grep -vE '//  (since|itself)\b' || true)"
report "double-space lowercase stub comments (likely mid-sentence truncation)" "$hits"

if [ "$fail" -ne 0 ]; then
    echo "Comment hygiene gate failed. Fix the comment or, if a fixture"
    echo "legitimately needs the pattern, add the path to SKIP_PATHS in"
    echo "scripts/ci/comment_hygiene_gate.sh (reviewed via this script's diff)."
    exit 1
fi

echo "Comment hygiene gate passed."
