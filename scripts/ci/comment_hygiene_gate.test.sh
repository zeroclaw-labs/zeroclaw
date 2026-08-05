#!/usr/bin/env bash

# Fixture tests for comment_hygiene_gate.sh: each planted violation must
# fire, each legal shape must pass.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
gate="${script_dir}/comment_hygiene_gate.sh"
scanner="${script_dir}/comment_hygiene_gate.py"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
git init -q .

pass_count=0
fail_count=0

expect_fail() {
    local name="$1" file="$2" label="$3"
    local out status
    set +e
    out="$(bash "$gate" "$file" 2>&1)"
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        echo "NOT CAUGHT: ${name}"
        fail_count=$((fail_count + 1))
    elif [ "$status" -ne 1 ]; then
        echo "WRONG STATUS: ${name} (wanted 1, got ${status})"
        fail_count=$((fail_count + 1))
    elif ! grep -qF "$label" <<<"$out"; then
        echo "WRONG DETECTOR: ${name} (wanted '${label}')"
        echo "$out" | grep -E '^FAIL|^FATAL|Traceback|Error' || true
        fail_count=$((fail_count + 1))
    else
        pass_count=$((pass_count + 1))
    fi
    rm -f "$file"
}

expect_pass() {
    local name="$1" file="$2"
    local out
    if ! out="$(bash "$gate" "$file" 2>&1)"; then
        echo "FALSE POSITIVE: ${name}"
        echo "$out" | grep -A2 '^FAIL' || true
        fail_count=$((fail_count + 1))
    else
        pass_count=$((pass_count + 1))
    fi
    rm -f "$file"
}

# ── violations must fire ────────────────────────────────────────────────

printf '// tracking #8519 for the upgrade\n' > a.rs
expect_fail "rust issue ref" a.rs "issue/PR refs"

printf '# See #1234 for details\n' > a.toml
expect_fail "top-level toml hash comment issue ref" a.toml "issue/PR refs"

printf '#!/bin/sh\n# fixes #999 upstream\n' > a.sh
expect_fail "shell comment tracking phrase" a.sh "tracking/see-issue"

printf 'x = 1  # closes #4321\n' > a.py
expect_fail "python trailing comment" a.py "tracking/see-issue"

printf '// NEW in this PR (req.123):\n' > a.rs
expect_fail "review-process leakage" a.rs "review-process"

printf '// New in this PR: mixed-case process note\n' > a.rs
expect_fail "mixed-case review-process leakage" a.rs "review-process"

printf '// round 2: follow-up process note\n' > a.rs
expect_fail "lowercase review round leakage" a.rs "review-process"

printf '// as of 2026-05 this is fine\n' > a.rs
expect_fail "dated note" a.rs "dated notes"

printf '// stamping is PR C (RFC§4).\n' > a.rs
expect_fail "RFC section artifact" a.rs "RFC/section"

printf '/// re-inflate from diskwithout re-sending megabytes\n' > a.rs
expect_fail "glued-word artifact" a.rs "glued-word"

printf '// deleted the old path (see\nfn f() {}\n' > a.rs
expect_fail "dangling open-paren fragment" a.rs "dangling open-paren"

printf '// the flag was removed — see\nfn f() {}\n' > a.rs
expect_fail "dangling trailing see" a.rs "dangling trailing-reference"

printf '// See\nfn f() {}\n' > a.rs
expect_fail "bare See stub" a.rs "bare See/Ref/Tracking"

printf '//  git -C must not be conflated\n' > a.rs
expect_fail "double-space lowercase stub" a.rs "double-space"

printf '/* tracking #8519 in a block comment */\n' > a.rs
expect_fail "rust block comment issue ref" a.rs "issue/PR refs"

printf '/*\n * tracking #8519 in a multiline block comment\n */\n' > a.rs
expect_fail "rust multiline block comment issue ref" a.rs "issue/PR refs"

printf 'let s = "ok"; // workaround, see #4321 for context\n' > a.rs
expect_fail "issue ref in comment beside a string literal" a.rs "tracking/see-issue"

printf "printf '%s\\n' '<<EOF'\n# tracking #8519 must still be scanned\n" > a.sh
expect_fail "quoted heredoc marker does not suppress later comments" a.sh "issue/PR refs"

