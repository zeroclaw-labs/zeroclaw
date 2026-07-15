#!/usr/bin/env bash
# Refresh, commit, push, tag, and pin the docs translation catalogues in the
# zeroclaw-docs-translations submodule (docs/book/po). Cuts the v{version} tag
# in the submodule and pins the main-repo gitlink to it. One command, no
# hand-typed version: the version is read from Cargo.toml (the single source of
# truth), the same way bump-version.sh derives it. Run bump-version.sh
# separately to sync the rest of the version references.
#
# Usage:
#   ./scripts/release/refresh-translations.sh --model-provider anthropic.release
#   ./scripts/release/refresh-translations.sh 0.8.2 --model-provider llama_cpp.qwen
#   ./scripts/release/refresh-translations.sh --no-translate
#
# Requires push access to zeroclaw-labs/zeroclaw-docs-translations. The submodule
# is initialised automatically if it is not yet checked out.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SUBMODULE_PATH="$REPO_ROOT/docs/book/po"

translate=1
VERSION=""
MODEL_PROVIDER=""
CONFIG_DIR=""

usage() {
  cat <<'EOF'
Usage:
  refresh-translations.sh [VERSION] --model-provider <configured-alias> [--config-dir <dir>]
  refresh-translations.sh [VERSION] --no-translate
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model-provider)
      if [[ $# -lt 2 || "$2" == -* ]]; then
        echo "error: --model-provider requires a value" >&2
        exit 2
      fi
      MODEL_PROVIDER="$2"
      shift 2
      ;;
    --model-provider=*)
      MODEL_PROVIDER="${1#*=}"
      shift
      ;;
    --config-dir)
      if [[ $# -lt 2 || "$2" == -* ]]; then
        echo "error: --config-dir requires a value" >&2
        exit 2
      fi
      CONFIG_DIR="$2"
      shift 2
      ;;
    --config-dir=*)
      CONFIG_DIR="${1#*=}"
      shift
      ;;
    --no-translate)
      translate=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      echo "error: unknown flag: $1" >&2
      usage >&2
      exit 2
      ;;
    *)
      if [[ -n "$VERSION" ]]; then
        echo "error: multiple version arguments: $VERSION and $1" >&2
        exit 2
      fi
      VERSION="$1"
      shift
      ;;
  esac
done

if [[ "$translate" -eq 1 && -z "$MODEL_PROVIDER" ]]; then
  echo "error: --model-provider <configured-alias> is required unless --no-translate is used" >&2
  exit 2
fi

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

git -C "$SUBMODULE_PATH" fetch --quiet origin main
remote_main="$(git -C "$SUBMODULE_PATH" rev-parse refs/remotes/origin/main)"
submodule_head="$(git -C "$SUBMODULE_PATH" rev-parse HEAD)"
if [[ -n "$(git -C "$SUBMODULE_PATH" status --porcelain)" ]]; then
  if [[ "$submodule_head" != "$remote_main" ]]; then
    echo "error: prepared catalogues are not based on current origin/main" >&2
    echo "       update or rebase docs/book/po before running the release wrapper." >&2
    exit 1
  fi
else
  git -C "$SUBMODULE_PATH" checkout --quiet --detach "$remote_main"
fi

if git -C "$SUBMODULE_PATH" rev-parse --verify --quiet "refs/tags/${TAG}" >/dev/null; then
  echo "error: tag ${TAG} already exists in the submodule; nothing to cut." >&2
  echo "       bump the version or delete the stale tag before re-running." >&2
  exit 1
fi

remote_tag_status=0
git -C "$SUBMODULE_PATH" ls-remote --exit-code --tags origin "refs/tags/${TAG}" \
  >/dev/null 2>&1 || remote_tag_status=$?
case "$remote_tag_status" in
  0)
    echo "error: tag ${TAG} already exists on the submodule remote; nothing to cut." >&2
    exit 1
    ;;
  2) ;;
  *)
    echo "error: unable to verify whether ${TAG} exists on the submodule remote" >&2
    exit 1
    ;;
esac

echo "Refreshing translation catalogues for ${TAG} ..."

if [[ "$translate" -eq 1 ]]; then
  sync_args=(mdbook sync --model-provider "$MODEL_PROVIDER")
  if [[ -n "$CONFIG_DIR" ]]; then
    sync_args+=(--config-dir "$CONFIG_DIR")
  fi
  ( cd "$REPO_ROOT" && cargo "${sync_args[@]}" )
  ( cd "$REPO_ROOT" && cargo mdbook check )
fi

if [[ -n "$(git -C "$SUBMODULE_PATH" status --porcelain)" ]]; then
  git -C "$SUBMODULE_PATH" add -A
  git -C "$SUBMODULE_PATH" commit -m "chore: refresh catalogues for ${TAG}"
  git -C "$SUBMODULE_PATH" push origin HEAD:main
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
