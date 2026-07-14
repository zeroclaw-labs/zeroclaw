#!/usr/bin/env bash

# Comment hygiene gate: rejects issue/PR refs, review-process leakage,
# gravitas phrases, dated notes, and sweep-truncation artifacts in
# source comments. String literals are out of scope by design.
# Optional args: paths to scan (defaults to the whole tree).

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

SCAN_ROOTS=("${@:-.}")

# Paths exempt from the gate. Keep this list short; additions are
# reviewed via the diff on this script.
SKIP_PATHS=(
    "scripts/check-pr-title.test.sh"
    "scripts/ci/comment_hygiene_gate.sh"
    "scripts/ci/comment_hygiene_gate.test.sh"
    ".cargo/audit.toml"
    "deny.toml"
)

GLOBS=(--hidden -g '*.rs' -g '*.toml' -g '*.sh' -g '*.py' -g '*.nix'
    -g '!.git/' -g '!target/' -g '!web/' -g '!docs/book/' -g '!.claude/')
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

# Re-match rg hits against only the comment portion of each line, with
# string literals and URLs removed first. Args: <regex> <flags: i|-> <mode:
# match|dangling>. Input: rg -n --no-heading output. Output: file:line:content
# for true comment hits. "dangling" additionally requires that the next
# source line is NOT a comment continuation.
filter_comments() {
    python3 -c "$PYFILTER" "$1" "$2" "$3"
}

read -r -d '' PYFILTER <<'PY' || true
import re
import sys

pattern = re.compile(sys.argv[1], re.I if sys.argv[2] == "i" else 0)
mode = sys.argv[3]
dq = re.compile(r'"(?:[^"\\]|\\.)*"')
sq = re.compile(r"'(?:[^'\\]|\\.)*'")
url = re.compile(r'https?://\S+')
HASH_EXTS = {"sh", "py", "nix", "toml"}

def comment_of(path, lineno, content):
    ext = path.rsplit(".", 1)[-1]
    text = dq.sub('""', content)
    if ext == "rs":
        idx = text.find("//")
        block = text.find("/*")
        if block >= 0 and (idx < 0 or block < idx):
            end = text.find("*/", block + 2)
            return text[block : end + 2] if end >= 0 else text[block:]
        return text[idx:] if idx >= 0 else None
    if ext in HASH_EXTS:
        text = sq.sub("''", text)
        idx = text.find("#")
        if idx < 0:
            return None
        if lineno == "1" and text.startswith("#!"):
            return None
        return text[idx:]
    return None

def next_is_comment(path, lineno):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            lines = f.readlines()
    except OSError:
        return False
    i = int(lineno)
    if i >= len(lines):
        return False
    nxt = lines[i].lstrip()
    return nxt.startswith(("//", "#"))

for raw in sys.stdin:
    raw = raw.rstrip("\n")
    parts = raw.split(":", 2)
    if len(parts) != 3:
        continue
    path, lineno, content = parts
    comment = comment_of(path, lineno, content)
    if comment is None:
        continue
    if not pattern.search(url.sub("", comment)):
        continue
    if mode == "dangling":
        if next_is_comment(path, lineno):
            continue
        print(f"{path}:{lineno}: dangling fragment (next line is not a comment continuation)")
    else:
        print(f"{path}:{lineno}:{content}")
PY