printf 'cat <<<"payload"\n# tracking #8519 after a here-string\n' > a.sh
expect_fail "here-string does not start a heredoc" a.sh "issue/PR refs"

printf 'value=$((1 << BITS))\n# tracking #8519 after an arithmetic shift\n' > a.sh
expect_fail "arithmetic shift does not start a heredoc" a.sh "issue/PR refs"

printf '((value = 1 << BITS))\n# tracking #8519 after an arithmetic command\n' > a.sh
expect_fail "arithmetic command shift does not start a heredoc" a.sh "issue/PR refs"

printf 'cat <<END-JSON\n# tracking #8519 is heredoc content\nEND-JSON\n# tracking #8519 after the heredoc\n' > a.sh
expect_fail "hyphenated heredoc delimiter terminates exactly" a.sh "issue/PR refs"

printf "cat <<-EOF\n\tpayload\n\tEOF\n# tracking #8519 after a tab-stripping heredoc\n" > a.sh
expect_fail "tab-stripping heredoc terminates correctly" a.sh "issue/PR refs"

printf 'let text = "first line\nsecond line"; // tracking #8519 after the close\n' > a.rs
expect_fail "rust comment after multiline string close" a.rs "issue/PR refs"

printf "value = ''\n  payload\n''; # tracking #8519 inside Nix interpolation\nin value\n" > a.nix
expect_fail "nix interpolation comment is scanned" a.nix "issue/PR refs"

printf "value = ''\n  \${let\n    # tracking #8519 is a real interpolation comment\n    x = 1;\n  in x}\n'';\n" > a.nix
expect_fail "actual nix interpolation comment is scanned" a.nix "issue/PR refs"

printf 'note = """value"""" # tracking #8519 after a four-quote close\n' > a.toml
expect_fail "toml comment after multiline closing quote run" a.toml "issue/PR refs"

printf '// regression #123456 remains relevant\n' > a.rs
expect_fail "six-digit issue ref" a.rs "issue/PR refs"

printf '// color parser regression #1234 remains relevant\n' > a.rs
expect_fail "color-related prose does not hide an issue ref" a.rs "issue/PR refs"

# ── legal shapes must pass ──────────────────────────────────────────────

printf 'assert!(ok, "regression #6156 must stay fixed");\n' > b.rs
expect_pass "issue ref inside rust string literal" b.rs

printf '// boundary check\nlet m = format!("see #{n}");\n' > b.rs
expect_pass "string literal beside an unrelated comment" b.rs

printf '"""\n# tracking #8519 is example text, not a comment\n"""\n' > b.py
expect_pass "python triple-quoted issue-like text" b.py

printf 'note = """\n# tracking #8519 is multiline TOML text\n"""\n' > b.toml
expect_pass "toml multiline-string issue-like text" b.toml

printf "text = ''\n  # tracking #8519 is a Nix multiline string\n'';\n" > b.nix
expect_pass "nix multiline-string issue-like text" b.nix

printf "cat <<'EOF'\n# tracking #8519 is heredoc content\nEOF\n" > b.sh
expect_pass "shell heredoc issue-like text" b.sh

printf "cat <<EOF\n\tEOF\n# tracking #8519 remains heredoc content\nEOF\n" > b.sh
expect_pass "ordinary heredoc does not strip terminator tabs" b.sh

printf 'cat <<123\n# tracking #8519 is heredoc content\n123\n' > b.sh
expect_pass "numeric heredoc delimiter" b.sh

printf 'cat <<END/JSON\n# tracking #8519 is heredoc content\nEND/JSON\n' > b.sh
expect_pass "punctuated heredoc delimiter" b.sh

printf "cat <<E'OF'\n# tracking #8519 is heredoc content\nEOF\n" > b.sh
expect_pass "compositionally quoted heredoc delimiter" b.sh

printf 'cat <<E\\OF\n# tracking #8519 is heredoc content\nEOF\n' > b.sh
expect_pass "escaped heredoc delimiter" b.sh

printf 'cat <<"E\\OF"\n# tracking #8519 is heredoc content\nE\\OF\n' > b.sh
expect_pass "double-quoted heredoc preserves ordinary backslash" b.sh

