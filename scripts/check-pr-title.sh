#!/usr/bin/env bash
# Validate a PR title against Conventional Commits with required scope.
# Usage: check-pr-title.sh "<title>"
# Exits 0 on accept, 1 on reject.

set -u

title="${1-}"

pattern='^(build|chore|ci|docs|feat|fix|perf|refactor|revert|style|test)\([a-z0-9._/:-]+\)!?: .+'

if printf '%s' "$title" | grep -qE "$pattern"; then
  exit 0
fi

echo "::error::PR title must follow Conventional Commits with a scope: 'type(scope): description'"
echo "Allowed types: build, chore, ci, docs, feat, fix, perf, refactor, revert, style, test"
echo "Got: $title"
exit 1