scan() {
    local label="$1" rg_pattern="$2" py_pattern="$3" flags="$4" mode="${5:-match}"
    local rg_flags=(-nH --no-heading)
    if [ "$flags" = "i" ]; then
        rg_flags+=(-i)
    fi
    local raw rg_status=0
    raw="$(rg "${rg_flags[@]}" "${GLOBS[@]}" "$rg_pattern" "${SCAN_ROOTS[@]}")" || rg_status=$?
    if [ "$rg_status" != "0" ] && [ "$rg_status" != "1" ]; then
        echo "FATAL: ripgrep failed (exit ${rg_status}) scanning ${label}" >&2
        exit 2
    fi
    if [ -n "${HYGIENE_DEBUG:-}" ]; then
        echo "DEBUG scan[${label}] roots=[${SCAN_ROOTS[*]}] rg_status=${rg_status} raw_lines=$(printf '%s' "$raw" | grep -c . || true)" >&2
    fi
    local hits filter_status=0
    hits="$(printf '%s' "$raw" | filter_comments "$py_pattern" "$flags" "$mode" 2>/tmp/hygiene_pyerr)" || filter_status=$?
    if [ "$filter_status" != "0" ]; then
        echo "FATAL: comment filter failed scanning ${label} (exit ${filter_status})" >&2
        cat /tmp/hygiene_pyerr >&2 || true
        exit 2
    fi
    report "$label" "$hits"
}

echo "==> comment hygiene: issue/PR refs in comments"
scan "issue/PR refs (#NNNN) in comments" \
    '#[0-9]{3,}' '#[0-9]{3,}(?![0-9a-fA-F])(?<![0-9a-fA-F]{7})' - match

echo "==> comment hygiene: tracking/see-issue phrasing"
scan "tracking/see-issue phrasing in comments" \
    '(tracking #|see #|see issue|see PR |fixes #|closes #|resolves #)' \
    '(tracking #|see #|see issue|see PR |fixes #|closes #|resolves #)' i match

echo "==> comment hygiene: gravitas phrases"
scan "gravitas phrases in comments" \
    '(sole source of truth|sibling PR|blast radius:|threat model:|rollback plan|canonical contract)' \
    '(sole source of truth|sibling PR|blast radius:|threat model:|rollback plan|canonical contract)' i match

echo "==> comment hygiene: review-process leakage"
scan "review-process leakage in comments" \
    '(NEW in this PR|previous revision of this PR|that started this PR|audit blocker|review pass|Round [0-9]+:)' \
    '(NEW in this PR|previous revision of this PR|that started this PR|audit blocker|review pass|Round [0-9]+:)' - match

echo "==> comment hygiene: dated notes"
scan "dated notes in comments" \
    '(as of 20[0-9]{2}-[0-9]{2}|last verified:)' \
    '(as of 20[0-9]{2}-[0-9]{2}|last verified:)' i match

echo "==> comment hygiene: ref-strip artifacts"
scan "RFC/section refs stripped mid-token (RFC-glued artifacts)" \
    'RFC(§|\s*\)|#?\s*$)' 'RFC(§|\s*\)|#?\s*$)' - match

scan "glued-word artifacts (line-join residue from comment deletion)" \
    '[a-z](without|exposes|therefore|because)\b' \
    '[a-z](?<!over)(?<!under)(?<!re)(without|exposes|therefore|because)\b' - match

echo "==> comment hygiene: truncation artifacts"
scan "dangling open-paren fragments in comments" \
    '\((see|issue|ref|tracking|regression)\s*$' \
    '\((see|issue|ref|tracking|regression)\s*$' i dangling

scan "dangling trailing-reference words in comments" \
    '(—|-|,)\s*(see|ref)\s*$' '(—|-|,)\s*(see|ref)\s*$' i dangling

scan "bare See/Ref/Tracking stub comments" \
    '(//|#)\s*(See|Ref|Tracking)\s*[.,;:]?\s*$' \
    '^(//+!?|#+)\s*(See|Ref|Tracking)\s*[.,;:]?\s*$' - match

scan "double-space lowercase stub comments (likely mid-sentence truncation)" \
    '^\s*//[/!]?  [a-z]' '^//[/!]?  (?!(?:since|itself)\b)[a-z]' - match

if [ "$fail" -ne 0 ]; then
    echo "Comment hygiene gate failed. Fix the comment or, if a fixture"
    echo "legitimately needs the pattern, add the path to SKIP_PATHS in"
    echo "scripts/ci/comment_hygiene_gate.sh (reviewed via this script's diff)."
    exit 1
fi

echo "Comment hygiene gate passed."