printf "text='first line\n# tracking #8519 is multiline shell text\nlast line'\n" > b.sh
expect_pass "shell multiline single-quoted text" b.sh

printf 'text="first line\n# tracking #8519 is multiline shell text\nlast line"\n' > b.sh
expect_pass "shell multiline double-quoted text" b.sh

printf 'let text = "first line\n// tracking #8519 is multiline Rust text\nlast line";\n' > b.rs
expect_pass "rust multiline string issue-like text" b.rs

printf "text = ''\n  literal ''\${name}\n  # tracking #8519 is still Nix string text\n'';\n" > b.nix
expect_pass "nix escaped interpolation stays in indented string" b.nix

printf 'note = """\ntext \\""" is still multiline text\n# tracking #8519 is TOML string text\n"""\n' > b.toml
expect_pass "toml escaped triple quote stays in multiline string" b.toml

printf 'color = "#141413"  # hex color comment\n' > b.toml
expect_pass "hex color in string with trailing comment" b.toml

printf '# background: #141413\n' > b.toml
expect_pass "numeric hex color in comment" b.toml

printf '# color: #123\n# background: #1234\n# color: #123abc\n' > b.toml
expect_pass "short numeric and alphanumeric color tokens in comment" b.toml

printf '#!/usr/bin/env bash\necho ok\n' > b.sh
expect_pass "shebang only" b.sh

printf '// https://example.com/thing#5678 anchor url\n' > b.rs
expect_pass "url fragment in comment" b.rs

printf '// wraps the snowflake as <@1088...> for mentions\n' > b.rs
expect_pass "discord snowflake prose" b.rs

printf '// (Will error on missing files\n//  since /tmp/x does not exist.)\n' > b.rs
expect_pass "intentional double-space continuation under paren" b.rs

printf '/* legitimate block comment describing the shape */\n' > b.rs
expect_pass "clean rust block comment" b.rs

printf '// This module is the sole source of truth for widget ownership.\n// Threat model: untrusted peers cannot mutate it.\n// Rollback plan: restore the previous generated catalog.\n// This is the canonical contract for dispatch ordering.\n' > b.rs
expect_pass "stable contract vocabulary" b.rs

printf '// deleted the old path (see\n#[cfg(test)]\nfn f() {}\n' > a.rs
expect_fail "rust attribute is not a comment continuation" a.rs "dangling open-paren"

set +e
scanner_out="$(bash "$gate" '/nonexistent/dir/xyz' 2>&1)"
scanner_status=$?
set -e
if [ "$scanner_status" -eq 0 ]; then
    echo "SCANNER FAILURE NOT PROPAGATED (gate returned success on a bad path)"
    fail_count=$((fail_count + 1))
elif [ "$scanner_status" -ne 2 ]; then
    echo "SCANNER FAILURE USED WRONG STATUS (wanted 2, got ${scanner_status})"
    fail_count=$((fail_count + 1))
elif grep -qF 'FATAL' <<<"$scanner_out"; then
    pass_count=$((pass_count + 1))
else
    echo "SCANNER FAILED WITHOUT FATAL DIAGNOSTIC"
    echo "$scanner_out"
    fail_count=$((fail_count + 1))
fi

printf 'missing.rs\0' > scanner-inputs
set +e
parser_out="$(python3 "$scanner" scanner-inputs 2>&1)"
parser_status=$?
set -e
if [ "$parser_status" -eq 0 ]; then
    echo "PARSER FAILURE NOT PROPAGATED (scanner returned success on a missing source)"
    fail_count=$((fail_count + 1))
elif [ "$parser_status" -ne 2 ]; then
    echo "PARSER FAILURE USED WRONG STATUS (wanted 2, got ${parser_status})"
    fail_count=$((fail_count + 1))
elif grep -qF 'FATAL' <<<"$parser_out"; then
    pass_count=$((pass_count + 1))
else
    echo "PARSER FAILED WITHOUT FATAL DIAGNOSTIC"
    echo "$parser_out"
    fail_count=$((fail_count + 1))
fi
rm -f scanner-inputs

echo
echo "${pass_count} passed, ${fail_count} failed"
[ "$fail_count" -eq 0 ]
