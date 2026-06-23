#!/usr/bin/env bash
# Refresh, commit, push, tag, and pin the docs translation catalogues in the
# zeroclaw-docs-translations submodule (docs/book/po). Cuts the v{version} tag
# in the submodule and pins the main-repo gitlink to it. One command, no
# hand-typed version: the version is read from Cargo.toml (the single source of
# truth), the same way bump-version.sh derives it. Run bump-version.sh
# separately to sync the rest of the version references.
#
# Usage:
#   ./scripts/release/refresh-translations.sh                 # version from Cargo.toml
#   ./scripts/release/refresh-translations.sh 0.8.2           # explicit override
#   ./scripts/release/refresh-translations.sh --no-translate  # skip the sync pass
#
# Requires push access to zeroclaw-labs/zeroclaw-docs-translations. The submodule
# is initialised automatically if it is not yet checked out.
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
  echo "Initialising docs/book/po submodule ..."
  git -C "$REPO_ROOT" submodule update --init docs/book/po
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

echo "Pinning main-repo gitlink to ${TAG} ..."
git -C "$SUBMODULE_PATH" checkout --quiet "$TAG"
git -C "$REPO_ROOT" add docs/book/po

echo "Done. docs/book/po pinned to ${TAG}. Commit the gitlink with the version bump."
