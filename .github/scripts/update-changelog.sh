#!/usr/bin/env bash
# update-changelog.sh — Generates a Keep a Changelog entry from git history
# and prepends it to CHANGELOG.md.
#
# Usage: ./update-changelog.sh <version> [previous-tag]
#   version:      Semver string, e.g. "0.5.0"
#   previous-tag: Optional. Auto-detected from latest stable tag if omitted.
#
# Environment:
#   RELEASE_DATE: Override the release date (default: today, YYYY-MM-DD)
#
# Requires: git with full history (fetch-depth: 0)

set -euo pipefail

VERSION="${1:?Usage: update-changelog.sh <version> [previous-tag]}"
TAG="v${VERSION}"
RELEASE_DATE="${RELEASE_DATE:-$(date -u +%Y-%m-%d)}"
CHANGELOG="CHANGELOG.md"

# ── Resolve previous stable tag ───────────────────────────────────────
if [ -n "${2:-}" ]; then
  PREV_TAG="$2"
else
  PREV_TAG=$(git tag --sort=-creatordate \
    | grep -vE '\-beta\.' \
    | grep -v "^${TAG}$" \
    | head -1 || echo "")
fi

if [ -z "$PREV_TAG" ]; then
  RANGE="HEAD"
  COMPARE_TEXT=""
else
  RANGE="${PREV_TAG}..HEAD"
  COMPARE_TEXT="**Full Changelog**: [\`${PREV_TAG}...${TAG}\`](https://github.com/zeroclaw-labs/zeroclaw/compare/${PREV_TAG}...${TAG})"
fi

echo "Generating changelog for ${TAG} (${RELEASE_DATE}), range: ${RANGE}"

# ── Extract commits by conventional-commit type ──────────────────────
extract_commits() {
  local prefix="$1"
  git log "$RANGE" --pretty=format:"%s" --no-merges \
    | grep -iE "^${prefix}(\\(|:)" \
    | sed -E "s/^${prefix}\(([^)]*)\): /\1: /" \
    | sed -E "s/^${prefix}: //" \
    | sed -E 's/ \(#[0-9]+\)$//' \
    | sort -uf \
    | while IFS= read -r line; do echo "- ${line}"; done || true
}

FEATURES=$(extract_commits "feat")
FIXES=$(extract_commits "fix")

# ── Build the entry ──────────────────────────────────────────────────
ENTRY="## [${VERSION}] - ${RELEASE_DATE}"

if [ -n "$FEATURES" ]; then
  ENTRY="${ENTRY}

### Added
${FEATURES}"
fi

if [ -n "$FIXES" ]; then
  ENTRY="${ENTRY}

### Fixed
${FIXES}"
fi

# If no features or fixes, add a generic line
if [ -z "$FEATURES" ] && [ -z "$FIXES" ]; then
  ENTRY="${ENTRY}

### Changed
- Incremental improvements and polish"
fi

if [ -n "$COMPARE_TEXT" ]; then
  ENTRY="${ENTRY}

${COMPARE_TEXT}"
fi

# ── Add release link reference ───────────────────────────────────────
LINK_REF="[${VERSION}]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/${TAG}"

# ── Prepend to CHANGELOG.md ──────────────────────────────────────────
if [ ! -f "$CHANGELOG" ]; then
  echo "Error: ${CHANGELOG} not found" >&2
  exit 1
fi

# Check if version already exists in changelog
if grep -qF "[${VERSION}]" "$CHANGELOG"; then
  echo "Version ${VERSION} already exists in ${CHANGELOG}, skipping."
  exit 0
fi

# Insert the new entry after the header block (after the "Note" blockquote)
# Strategy: find the first "## [" line and insert before it
if grep -q "^## \[" "$CHANGELOG"; then
  # Insert before the first version entry
  MARKER=$(grep -n "^## \[" "$CHANGELOG" | head -1 | cut -d: -f1)
  {
    head -n "$((MARKER - 1))" "$CHANGELOG"
    echo ""
    echo "$ENTRY"
    echo ""
    tail -n +"$MARKER" "$CHANGELOG"
  } > "${CHANGELOG}.tmp"
else
  # No existing entries — append after header
  {
    cat "$CHANGELOG"
    echo ""
    echo "$ENTRY"
    echo ""
  } > "${CHANGELOG}.tmp"
fi

mv "${CHANGELOG}.tmp" "$CHANGELOG"

# Append link reference if not already present
if ! grep -qF "$LINK_REF" "$CHANGELOG"; then
  echo "$LINK_REF" >> "$CHANGELOG"
fi

echo "Changelog updated for ${VERSION}"
