#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/pr-verify.sh <pr-number> [repo]

Examples:
  scripts/pr-verify.sh 2293
  scripts/pr-verify.sh 2293 zeroclaw-labs/zeroclaw

Description:
  Verifies PR merge state using GitHub REST API (low-rate path) and
  confirms merge commit ancestry against local git refs when possible.
EOF
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 1
  fi
}

format_epoch() {
  local ts="${1:-}"
  if [[ -z "$ts" || "$ts" == "null" ]]; then
    echo "n/a"
    return
  fi

  if date -r "$ts" "+%Y-%m-%d %H:%M:%S %Z" >/dev/null 2>&1; then
    date -r "$ts" "+%Y-%m-%d %H:%M:%S %Z"
    return
  fi

  if date -d "@$ts" "+%Y-%m-%d %H:%M:%S %Z" >/dev/null 2>&1; then
    date -d "@$ts" "+%Y-%m-%d %H:%M:%S %Z"
    return
  fi

  echo "$ts"
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || $# -lt 1 ]]; then
  usage
  exit 0
fi

PR_NUMBER="$1"
REPO="${2:-zeroclaw-labs/zeroclaw}"
BASE_REMOTE="${BASE_REMOTE:-origin}"

require_cmd gh
require_cmd git

if ! [[ "$PR_NUMBER" =~ ^[0-9]+$ ]]; then
  echo "error: <pr-number> must be numeric (got: $PR_NUMBER)" >&2
  exit 1
fi

echo "== PR Snapshot (REST) =="
IFS=$'\t' read -r number title state merged merged_at merge_sha base_ref head_ref head_sha url < <(
  gh api "repos/$REPO/pulls/$PR_NUMBER" \
    --jq '[.number, .title, .state, (.merged|tostring), (.merged_at // ""), (.merge_commit_sha // ""), .base.ref, .head.ref, .head.sha, .html_url] | @tsv'
)

echo "repo:         $REPO"
echo "pr:           #$number"
echo "title:        $title"
echo "state:        $state"
echo "merged:       $merged"
echo "merged_at:    ${merged_at:-n/a}"
echo "base_ref:     $base_ref"
echo "head_ref:     $head_ref"
echo "head_sha:     $head_sha"
echo "merge_sha:    ${merge_sha:-n/a}"
echo "url:          $url"

echo
echo "== API Buckets =="
IFS=$'\t' read -r core_rem core_lim gql_rem gql_lim core_reset gql_reset < <(
  gh api rate_limit \
    --jq '[.resources.core.remaining, .resources.core.limit, .resources.graphql.remaining, .resources.graphql.limit, .resources.core.reset, .resources.graphql.reset] | @tsv'
)

echo "core:         $core_rem/$core_lim (reset: $(format_epoch "$core_reset"))"
echo "graphql:      $gql_rem/$gql_lim (reset: $(format_epoch "$gql_reset"))"

echo
echo "== Git Ancestry Check =="
if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "local_repo:   n/a (not inside a git worktree)"
  exit 0
fi

echo "local_repo:   $(git rev-parse --show-toplevel)"

if [[ "$merged" != "true" || -z "$merge_sha" ]]; then
  echo "result:       skipped (PR not merged or merge commit unavailable)"
  exit 0
fi

if ! git fetch "$BASE_REMOTE" "$base_ref" >/dev/null 2>&1; then
  echo "result:       unable to fetch $BASE_REMOTE/$base_ref (network/remote issue)"
  exit 0
fi

if ! git rev-parse --verify "$BASE_REMOTE/$base_ref" >/dev/null 2>&1; then
  echo "result:       unable to resolve $BASE_REMOTE/$base_ref"
  exit 0
fi

if git merge-base --is-ancestor "$merge_sha" "$BASE_REMOTE/$base_ref"; then
  echo "result:       PASS ($merge_sha is on $BASE_REMOTE/$base_ref)"
else
  echo "result:       FAIL ($merge_sha not found on $BASE_REMOTE/$base_ref)"
  exit 2
fi
