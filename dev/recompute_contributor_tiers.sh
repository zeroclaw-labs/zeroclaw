#!/usr/bin/env bash

set -euo pipefail

SCRIPT_NAME="$(basename "$0")"

usage() {
  cat <<USAGE
Recompute contributor tier labels for historical PRs/issues.

Usage:
  ./$SCRIPT_NAME [options]

Options:
  --repo <owner/repo>     Target repository (default: current gh repo)
  --kind <both|prs|issues>
                          Target objects (default: both)
  --state <all|open|closed>
                          State filter for listing objects (default: all)
  --limit <N>             Limit processed objects after fetch (default: 0 = no limit)
  --apply                 Apply label updates (default is dry-run)
  --dry-run               Preview only (default)
  -h, --help              Show this help

Examples:
  ./$SCRIPT_NAME --repo zeroclaw-labs/zeroclaw --limit 50
  ./$SCRIPT_NAME --repo zeroclaw-labs/zeroclaw --kind prs --state open --apply
USAGE
}

die() {
  echo "[$SCRIPT_NAME] ERROR: $*" >&2
  exit 1
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    die "Required command not found: $1"
  fi
}

urlencode() {
  jq -nr --arg value "$1" '$value|@uri'
}

select_contributor_tier() {
  local merged_count="$1"
  if (( merged_count >= 50 )); then
    echo "distinguished contributor"
  elif (( merged_count >= 20 )); then
    echo "principal contributor"
  elif (( merged_count >= 10 )); then
    echo "experienced contributor"
  elif (( merged_count >= 5 )); then
    echo "trusted contributor"
  else
    echo ""
  fi
}

DRY_RUN=1
KIND="both"
STATE="all"
LIMIT=0
REPO=""

