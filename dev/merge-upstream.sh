#!/usr/bin/env bash
set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# merge-upstream.sh — Automate upstream ZeroClaw sync for One2X custom fork
#
# Usage:
#   ./dev/merge-upstream.sh              # Create next version branch (auto-increment)
#   ./dev/merge-upstream.sh v6           # Create specific version branch
#   ./dev/merge-upstream.sh --dry-run    # Show what would happen without doing it
#
# What it does:
#   1. Fetches latest upstream/master
#   2. Creates one2x/custom-vN branch from upstream/master
#   3. Cherry-picks all One2X custom commits
#   4. Runs cargo fmt + clippy + test
#   5. Reports results
#
# If a cherry-pick conflicts, the script stops and prints which commit
# and which files are conflicted. Fix manually, then re-run.
# ─────────────────────────────────────────────────────────────────────────────

UPSTREAM_REMOTE="upstream"
UPSTREAM_BRANCH="master"
CUSTOM_BRANCH_PREFIX="one2x/custom-"
DRY_RUN=false

# Parse args
TARGET_VERSION=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        v*) TARGET_VERSION="$arg" ;;
        *) echo "Unknown argument: $arg"; exit 1 ;;
    esac
done

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC} $1"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Find the current custom branch
CURRENT_BRANCH=$(git branch --show-current)
if [[ ! "$CURRENT_BRANCH" =~ ^one2x/custom- ]]; then
    error "Not on a one2x/custom-* branch. Current: $CURRENT_BRANCH"
    error "Please checkout the latest custom branch first."
    exit 1
fi

CURRENT_VERSION=$(echo "$CURRENT_BRANCH" | sed 's/one2x\/custom-v//')
info "Current branch: $CURRENT_BRANCH (version $CURRENT_VERSION)"

# Determine target version
if [ -z "$TARGET_VERSION" ]; then
    NEXT_VERSION=$((CURRENT_VERSION + 1))
    TARGET_VERSION="v$NEXT_VERSION"
fi
TARGET_BRANCH="${CUSTOM_BRANCH_PREFIX}${TARGET_VERSION}"
info "Target branch: $TARGET_BRANCH"

# Find custom commits (commits on current branch not in upstream)
info "Finding custom commits..."
git fetch "$UPSTREAM_REMOTE" "$UPSTREAM_BRANCH" --quiet

CUSTOM_COMMITS=$(git log --oneline "${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}..HEAD" --reverse --format="%H")
COMMIT_COUNT=$(echo "$CUSTOM_COMMITS" | grep -c . || true)

if [ "$COMMIT_COUNT" -eq 0 ]; then
    error "No custom commits found on $CURRENT_BRANCH relative to $UPSTREAM_REMOTE/$UPSTREAM_BRANCH"
    exit 1
fi

info "Found $COMMIT_COUNT custom commit(s) to cherry-pick:"
git log --oneline "${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}..HEAD" --reverse

# Show upstream delta
UPSTREAM_AHEAD=$(git rev-list --count "HEAD..${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}")
info "Upstream is $UPSTREAM_AHEAD commit(s) ahead"

if $DRY_RUN; then
    info "[DRY RUN] Would create $TARGET_BRANCH from $UPSTREAM_REMOTE/$UPSTREAM_BRANCH"
    info "[DRY RUN] Would cherry-pick $COMMIT_COUNT commit(s)"
    exit 0
fi

# Create new branch from upstream
info "Creating $TARGET_BRANCH from $UPSTREAM_REMOTE/$UPSTREAM_BRANCH..."
git checkout -b "$TARGET_BRANCH" "${UPSTREAM_REMOTE}/${UPSTREAM_BRANCH}"

# Cherry-pick custom commits one by one
APPLIED=0
for COMMIT in $CUSTOM_COMMITS; do
    SHORT=$(git log --oneline -1 "$COMMIT")
    info "Cherry-picking: $SHORT"

    if ! git cherry-pick "$COMMIT" --no-edit 2>/dev/null; then
        echo ""
        error "CONFLICT during cherry-pick of: $SHORT"
        echo ""
        echo "Conflicted files:"
        git diff --name-only --diff-filter=U
        echo ""
        echo "To resolve:"
        echo "  1. Fix the conflicts manually"
        echo "  2. git add <resolved-files>"
        echo "  3. git cherry-pick --continue"
        echo "  4. Re-run: ./dev/merge-upstream.sh"
        echo ""
        echo "To abort: git cherry-pick --abort && git checkout $CURRENT_BRANCH && git branch -D $TARGET_BRANCH"
        exit 1
    fi
    APPLIED=$((APPLIED + 1))
done

info "Successfully cherry-picked $APPLIED/$COMMIT_COUNT commit(s)"

# Verify build
info "Running cargo fmt --check..."
if ! cargo fmt --all -- --check 2>/dev/null; then
    warn "Formatting issues detected, auto-fixing..."
    cargo fmt --all
    git add -A && git commit -m "one2x: cargo fmt" --no-verify
fi

info "Running cargo clippy..."
if ! cargo clippy --features one2x -- -D warnings 2>&1 | tail -5; then
    error "Clippy found warnings. Fix them before proceeding."
    exit 1
fi

info "Running cargo test..."
if ! cargo test 2>&1 | tail -10; then
    error "Tests failed. Fix them before proceeding."
    exit 1
fi

echo ""
info "╔══════════════════════════════════════════════════════════╗"
info "║  Merge complete: $TARGET_BRANCH                        ║"
info "║  Cherry-picked: $APPLIED commits                       ║"
info "║  All checks passed (fmt + clippy + test)               ║"
info "╚══════════════════════════════════════════════════════════╝"
echo ""
info "Next steps:"
echo "  git push -u origin $TARGET_BRANCH"
echo "  # Update loveops Dockerfile and workflow to use $TARGET_BRANCH"
