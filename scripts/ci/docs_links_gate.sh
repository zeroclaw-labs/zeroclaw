#!/usr/bin/env bash

set -euo pipefail

BASE_SHA="${BASE_SHA:-}"
DOCS_FILES_RAW="${DOCS_FILES:-}"

LINKS_FILE="$(mktemp)"
trap 'rm -f "$LINKS_FILE"' EXIT

python3 ./scripts/ci/collect_changed_links.py \
    --base "$BASE_SHA" \
    --docs-files "$DOCS_FILES_RAW" \
    --output "$LINKS_FILE"

if [ ! -s "$LINKS_FILE" ]; then
    echo "No added links detected in changed docs lines."
    exit 0
fi

if ! command -v lychee >/dev/null 2>&1; then
    echo "lychee is required to run docs link gate locally."
    echo "Install via: cargo install lychee"
    exit 1
fi

echo "Checking added links with lychee (offline mode)..."
lychee --offline --no-progress --format detailed "$LINKS_FILE"
