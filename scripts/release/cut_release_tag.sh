#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/release/cut_release_tag.sh <tag> [--push]

Create an annotated release tag from the current checkout.

Requirements:
- tag must match vX.Y.Z (optional suffix like -rc.1)
- working tree must be clean
- HEAD must match origin/main
- tag must not already exist locally or on origin

Options:
  --push   Push the tag to origin after creating it
USAGE
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage
  exit 1
fi

TAG="$1"
PUSH_TAG="false"
if [[ $# -eq 2 ]]; then
  if [[ "$2" != "--push" ]]; then
    usage
    exit 1
  fi
  PUSH_TAG="true"
fi

SEMVER_PATTERN='^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$'
if [[ ! "$TAG" =~ $SEMVER_PATTERN ]]; then
  echo "error: tag must match vX.Y.Z or vX.Y.Z-suffix (received: $TAG)" >&2
  exit 1
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "error: run this script inside the git repository" >&2
  exit 1
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "error: working tree is not clean; commit or stash changes first" >&2
  exit 1
fi

echo "Fetching origin/main and tags..."
git fetch --quiet origin main --tags

HEAD_SHA="$(git rev-parse HEAD)"
MAIN_SHA="$(git rev-parse origin/main)"
if [[ "$HEAD_SHA" != "$MAIN_SHA" ]]; then
  echo "error: HEAD ($HEAD_SHA) is not origin/main ($MAIN_SHA)." >&2
  echo "hint: checkout/update main before cutting a release tag." >&2
  exit 1
fi

# --- CI green gate (blocks on pending/failure, warns on unavailable) ---
echo "Checking CI status on HEAD ($HEAD_SHA)..."
if command -v gh >/dev/null 2>&1; then
  CI_STATUS="$(gh api "repos/$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null || echo 'zeroclaw-labs/zeroclaw')/commits/${HEAD_SHA}/check-runs" \
    --jq '[.check_runs[] | select(.name == "CI Required Gate")] |
           if length == 0 then "not_found"
           elif .[0].conclusion == "success" then "success"
           elif .[0].status != "completed" then "pending"
           else .[0].conclusion end' 2>/dev/null || echo "api_error")"

  case "$CI_STATUS" in
    success)
      echo "CI Required Gate: passed"
      ;;
    pending)
      echo "error: CI is still running on $HEAD_SHA. Wait for CI Required Gate to complete." >&2
      exit 1
      ;;
    not_found)
      echo "warning: CI Required Gate check-run not found for $HEAD_SHA." >&2
      echo "hint: ensure ci-run.yml has completed on main before cutting a release tag." >&2
      ;;
    api_error)
      echo "warning: could not query GitHub API for CI status (gh CLI issue or auth)." >&2
      echo "hint: CI status will be verified server-side by release_trigger_guard.py." >&2
      ;;
    *)
      echo "error: CI Required Gate conclusion is '$CI_STATUS' (expected 'success')." >&2
      exit 1
      ;;
  esac
else
  echo "warning: gh CLI not found; skipping local CI status check."
  echo "hint: CI status will be verified server-side by release_trigger_guard.py."
fi

# --- Cargo.lock consistency pre-flight ---
echo "Checking Cargo.lock consistency..."
if command -v cargo >/dev/null 2>&1; then
  if ! cargo check --locked --quiet; then
    echo "error: cargo check --locked failed." >&2
    echo "hint: if this is lockfile drift, run 'cargo check' and commit the updated Cargo.lock." >&2
    exit 1
  fi
  echo "Cargo.lock: consistent"
else
  echo "warning: cargo not found; skipping Cargo.lock consistency check."
fi

if git show-ref --tags --verify --quiet "refs/tags/$TAG"; then
  echo "error: tag already exists locally: $TAG" >&2
  exit 1
fi

if git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
  echo "error: tag already exists on origin: $TAG" >&2
  exit 1
fi

MESSAGE="zeroclaw $TAG"
git tag -a "$TAG" -m "$MESSAGE"
echo "Created annotated tag: $TAG"

if [[ "$PUSH_TAG" == "true" ]]; then
  git push origin "$TAG"
  echo "Pushed tag to origin: $TAG"
  echo "GitHub release pipeline will run via .github/workflows/pub-release.yml"
else
  echo "Next step: git push origin $TAG"
fi
