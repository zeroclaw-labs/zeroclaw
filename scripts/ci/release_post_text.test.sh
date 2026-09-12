#!/usr/bin/env bash
# Tests for scripts/ci/release_post_text.sh, the release announcement composer.
# No network: the feed lookup is exercised only through explicit links.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
composer="${root_dir}/scripts/ci/release_post_text.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_equal() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" != "$actual" ]; then
    echo "FAIL: $name" >&2
    echo "--- expected ---" >&2
    printf '%s\n' "$expected" >&2
    echo "--- actual ---" >&2
    printf '%s\n' "$actual" >&2
    exit 1
  fi
}

# Release notes in the shape the changelog skill writes.
cat > "$work/notes.md" <<'EOF'
# ZeroClaw v9.9.9

ZeroClaw v9.9.9 is a test release spanning **12 commits** from **3 contributors**.

## In brief

Relay + Router, together — a blind relay with native mTLS, and a hosted model router.

Plus multi-chat with image upload, *live thinking*, and hardened [plugin boundaries](https://docs.example/plugins) (#123, #456).

## Highlights

- **Relay arrives:** blind forwarding with `mTLS` enrollment (#10142, GHSA-93f6-34w8-5g98).
- **Chat grows up:** several conversations per agent (#9353).
- **Third thing:** not needed by these tests.

## What's New

- Irrelevant to the announcement.
EOF

# 1. In brief, counts from the preamble, explicit link. Markdown stripped.
expected="$(printf '%s' 'Relay + Router, together — a blind relay with native mTLS, and a hosted model router.

Plus multi-chat with image upload, live thinking, and hardened plugin boundaries.

12 commits. 3 contributors.

https://www.example.com/blog/v999')"
actual="$(bash "$composer" --notes "$work/notes.md" --tag v9.9.9 --link https://www.example.com/blog/v999)"
assert_equal "in brief with counts and link" "$expected" "$actual"

# 2. Discord shape: Highlights bullets follow the In brief text, bold labels
#    kept as text, code and PR/advisory references removed.
actual="$(bash "$composer" --notes "$work/notes.md" --tag v9.9.9 --highlights 2 --limit 3500 --fallback-link https://github.example/release)"
grep -Fxq '• Relay arrives: blind forwarding with mTLS enrollment.' <<<"$actual" || fail "highlight bullet stripped of code and references"
grep -Fxq '• Chat grows up: several conversations per agent.' <<<"$actual" || fail "second highlight bullet"
grep -Fq 'Third thing' <<<"$actual" && fail "--highlights 2 must stop at two bullets"
grep -Fxq 'https://github.example/release' <<<"$actual" || fail "fallback link appended when no explicit link"

# 3. Without In brief, the first two Highlights bullets stand in.
sed '/^## In brief$/,/^## Highlights$/{/^## Highlights$/!d;}' "$work/notes.md" > "$work/no-brief.md"
actual="$(bash "$composer" --notes "$work/no-brief.md" --tag v9.9.9)"
expected="$(printf '%s' 'Relay arrives: blind forwarding with mTLS enrollment.

Chat grows up: several conversations per agent.

12 commits. 3 contributors.')"
assert_equal "highlights fallback" "$expected" "$actual"

# 4. Neither section: exit 2 so the caller can fall back to its own text.
printf '# Nothing here\n\nJust a preamble.\n' > "$work/empty.md"
if bash "$composer" --notes "$work/empty.md" >/dev/null 2>&1; then
  fail "expected a non-zero exit without In brief or Highlights"
fi
status=0
bash "$composer" --notes "$work/empty.md" >/dev/null 2>&1 || status=$?
[ "$status" -eq 2 ] || fail "expected exit 2, got $status"

# 5. Over the limit: cut at a word boundary, ellipsis appended, paragraph
#    breaks preserved, warning on stderr, link still intact.
long_body="$(printf 'first paragraph %s\n\nsecond paragraph %s' \
  "$(printf 'w%.0s ' $(seq 1 30) | sed 's/ $//')" "$(printf 'x%.0s ' $(seq 1 120) | sed 's/ $//')")"
printf '## In brief\n\n%s\n' "$long_body" > "$work/long.md"
actual="$(bash "$composer" --notes "$work/long.md" --link https://www.example.com/p 2>"$work/stderr")"
grep -Fq 'truncating at a word boundary' "$work/stderr" || fail "truncation must warn"
body="$(printf '%s' "$actual" | sed '$d' | sed '$d')"   # drop blank line and link
[ "${#body}" -le 255 ] || fail "truncated body is ${#body} characters, over 255"
case "$body" in *…) ;; *) fail "truncated body must end with an ellipsis" ;; esac
grep -Fq 'second paragraph' <<<"$body" || fail "truncation must keep the paragraph break"
prefix="${body%…}"
[ "${long_body:0:${#prefix}}" = "$prefix" ] || fail "truncated text must be a prefix of the original"
case "${long_body:${#prefix}:1}" in ' '|$'\n') ;; *) fail "truncation must end at a word boundary" ;; esac
grep -Fxq 'https://www.example.com/p' <<<"$actual" || fail "link must survive truncation"

# 6. Characters, not bytes: an em dash and emoji count as one each, so a
#    255-character body with multibyte characters is not truncated.
{
  echo '## In brief'
  echo
  python3 -c "print('—🦀' + 'a' * 253)"
} > "$work/multibyte.md"
actual="$(bash "$composer" --notes "$work/multibyte.md" --link https://www.example.com/p 2>"$work/stderr")"
[ ! -s "$work/stderr" ] || fail "255 characters must not trigger truncation (byte counting?)"

# 7. No counts in the preamble: computed from git for the tag range,
#    excluding bot authors.
repo="$work/repo"
git init -q "$repo"
# Independent of the machine's signing configuration.
g() { git -C "$repo" -c commit.gpgsign=false -c tag.gpgsign=false -c user.name="$1" -c user.email="$2" "${@:3}"; }
g Alice alice@example.com commit -q --allow-empty -m "one"
g Alice alice@example.com tag v1.0.0
g Alice alice@example.com commit -q --allow-empty -m "two"
g Bob bob@example.com commit -q --allow-empty -m "three"
g "dependabot[bot]" bot@example.com commit -q --allow-empty -m "four"
g Alice alice@example.com tag v1.1.0
printf '## In brief\n\nShort.\n' > "$work/nocounts.md"
actual="$(cd "$repo" && bash "$composer" --notes "$work/nocounts.md" --tag v1.1.0 --prev v1.0.0)"
expected="$(printf 'Short.\n\n3 commits. 2 contributors.')"
assert_equal "git counts excluding bots" "$expected" "$actual"

echo "release_post_text.test.sh: all checks passed"
