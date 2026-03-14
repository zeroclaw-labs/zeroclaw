#!/usr/bin/env bash
# sync-readme.sh — Auto-update "What's New" and "Recent Contributors" in all READMEs
# Called by the sync-readme GitHub Actions workflow on each release.
set -euo pipefail

# --- Resolve version and ranges ---

LATEST_TAG=$(git tag --sort=-creatordate | head -1 || echo "")
if [ -z "$LATEST_TAG" ]; then
  echo "No tags found — skipping README sync"
  exit 0
fi

VERSION="${LATEST_TAG#v}"

# Find previous stable tag for contributor range
PREV_STABLE=$(git tag --sort=-creatordate \
  | grep -v "^${LATEST_TAG}$" \
  | grep -vE '\-beta\.' \
  | head -1 || echo "")

FEAT_RANGE="${PREV_STABLE:+${PREV_STABLE}..}${LATEST_TAG}"
CONTRIB_RANGE="${PREV_STABLE:+${PREV_STABLE}..}${LATEST_TAG}"

# --- Build "What's New" table rows ---

FEATURES=$(git log "$FEAT_RANGE" --pretty=format:"%s" --no-merges \
  | grep -iE '^feat(\(|:)' \
  | sed 's/^feat(\([^)]*\)): /| \1 | /' \
  | sed 's/^feat: /| General | /' \
  | sed 's/ (#[0-9]*)$//' \
  | sort -uf \
  | while IFS= read -r line; do echo "${line} |"; done || true)

if [ -z "$FEATURES" ]; then
  FEATURES="| General | Incremental improvements and polish |"
fi

MONTH_YEAR=$(date -u +"%B %Y")

# --- Build contributor list ---

GIT_AUTHORS=$(git log "$CONTRIB_RANGE" --pretty=format:"%an" --no-merges | sort -uf || true)
CO_AUTHORS=$(git log "$CONTRIB_RANGE" --pretty=format:"%b" --no-merges \
  | grep -ioE 'Co-Authored-By: *[^<]+' \
  | sed 's/Co-Authored-By: *//i' \
  | sed 's/ *$//' \
  | sort -uf || true)

ALL_CONTRIBUTORS=$(printf "%s\n%s" "$GIT_AUTHORS" "$CO_AUTHORS" \
  | sort -uf \
  | grep -v '^$' \
  | grep -viE '\[bot\]$|^dependabot|^github-actions|^copilot|^ZeroClaw Bot|^ZeroClaw Runner|^ZeroClaw Agent|^blacksmith' \
  || true)

CONTRIBUTOR_COUNT=$(echo "$ALL_CONTRIBUTORS" | grep -c . || echo "0")

CONTRIBUTOR_LIST=$(echo "$ALL_CONTRIBUTORS" \
  | while IFS= read -r name; do
    [ -z "$name" ] && continue
    echo "- **${name}**"
  done || true)

# --- Write temp files for section content ---

WHATS_NEW_FILE=$(mktemp)
cat > "$WHATS_NEW_FILE" <<WHATS_EOF

### 🚀 What's New in ${LATEST_TAG} (${MONTH_YEAR})

| Area | Highlights |
|---|---|
${FEATURES}

WHATS_EOF

CONTRIBUTORS_FILE=$(mktemp)
cat > "$CONTRIBUTORS_FILE" <<CONTRIB_EOF

### 🌟 Recent Contributors (${LATEST_TAG})

${CONTRIBUTOR_COUNT} contributors shipped features, fixes, and improvements in this release cycle:

${CONTRIBUTOR_LIST}

Thank you to everyone who opened issues, reviewed PRs, translated docs, and helped test. Every contribution matters. 🦀

CONTRIB_EOF

# --- Replace sections in all README files with markers ---

README_FILES=$(find . -maxdepth 1 -name 'README*.md' -type f | sort)
UPDATED=0

for readme in $README_FILES; do
  if ! grep -q 'BEGIN:WHATS_NEW' "$readme"; then
    continue
  fi

  python3 - "$readme" "$WHATS_NEW_FILE" "$CONTRIBUTORS_FILE" <<'PYEOF'
import sys, re

readme_path = sys.argv[1]
whats_new_path = sys.argv[2]
contributors_path = sys.argv[3]

with open(readme_path, 'r') as f:
    content = f.read()

with open(whats_new_path, 'r') as f:
    whats_new = f.read()

with open(contributors_path, 'r') as f:
    contributors = f.read()

content = re.sub(
    r'(<!-- BEGIN:WHATS_NEW -->)\n.*?(<!-- END:WHATS_NEW -->)',
    r'\1\n' + whats_new + r'\2',
    content,
    flags=re.DOTALL
)

content = re.sub(
    r'(<!-- BEGIN:RECENT_CONTRIBUTORS -->)\n.*?(<!-- END:RECENT_CONTRIBUTORS -->)',
    r'\1\n' + contributors + r'\2',
    content,
    flags=re.DOTALL
)

with open(readme_path, 'w') as f:
    f.write(content)
PYEOF

  UPDATED=$((UPDATED + 1))
done

rm -f "$WHATS_NEW_FILE" "$CONTRIBUTORS_FILE"

echo "README synced: ${LATEST_TAG} — ${CONTRIBUTOR_COUNT} contributors — ${UPDATED} files updated"
