#!/usr/bin/env bash
# Detect change scope for CI pipeline.
# Classifies changed files into docs-only, rust, workflow categories
# and writes results to $GITHUB_OUTPUT.
#
# Required environment variables:
#   GITHUB_OUTPUT   — GitHub Actions output file
#   EVENT_NAME      — github.event_name (push or pull_request)
#   BASE_SHA        — base commit SHA to diff against
set -euo pipefail

write_empty_docs_files() {
  {
    echo "docs_files<<EOF"
    echo "EOF"
  } >> "$GITHUB_OUTPUT"
}

BASE="$BASE_SHA"

if [ -z "$BASE" ] || ! git cat-file -e "$BASE^{commit}" 2>/dev/null; then
  {
    echo "docs_only=false"
    echo "docs_changed=false"
    echo "rust_changed=true"
    echo "workflow_changed=false"
    echo "base_sha="
  } >> "$GITHUB_OUTPUT"
  write_empty_docs_files
  exit 0
fi

# Use merge-base to avoid false positives when the base branch has advanced
# and the PR branch is temporarily behind. This limits scope to changes
# introduced by the head branch itself.
DIFF_BASE="$BASE"
if MERGE_BASE="$(git merge-base "$BASE" HEAD 2>/dev/null)"; then
  if [ -n "$MERGE_BASE" ]; then
    DIFF_BASE="$MERGE_BASE"
  fi
fi

CHANGED="$(git diff --name-only "$DIFF_BASE" HEAD || true)"
if [ -z "$CHANGED" ]; then
  {
    echo "docs_only=false"
    echo "docs_changed=false"
    echo "rust_changed=false"
    echo "workflow_changed=false"
    echo "base_sha=$DIFF_BASE"
  } >> "$GITHUB_OUTPUT"
  write_empty_docs_files
  exit 0
fi

docs_only=true
docs_changed=false
rust_changed=false
workflow_changed=false
docs_files=()
while IFS= read -r file; do
  [ -z "$file" ] && continue

  if [[ "$file" == .github/workflows/* ]]; then
    workflow_changed=true
  fi

  if [[ "$file" == docs/* ]] \
    || [[ "$file" == *.md ]] \
    || [[ "$file" == *.mdx ]] \
    || [[ "$file" == "LICENSE" ]] \
    || [[ "$file" == ".markdownlint-cli2.yaml" ]] \
    || [[ "$file" == .github/ISSUE_TEMPLATE/* ]] \
    || [[ "$file" == .github/pull_request_template.md ]]; then
    if [[ "$file" == *.md ]] \
      || [[ "$file" == *.mdx ]] \
      || [[ "$file" == "LICENSE" ]] \
      || [[ "$file" == .github/pull_request_template.md ]]; then
      docs_changed=true
      docs_files+=("$file")
    fi
    continue
  fi

  docs_only=false

  if [[ "$file" == src/* ]] \
    || [[ "$file" == tests/* ]] \
    || [[ "$file" == "Cargo.toml" ]] \
    || [[ "$file" == "Cargo.lock" ]] \
    || [[ "$file" == "deny.toml" ]]; then
    rust_changed=true
  fi
done <<< "$CHANGED"

{
  echo "docs_only=$docs_only"
  echo "docs_changed=$docs_changed"
  echo "rust_changed=$rust_changed"
  echo "workflow_changed=$workflow_changed"
  echo "base_sha=$DIFF_BASE"
  echo "docs_files<<EOF"
  printf '%s\n' "${docs_files[@]}"
  echo "EOF"
} >> "$GITHUB_OUTPUT"