while (($# > 0)); do
  case "$1" in
    --repo)
      [[ $# -ge 2 ]] || die "Missing value for --repo"
      REPO="$2"
      shift 2
      ;;
    --kind)
      [[ $# -ge 2 ]] || die "Missing value for --kind"
      KIND="$2"
      shift 2
      ;;
    --state)
      [[ $# -ge 2 ]] || die "Missing value for --state"
      STATE="$2"
      shift 2
      ;;
    --limit)
      [[ $# -ge 2 ]] || die "Missing value for --limit"
      LIMIT="$2"
      shift 2
      ;;
    --apply)
      DRY_RUN=0
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "Unknown option: $1"
      ;;
  esac
done

case "$KIND" in
  both|prs|issues) ;;
  *) die "--kind must be one of: both, prs, issues" ;;
esac

case "$STATE" in
  all|open|closed) ;;
  *) die "--state must be one of: all, open, closed" ;;
esac

if ! [[ "$LIMIT" =~ ^[0-9]+$ ]]; then
  die "--limit must be a non-negative integer"
fi

require_cmd gh
require_cmd jq

if ! gh auth status >/dev/null 2>&1; then
  die "gh CLI is not authenticated. Run: gh auth login"
fi

if [[ -z "$REPO" ]]; then
  REPO="$(gh repo view --json nameWithOwner --jq '.nameWithOwner' 2>/dev/null || true)"
  [[ -n "$REPO" ]] || die "Unable to infer repo. Pass --repo <owner/repo>."
fi

echo "[$SCRIPT_NAME] Repo: $REPO"
echo "[$SCRIPT_NAME] Mode: $([[ "$DRY_RUN" -eq 1 ]] && echo "dry-run" || echo "apply")"
echo "[$SCRIPT_NAME] Kind: $KIND | State: $STATE | Limit: $LIMIT"

TIERS_JSON='["trusted contributor","experienced contributor","principal contributor","distinguished contributor"]'

TMP_FILES=()
cleanup() {
  if ((${#TMP_FILES[@]} > 0)); then
    rm -f "${TMP_FILES[@]}"
  fi
}
trap cleanup EXIT

new_tmp_file() {
  local tmp
  tmp="$(mktemp)"
  TMP_FILES+=("$tmp")
  echo "$tmp"
}

targets_file="$(new_tmp_file)"

if [[ "$KIND" == "both" || "$KIND" == "prs" ]]; then
  gh api --paginate "repos/$REPO/pulls?state=$STATE&per_page=100" \
    --jq '.[] | {
      kind: "pr",
      number: .number,
      author: (.user.login // ""),
      author_type: (.user.type // ""),
      labels: [(.labels[]?.name // empty)]
    }' >> "$targets_file"
fi

if [[ "$KIND" == "both" || "$KIND" == "issues" ]]; then
  gh api --paginate "repos/$REPO/issues?state=$STATE&per_page=100" \
    --jq '.[] | select(.pull_request | not) | {
      kind: "issue",
      number: .number,
      author: (.user.login // ""),
      author_type: (.user.type // ""),
      labels: [(.labels[]?.name // empty)]
    }' >> "$targets_file"
fi

if [[ "$LIMIT" -gt 0 ]]; then
  limited_file="$(new_tmp_file)"
  head -n "$LIMIT" "$targets_file" > "$limited_file"
  mv "$limited_file" "$targets_file"
fi

target_count="$(wc -l < "$targets_file" | tr -d ' ')"
if [[ "$target_count" -eq 0 ]]; then
  echo "[$SCRIPT_NAME] No targets found."
  exit 0
fi

echo "[$SCRIPT_NAME] Targets fetched: $target_count"

# Ensure tier labels exist (trusted contributor might be new).
label_color=""
for probe_label in "experienced contributor" "principal contributor" "distinguished contributor" "trusted contributor"; do
  encoded_label="$(urlencode "$probe_label")"
  if color_candidate="$(gh api "repos/$REPO/labels/$encoded_label" --jq '.color' 2>/dev/null || true)"; then
    if [[ -n "$color_candidate" ]]; then
      label_color="$(echo "$color_candidate" | tr '[:lower:]' '[:upper:]')"
      break
    fi
  fi
done
[[ -n "$label_color" ]] || label_color="C5D7A2"

while IFS= read -r tier_label; do
  [[ -n "$tier_label" ]] || continue
  encoded_label="$(urlencode "$tier_label")"
  if gh api "repos/$REPO/labels/$encoded_label" >/dev/null 2>&1; then
    continue
  fi

  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[dry-run] Would create missing label: $tier_label (color=$label_color)"
  else
    gh api -X POST "repos/$REPO/labels" \
      -f name="$tier_label" \
      -f color="$label_color" >/dev/null
    echo "[apply] Created missing label: $tier_label"
  fi
done < <(jq -r '.[]' <<<"$TIERS_JSON")

# Build merged PR count cache by unique human authors.
authors_file="$(new_tmp_file)"
jq -r 'select(.author != "" and .author_type != "Bot") | .author' "$targets_file" | sort -u > "$authors_file"
author_count="$(wc -l < "$authors_file" | tr -d ' ')"
echo "[$SCRIPT_NAME] Unique human authors: $author_count"

author_counts_file="$(new_tmp_file)"
while IFS= read -r author; do
  [[ -n "$author" ]] || continue
  query="repo:$REPO is:pr is:merged author:$author"
  merged_count="$(gh api search/issues -f q="$query" -F per_page=1 --jq '.total_count' 2>/dev/null || true)"
  if ! [[ "$merged_count" =~ ^[0-9]+$ ]]; then
    merged_count=0
  fi
  printf '%s\t%s\n' "$author" "$merged_count" >> "$author_counts_file"
done < "$authors_file"

updated=0
unchanged=0
skipped=0
failed=0

while IFS= read -r target_json; do
  [[ -n "$target_json" ]] || continue

  number="$(jq -r '.number' <<<"$target_json")"
  kind="$(jq -r '.kind' <<<"$target_json")"
  author="$(jq -r '.author' <<<"$target_json")"
  author_type="$(jq -r '.author_type' <<<"$target_json")"
  current_labels_json="$(jq -c '.labels // []' <<<"$target_json")"

  if [[ -z "$author" || "$author_type" == "Bot" ]]; then
    skipped=$((skipped + 1))
    continue
  fi

  merged_count="$(awk -F '\t' -v key="$author" '$1 == key { print $2; exit }' "$author_counts_file")"
  if ! [[ "$merged_count" =~ ^[0-9]+$ ]]; then
    merged_count=0
  fi
  desired_tier="$(select_contributor_tier "$merged_count")"

  if ! current_tier="$(jq -r --argjson tiers "$TIERS_JSON" '[.[] | select(. as $label | ($tiers | index($label)) != null)][0] // ""' <<<"$current_labels_json" 2>/dev/null)"; then
    echo "[warn] Skipping ${kind} #${number}: cannot parse current labels JSON" >&2
    failed=$((failed + 1))
    continue
  fi

  if ! next_labels_json="$(jq -c --arg desired "$desired_tier" --argjson tiers "$TIERS_JSON" '
    (. // [])
    | map(select(. as $label | ($tiers | index($label)) == null))
    | if $desired != "" then . + [$desired] else . end
    | unique
  ' <<<"$current_labels_json" 2>/dev/null)"; then
    echo "[warn] Skipping ${kind} #${number}: cannot compute next labels" >&2
    failed=$((failed + 1))
    continue
  fi

  if ! normalized_current="$(jq -c 'unique | sort' <<<"$current_labels_json" 2>/dev/null)"; then
    echo "[warn] Skipping ${kind} #${number}: cannot normalize current labels" >&2
    failed=$((failed + 1))
    continue
  fi

  if ! normalized_next="$(jq -c 'unique | sort' <<<"$next_labels_json" 2>/dev/null)"; then
    echo "[warn] Skipping ${kind} #${number}: cannot normalize next labels" >&2
    failed=$((failed + 1))
    continue
  fi

  if [[ "$normalized_current" == "$normalized_next" ]]; then
    unchanged=$((unchanged + 1))
    continue
  fi

  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[dry-run] ${kind} #${number} @${author} merged=${merged_count} tier: '${current_tier:-none}' -> '${desired_tier:-none}'"
    updated=$((updated + 1))
    continue
  fi

  payload="$(jq -cn --argjson labels "$next_labels_json" '{labels: $labels}')"
  if gh api -X PUT "repos/$REPO/issues/$number/labels" --input - <<<"$payload" >/dev/null; then
    echo "[apply] Updated ${kind} #${number} @${author} tier: '${current_tier:-none}' -> '${desired_tier:-none}'"
    updated=$((updated + 1))
  else
    echo "[apply] FAILED ${kind} #${number}" >&2
    failed=$((failed + 1))
  fi
done < "$targets_file"

echo ""
echo "[$SCRIPT_NAME] Summary"
echo "  Targets:   $target_count"
echo "  Updated:   $updated"
echo "  Unchanged: $unchanged"
echo "  Skipped:   $skipped"
echo "  Failed:    $failed"

if [[ "$failed" -gt 0 ]]; then
  exit 1
fi
