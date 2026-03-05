#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

echo "Checking repository for deprecated Linear product references..."

# Keep this guard narrow to explicit Linear product identifiers only.
PATTERNS=(
  "LINEAR_API_KEY"
  "api\\.linear\\.app"
  "linear\\.app"
  "name:[[:space:]]*\"Linear\""
  "Linear issue key"
  "Linear issue URL"
  "Linear execution issue"
)

RG_ARGS=(
  --line-number
  --with-filename
  --hidden
  --glob
  "!.git"
  --glob
  "!target"
  --glob
  "!scripts/ci/check_no_linear_product_refs.sh"
)

matches_file="$(mktemp)"
trap 'rm -f "$matches_file"' EXIT

found=0
for pattern in "${PATTERNS[@]}"; do
  if rg "${RG_ARGS[@]}" -e "$pattern" . >>"$matches_file"; then
    found=1
  fi
done

if [[ "$found" -ne 0 ]]; then
  echo "Deprecated Linear product references found:"
  sort -u "$matches_file"
  echo
  echo "Remove these references or migrate to GitHub-native tracking."
  exit 1
fi

echo "No deprecated Linear product references detected."
