#!/usr/bin/env bash
set -euo pipefail

# bump-version.sh — Update every hardcoded version reference in the repo.
#
# Usage:
#   scripts/release/bump-version.sh           # reads version from Cargo.toml
#   scripts/release/bump-version.sh 0.7.0     # explicit version
#
# This script is called automatically by the version-sync workflow
# whenever Cargo.toml changes on master. It can also be run locally.

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

if [[ $# -ge 1 ]]; then
  VERSION="$1"
else
  VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
fi

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "error: invalid semver: $VERSION" >&2
  exit 1
fi

echo "Syncing all version references to $VERSION ..."

changed=0
bump() {
  local file="$1" pattern="$2" replacement="$3"
  local target="$REPO_ROOT/$file"
  if [[ ! -f "$target" ]]; then
    echo "  skip (missing): $file"
    return
  fi
  if grep -qE "$pattern" "$target"; then
    sed -i '' -E "s|$pattern|$replacement|g" "$target" 2>/dev/null \
      || sed -i -E "s|$pattern|$replacement|g" "$target"
    echo "  updated: $file"
    changed=$((changed + 1))
  fi
}

# ── README version badges ──────────────────────────────────────────
echo "README badges..."
for readme in README.md docs/i18n/*/README.md; do
  bump "$readme" \
    'version-v[0-9]+\.[0-9]+\.[0-9]+-blue" alt="Version v[0-9]+\.[0-9]+\.[0-9]+"' \
    "version-v${VERSION}-blue\" alt=\"Version v${VERSION}\""
done

# ── Tauri desktop app config ───────────────────────────────────────
echo "Tauri config..."
TAURI_CONF="$REPO_ROOT/apps/tauri/tauri.conf.json"
if [[ -f "$TAURI_CONF" ]]; then
  if command -v jq >/dev/null 2>&1; then
    jq --arg v "$VERSION" '.version = $v' "$TAURI_CONF" > "$TAURI_CONF.tmp" \
      && mv "$TAURI_CONF.tmp" "$TAURI_CONF"
  else
    sed -i '' -E "s|\"version\": \"[^\"]+\"|\"version\": \"$VERSION\"|" "$TAURI_CONF" 2>/dev/null \
      || sed -i -E "s|\"version\": \"[^\"]+\"|\"version\": \"$VERSION\"|" "$TAURI_CONF"
  fi
  echo "  updated: apps/tauri/tauri.conf.json"
  changed=$((changed + 1))
fi

# ── Marketplace: Dokploy ───────────────────────────────────────────
echo "Marketplace templates..."
bump "marketplace/dokploy/meta-entry.json" \
  '"version": "[0-9]+\.[0-9]+\.[0-9]+"' \
  "\"version\": \"${VERSION}\""

bump "marketplace/dokploy/blueprints/quantclaw/docker-compose.yml" \
  'ghcr\.io/quant-speed/quantclaw:[0-9]+\.[0-9]+\.[0-9]+' \
  "ghcr.io/quant-speed/quantclaw:${VERSION}"

# ── Marketplace: EasyPanel ─────────────────────────────────────────
bump "marketplace/easypanel/meta.yaml" \
  'ghcr\.io/quant-speed/quantclaw:[0-9]+\.[0-9]+\.[0-9]+' \
  "ghcr.io/quant-speed/quantclaw:${VERSION}"

# ── Workflow description examples ──────────────────────────────────
echo "Workflow descriptions..."
for wf in \
  .github/workflows/sync-marketplace-templates.yml \
  .github/workflows/discord-release.yml \
  marketplace/sync-marketplace-templates.yml; do
  bump "$wf" \
    '\(e\.g\. v[0-9]+\.[0-9]+\.[0-9]+\)' \
    "(e.g. v${VERSION})"
done

echo ""
if [[ $changed -gt 0 ]]; then
  echo "Done — $changed file(s) updated to v$VERSION."
else
  echo "Done — all files already at v$VERSION."
fi
