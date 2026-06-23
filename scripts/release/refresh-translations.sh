#!/usr/bin/env bash
# Refresh, commit, push, and tag the docs translation catalogues in the
# zeroclaw-docs-translations submodule (docs/book/po), then pin the main-repo
# gitlink to that tag via bump-version.sh. One command, no hand-typed version:
# the version is read from Cargo.toml (the single source of truth), the same way
# bump-version.sh derives it.
#
# Usage:
#   ./scripts/release/refresh-translations.sh                 # version from Cargo.toml
#   ./scripts/release/refresh-translations.sh 0.8.2           # explicit override
#   ./scripts/release/refresh-translations.sh --no-translate  # skip the sync pass
#
# Requires the submodule checked out (git submodule update --init docs/book/po)
# and push access to zeroclaw-labs/zeroclaw-docs-translations.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SUBMODULE_PATH="$REPO_ROOT/docs/book/po"

translate=1
VERSION=""
for arg in "$@"; do
  case "$arg" in
    --no-translate) translate=0 ;;
    -*) echo "error: unknown flag: $arg" >&2; exit 2 ;;
    *) VERSION="$arg" ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
fi
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "error: invalid semver: $VERSION" >&2
  exit 1
fi
TAG="v${VERSION}"

if [[ ! -d "$SUBMODULE_PATH/.git" && ! -f "$SUBMODULE_PATH/.git" ]]; then
  echo "error: docs/book/po submodule not initialised." >&2
  echo "       run: git submodule update --init docs/book/po" >&2
  exit 1
fi

echo "Refreshing translation catalogues for ${TAG} ..."

if [[ "$translate" -eq 1 ]]; then
  ( cd "$REPO_ROOT" && cargo mdbook sync --model-provider ollama )
  ( cd "$REPO_ROOT" && cargo mdbook check )
fi

if git -C "$SUBMODULE_PATH" rev-parse --verify --quiet "refs/tags/${TAG}" >/dev/null; then
  echo "error: tag ${TAG} already exists in the submodule; nothing to cut." >&2
  echo "       bump the version or delete the stale tag before re-running." >&2
  exit 1
fi

if [[ -n "$(git -C "$SUBMODULE_PATH" status --porcelain)" ]]; then
  git -C "$SUBMODULE_PATH" add -A
  git -C "$SUBMODULE_PATH" commit -m "chore: refresh catalogues for ${TAG}"
  git -C "$SUBMODULE_PATH" push origin main
else
  echo "  catalogues unchanged; tagging the current submodule HEAD"
fi

git -C "$SUBMODULE_PATH" tag "$TAG"
git -C "$SUBMODULE_PATH" push origin "$TAG"
echo "  submodule tagged ${TAG} and pushed"

echo "Pinning main-repo gitlink ..."
"$REPO_ROOT/scripts/release/bump-version.sh" "$VERSION"

echo "Done. docs/book/po pinned to ${TAG}."
