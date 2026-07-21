#!/usr/bin/env bash
# Regression tests for detached and stale docs translation submodule states.

set -euo pipefail

SCRIPT_SOURCE="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/refresh-translations.sh}"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

git_env=(
  GIT_AUTHOR_NAME=Test
  GIT_AUTHOR_EMAIL=test@example.com
  GIT_COMMITTER_NAME=Test
  GIT_COMMITTER_EMAIL=test@example.com
  GIT_ALLOW_PROTOCOL=file
)

setup_fixture() {
  local name="$1"
  local root="$TMP_ROOT/$name"
  local remote="$root/translations.git"
  local seed="$root/seed"
  local repo="$root/zeroclaw"

  mkdir -p "$root"
  git init --quiet --bare "$remote"
  git clone --quiet "$remote" "$seed"
  git -C "$seed" switch --quiet --create main
  printf 'base\n' > "$seed/es.po"
  env "${git_env[@]}" git -C "$seed" add es.po
  env "${git_env[@]}" git -C "$seed" commit --quiet -m base
  git -C "$seed" push --quiet --set-upstream origin main
  git --git-dir="$remote" symbolic-ref HEAD refs/heads/main

  git init --quiet "$repo"
  mkdir -p "$repo/scripts/release" "$repo/docs/book"
  cp "$SCRIPT_SOURCE" "$repo/scripts/release/refresh-translations.sh"
  chmod +x "$repo/scripts/release/refresh-translations.sh"
  printf '[workspace.package]\nversion = "9.9.9"\n' > "$repo/Cargo.toml"
  git -C "$repo" -c protocol.file.allow=always submodule add --quiet -b main \
    "$remote" docs/book/po
  env "${git_env[@]}" git -C "$repo" add .
  env "${git_env[@]}" git -C "$repo" commit --quiet -m fixture

  printf '%s\n' "$root"
}

assert_equal() {
  local expected="$1"
  local actual="$2"
  local message="$3"
  if [[ "$expected" != "$actual" ]]; then
    echo "FAIL: $message" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    exit 1
  fi
}

success_root="$(setup_fixture success)"
success_repo="$success_root/zeroclaw"
success_remote="$success_root/translations.git"
base_commit="$(git -C "$success_repo/docs/book/po" rev-parse HEAD)"
git -C "$success_repo/docs/book/po" checkout --quiet --detach "$base_commit"
printf 'translated\n' >> "$success_repo/docs/book/po/es.po"

env "${git_env[@]}" "$success_repo/scripts/release/refresh-translations.sh" \
  9.9.9 --no-translate >/dev/null

remote_main="$(git --git-dir="$success_remote" rev-parse refs/heads/main)"
remote_tag="$(git --git-dir="$success_remote" rev-parse refs/tags/v9.9.9)"
pinned_head="$(git -C "$success_repo/docs/book/po" rev-parse HEAD)"
if [[ "$remote_main" == "$base_commit" ]]; then
  echo "FAIL: detached catalogue commit did not advance remote main" >&2
  exit 1
fi
assert_equal "$remote_main" "$remote_tag" "tag should point at refreshed remote main"
assert_equal "$remote_main" "$pinned_head" "main repo should pin the refreshed tag"

stale_root="$(setup_fixture stale)"
stale_repo="$stale_root/zeroclaw"
stale_seed="$stale_root/seed"
stale_remote="$stale_root/translations.git"
stale_base="$(git -C "$stale_repo/docs/book/po" rev-parse HEAD)"
printf 'remote update\n' >> "$stale_seed/es.po"
env "${git_env[@]}" git -C "$stale_seed" add es.po
env "${git_env[@]}" git -C "$stale_seed" commit --quiet -m remote-update
git -C "$stale_seed" push --quiet
git -C "$stale_repo/docs/book/po" checkout --quiet --detach "$stale_base"
printf 'local translation\n' >> "$stale_repo/docs/book/po/es.po"

if env "${git_env[@]}" "$stale_repo/scripts/release/refresh-translations.sh" \
  9.9.8 --no-translate >"$stale_root/output.log" 2>&1; then
  echo "FAIL: stale prepared catalogues should be rejected" >&2
  exit 1
fi
grep -q "prepared catalogues are not based on current origin/main" \
  "$stale_root/output.log"
if git --git-dir="$stale_remote" rev-parse --verify refs/tags/v9.9.8 >/dev/null 2>&1; then
  echo "FAIL: rejected stale catalogues must not create a tag" >&2
  exit 1
fi

echo "refresh-translations detached-head tests: pass"
