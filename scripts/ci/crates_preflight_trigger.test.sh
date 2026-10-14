#!/usr/bin/env bash
set -euo pipefail
script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/crates_preflight_trigger.sh"
repo="$(mktemp -d)"
trap 'rm -rf "$repo"' EXIT
cd "$repo"

git init -q -b main
git config user.name fixture
git config user.email fixture@example.invalid
git config commit.gpgsign false

commit() {
  printf '[workspace.package]\nversion = "%s"\nrust-version = "1.98"\n%s' "$1" "${2:-}" >Cargo.toml
  git add Cargo.toml
  git commit -q --allow-empty -m "$1 ${2:-}"
  git rev-parse HEAD
}

expect() {
  local expected="$1" event="$2" base="${3:-}" actual
  actual="$(bash "$script" "$event" "$base" 2>/dev/null)"
  if [[ "$actual" != "$expected" ]]; then
    echo "unexpected trigger for $event from ${base:-<none>}:" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    exit 1
  fi
}

base="$(commit 1.2.3)"
commit 1.2.3 "# unrelated edit" >/dev/null
expect "run=false" pull_request "$base"

commit 1.2.4 >/dev/null
expect $'run=true\ntag=v1.2.4' pull_request "$base"
expect $'run=true\ntag=v1.2.4' merge_group "$base"

# Master pushes and other events never run it; the release run verifies again.
expect "run=false" push "$base"
expect "run=false" workflow_dispatch "$base"

# Only stable versions reach crates.io.
commit 1.3.0-rc.1 >/dev/null
expect "run=false" pull_request "$base"

# A dependency's version line later in the file is not the workspace version.
commit 1.2.3 $'[dependencies]\nversion = "9.9.9"\n' >/dev/null
expect "run=false" pull_request "$base"

# A missing base must fail loudly rather than silently skip a blocking check.
if bash "$script" pull_request 0000000000000000000000000000000000000000 >/dev/null 2>&1; then
  echo "an unavailable base commit must fail the trigger" >&2
  exit 1
fi
if bash "$script" pull_request >/dev/null 2>&1; then
  echo "a missing base commit must fail the trigger" >&2
  exit 1
fi

echo "crates_preflight_trigger: ok"
