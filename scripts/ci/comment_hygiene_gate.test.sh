#!/usr/bin/env bash

# Fixture tests for comment_hygiene_gate.sh: each planted violation must
# fire, each legal shape must pass.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
gate="${script_dir}/comment_hygiene_gate.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
git init -q .

pass_count=0
fail_count=0

expect_fail() {
    local name="$1" file="$2" label="$3"
    local out
    if out="$(bash "$gate" "$file" 2>&1)"; then
        echo "NOT CAUGHT: ${name}"
        fail_count=$((fail_count + 1))
    elif ! grep -qF "$label" <<<"$out"; then
        echo "WRONG DETECTOR: ${name} (wanted '${label}')"
        echo "$out" | grep '^FAIL' || true
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

printf '// this module is the sole source of truth for widgets\n' > a.rs
expect_fail "gravitas phrase" a.rs "gravitas"

printf '// NEW in this PR (req.123):\n' > a.rs
expect_fail "review-process leakage" a.rs "review-process"

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

printf 'let s = "ok"; // workaround, see #4321 for context\n' > a.rs
expect_fail "issue ref in comment beside a string literal" a.rs "tracking/see-issue"

# ── legal shapes must pass ──────────────────────────────────────────────

printf 'assert!(ok, "regression #6156 must stay fixed");\n' > b.rs
expect_pass "issue ref inside rust string literal" b.rs

printf '// boundary check\nlet m = format!("see #{n}");\n' > b.rs
expect_pass "string literal beside an unrelated comment" b.rs

printf 'color = "#141413"  # hex color comment\n' > b.toml
expect_pass "hex color in string with trailing comment" b.toml

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

if scanner_out="$(bash "$gate" '/nonexistent/dir/xyz' 2>&1)"; then
    echo "SCANNER FAILURE NOT PROPAGATED (gate returned success on a bad path)"
    fail_count=$((fail_count + 1))
elif grep -qF 'FATAL' <<<"$scanner_out"; then
    pass_count=$((pass_count + 1))
else
    echo "SCANNER FAILED WITHOUT FATAL DIAGNOSTIC"
    echo "$scanner_out"
    fail_count=$((fail_count + 1))
fi

echo
echo "${pass_count} passed, ${fail_count} failed"
[ "$fail_count" -eq 0 ]
