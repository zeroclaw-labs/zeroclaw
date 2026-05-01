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

# ── Workspace Cargo.toml ───────────────────────────────────────────
# Bumps [workspace.package] version (the root version inherited by every child
# crate via `version.workspace = true`) and the version pins on every path dep
# in [workspace.dependencies], skipping aardvark* which tracks an independent
# version.
echo "Workspace Cargo.toml..."
ROOT_CARGO="$REPO_ROOT/Cargo.toml"
if [[ -f "$ROOT_CARGO" ]]; then
  before="$(sha256sum "$ROOT_CARGO" | awk '{print $1}')"
  # [workspace.package] version — first bare `version = "..."` line in the file
  sed -i -E '0,/^version = "[^"]+"/s||version = "'"$VERSION"'"|' "$ROOT_CARGO" 2>/dev/null \
    || sed -i '' -E '/^version = "[^"]+"/{s//version = "'"$VERSION"'"/;:a;n;ba;}' "$ROOT_CARGO"
  # [workspace.dependencies] path-dep version pins, skipping aardvark*
  sed -i -E '/path = "crates\/aardvark/!s|(path = "crates/[^"]+", version = ")[^"]+(")|\1'"$VERSION"'\2|' "$ROOT_CARGO" 2>/dev/null \
    || sed -i '' -E '/path = "crates\/aardvark/!s|(path = "crates/[^"]+", version = ")[^"]+(")|\1'"$VERSION"'\2|' "$ROOT_CARGO"
  after="$(sha256sum "$ROOT_CARGO" | awk '{print $1}')"
  if [[ "$before" != "$after" ]]; then
    echo "  updated: Cargo.toml ([workspace.package] + [workspace.dependencies])"
    changed=$((changed + 1))
  fi
fi

# ── Cargo.lock (workspace crates only) ─────────────────────────────
# Re-resolves only the workspace member entries so their lockfile versions
# track the new [workspace.package] / [workspace.dependencies] values. External
# deps that happen to share a version string are left alone.
echo "Cargo.lock..."
ROOT_LOCK="$REPO_ROOT/Cargo.lock"
if [[ -f "$ROOT_LOCK" ]] && command -v cargo >/dev/null 2>&1; then
  before="$(sha256sum "$ROOT_LOCK" | awk '{print $1}')"
  ( cd "$REPO_ROOT" && cargo update --workspace --offline >/dev/null 2>&1 ) \
    || ( cd "$REPO_ROOT" && cargo update --workspace >/dev/null 2>&1 ) \
    || echo "  warn: cargo update --workspace failed; review Cargo.lock manually"
  after="$(sha256sum "$ROOT_LOCK" | awk '{print $1}')"
  if [[ "$before" != "$after" ]]; then
    echo "  updated: Cargo.lock"
    changed=$((changed + 1))
  fi
elif [[ -f "$ROOT_LOCK" ]]; then
  echo "  skip: cargo not on PATH; Cargo.lock not refreshed"
fi

# ── Marketplace: Dokploy ───────────────────────────────────────────
echo "Marketplace templates..."
bump "marketplace/dokploy/meta-entry.json" \
  '"version": "[0-9]+\.[0-9]+\.[0-9]+"' \
  "\"version\": \"${VERSION}\""

bump "marketplace/dokploy/blueprints/zeroclaw/docker-compose.yml" \
  'ghcr\.io/zeroclaw-labs/zeroclaw:[0-9]+\.[0-9]+\.[0-9]+' \
  "ghcr.io/zeroclaw-labs/zeroclaw:${VERSION}"

# ── Marketplace: EasyPanel ─────────────────────────────────────────
bump "marketplace/easypanel/meta.yaml" \
  'ghcr\.io/zeroclaw-labs/zeroclaw:[0-9]+\.[0-9]+\.[0-9]+' \
  "ghcr.io/zeroclaw-labs/zeroclaw:${VERSION}"

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
