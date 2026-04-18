#!/usr/bin/env bash
set -euo pipefail

# ── Manual release script ─────────────────────────────────────────────
# Build and publish a stable release from your local machine.
# Works with any authenticated `gh` CLI — no PATs or GitHub Apps needed.
#
# Usage:
#   scripts/release/release.sh <version>           # full release
#   scripts/release/release.sh <version> --dry-run  # build only, no publish
#
# Prerequisites:
#   - Rust toolchain (1.93.0+)
#   - Node.js 22+
#   - gh CLI authenticated (`gh auth status`)
#   - Docker with buildx (for Docker image push)
#   - Push access to zeroclaw-labs/zeroclaw
#
# What it does:
#   1. Validates version and environment
#   2. Syncs version references across the repo
#   3. Builds the binary for your current platform
#   4. Creates a GitHub release with the binary
#   5. Optionally triggers downstream jobs (Docker, Scoop, AUR, etc.)
#
# What it does NOT do (handled by CI or separate scripts):
#   - Cross-compile for all 8 platforms (CI does this)
#   - Build the Tauri desktop app
#   - Push Docker images
#   - Update package managers (Scoop, Homebrew, AUR)
#   - Post to Discord/Twitter
#
# For a full multi-platform release, push the tag and let CI handle it:
#   scripts/release/cut_release_tag.sh v0.7.1 --push

REPO="zeroclaw-labs/zeroclaw"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ── Parse arguments ───────────────────────────────────────────────────
usage() {
  cat <<'EOF'
Usage: scripts/release/release.sh <version> [options]

Arguments:
  version       Semver version to release (e.g. 0.7.1)

Options:
  --dry-run     Build and package but don't publish
  --skip-build  Skip building (use existing binary)
  --local-only  Only build for current platform (default)
  --help        Show this help

Examples:
  scripts/release/release.sh 0.7.1              # build + publish
  scripts/release/release.sh 0.7.1 --dry-run    # build only
  scripts/release/release.sh 0.7.1 --skip-build # publish existing build
EOF
}

if [[ $# -lt 1 ]]; then
  usage
  exit 1
fi

VERSION="$1"
shift

DRY_RUN=false
SKIP_BUILD=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)    DRY_RUN=true ;;
    --skip-build) SKIP_BUILD=true ;;
    --local-only) ;; # default behavior
    --help)       usage; exit 0 ;;
    *)            echo "Unknown option: $1"; usage; exit 1 ;;
  esac
  shift
done

TAG="v${VERSION}"

# ── Colors ────────────────────────────────────────────────────────────
bold() { printf '\033[1m%s\033[0m\n' "$*"; }
green() { printf '\033[32m✓ %s\033[0m\n' "$*"; }
red() { printf '\033[31m✗ %s\033[0m\n' "$*"; }
step() { printf '\n\033[1;34m── %s ──\033[0m\n' "$*"; }

# ── Step 1: Validate ──────────────────────────────────────────────────
step "Validating environment"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  red "Version must be semver (X.Y.Z). Got: $VERSION"
  exit 1
fi
green "Version format: $VERSION"

if ! command -v gh &>/dev/null; then
  red "gh CLI not found. Install: https://cli.github.com"
  exit 1
fi

if ! gh auth status &>/dev/null; then
  red "gh CLI not authenticated. Run: gh auth login"
  exit 1
fi
green "gh CLI authenticated"

if ! command -v cargo &>/dev/null; then
  red "Rust toolchain not found. Install: https://rustup.rs"
  exit 1
fi
green "Rust toolchain available"

if ! command -v node &>/dev/null; then
  red "Node.js not found. Required for web dashboard build."
  exit 1
fi
green "Node.js available"

cd "$REPO_ROOT"

cargo_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
if [[ "$cargo_version" != "$VERSION" ]]; then
  red "Cargo.toml version ($cargo_version) doesn't match $VERSION"
  echo "Run: sed -i '' 's/^version = \"$cargo_version\"/version = \"$VERSION\"/' Cargo.toml && cargo check"
  exit 1
fi
green "Cargo.toml version matches"

if git ls-remote --exit-code --tags origin "refs/tags/$TAG" &>/dev/null; then
  red "Tag $TAG already exists on origin"
  exit 1
fi
green "Tag $TAG is available"

if ! git diff --quiet || ! git diff --cached --quiet; then
  red "Working tree is not clean. Commit or stash changes first."
  exit 1
fi
green "Working tree is clean"

# ── Step 2: Sync version references ──────────────────────────────────
step "Syncing version references"

bash "$SCRIPT_DIR/bump-version.sh" "$VERSION"
if ! git diff --quiet; then
  git add -A
  git commit -m "chore: sync version references to $TAG"
  green "Committed version sync"
else
  green "All references already at $VERSION"
fi

# ── Step 3: Detect platform ──────────────────────────────────────────
step "Detecting build target"

ARCH=$(uname -m)
OS=$(uname -s)

case "$OS-$ARCH" in
  Darwin-arm64)  TARGET="aarch64-apple-darwin" ;;
  Darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
  Linux-x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
  *)
    red "Unsupported platform: $OS-$ARCH"
    echo "Use CI for cross-platform builds: scripts/release/cut_release_tag.sh $TAG --push"
    exit 1
    ;;
esac

green "Build target: $TARGET"

# ── Step 4: Build web dashboard ───────────────────────────────────────
if [[ "$SKIP_BUILD" == false ]]; then
  step "Building web dashboard"
  cd "$REPO_ROOT/web"
  npm ci --silent
  npm run build
  green "Web dashboard built"
  cd "$REPO_ROOT"
fi

# ── Step 5: Build release binary ──────────────────────────────────────
if [[ "$SKIP_BUILD" == false ]]; then
  step "Building release binary ($TARGET)"

  FEATURES="channel-matrix,channel-lark,whatsapp-web"
  cargo build --release --locked --features "$FEATURES" --target "$TARGET"
  green "Binary built"

  BINARY="target/$TARGET/release/zeroclaw"
  if [[ ! -f "$BINARY" ]]; then
    red "Binary not found at $BINARY"
    exit 1
  fi

  SIZE=$(wc -c < "$BINARY" | tr -d ' ')
  SIZE_MB=$((SIZE / 1024 / 1024))
  if [[ $SIZE -gt 52428800 ]]; then
    red "Binary too large: ${SIZE_MB}MB (limit: 50MB)"
    exit 1
  fi
  green "Binary size: ${SIZE_MB}MB"
fi

# ── Step 6: Package ───────────────────────────────────────────────────
step "Packaging release assets"

RELEASE_DIR="$REPO_ROOT/target/release-assets"
rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR/staging/web"

BINARY="target/$TARGET/release/zeroclaw"
cp "$BINARY" "$RELEASE_DIR/staging/"
cp -r web/dist "$RELEASE_DIR/staging/web/dist"

ARCHIVE_NAME="zeroclaw-${TARGET}.tar.gz"
cd "$RELEASE_DIR/staging"
tar czf "$RELEASE_DIR/$ARCHIVE_NAME" zeroclaw web/dist
cd "$REPO_ROOT"

cp install.sh "$RELEASE_DIR/"

cd "$RELEASE_DIR"
sha256sum "$ARCHIVE_NAME" > SHA256SUMS 2>/dev/null \
  || shasum -a 256 "$ARCHIVE_NAME" > SHA256SUMS
cd "$REPO_ROOT"

green "Packaged: $ARCHIVE_NAME"

echo ""
bold "Release assets:"
ls -lh "$RELEASE_DIR"/*.tar.gz "$RELEASE_DIR/SHA256SUMS" "$RELEASE_DIR/install.sh"

# ── Step 7: Generate release notes ────────────────────────────────────
step "Generating release notes"

if [[ -f "CHANGELOG-next.md" ]]; then
  cp CHANGELOG-next.md "$RELEASE_DIR/release-notes.md"
  green "Using hand-written CHANGELOG-next.md"
else
  PREV_TAG=$(git tag --sort=-creatordate | grep -vE '\-beta\.' | head -1 || echo "")
  if [[ -n "$PREV_TAG" ]]; then
    RANGE="${PREV_TAG}..HEAD"
  else
    RANGE="HEAD"
  fi

  FEATURES_LIST=$(git log "$RANGE" --pretty=format:"%s" --no-merges \
    | grep -iE '^feat(\(|:)' \
    | sed 's/^feat(\([^)]*\)): /\1: /' \
    | sed 's/^feat: //' \
    | sed 's/ (#[0-9]*)$//' \
    | sort -uf \
    | while IFS= read -r line; do echo "- ${line}"; done || true)

  if [[ -z "$FEATURES_LIST" ]]; then
    FEATURES_LIST="- Incremental improvements and polish"
  fi

  GIT_AUTHORS=$(git log "$RANGE" --pretty=format:"%an" --no-merges | sort -uf || true)
  CO_AUTHORS=$(git log "$RANGE" --pretty=format:"%b" --no-merges \
    | grep -ioE 'Co-Authored-By: *[^<]+' \
    | sed 's/Co-Authored-By: *//i' \
    | sed 's/ *$//' \
    | sort -uf || true)

  ALL_CONTRIBUTORS=$(printf "%s\n%s" "$GIT_AUTHORS" "$CO_AUTHORS" \
    | sort -uf \
    | grep -v '^$' \
    | grep -viE '\[bot\]$|^dependabot|^github-actions|^copilot|^ZeroClaw Bot|^ZeroClaw Runner|^ZeroClaw Agent|^blacksmith' \
    | while IFS= read -r name; do echo "- ${name}"; done || true)

  cat > "$RELEASE_DIR/release-notes.md" <<NOTES
## What's New

${FEATURES_LIST}

## Contributors

${ALL_CONTRIBUTORS}

---
*Full changelog: ${PREV_TAG}...$TAG*
NOTES

  green "Auto-generated release notes from commits"
fi

echo ""
bold "Release notes preview:"
head -20 "$RELEASE_DIR/release-notes.md"
echo "..."

# ── Step 8: Publish ───────────────────────────────────────────────────
if [[ "$DRY_RUN" == true ]]; then
  step "Dry run complete"
  echo ""
  bold "Assets ready in: $RELEASE_DIR"
  echo ""
  echo "To publish manually:"
  echo "  git tag -a $TAG -m 'zeroclaw $TAG'"
  echo "  git push origin $TAG"
  echo "  gh release create $TAG $RELEASE_DIR/*.tar.gz $RELEASE_DIR/SHA256SUMS $RELEASE_DIR/install.sh \\"
  echo "    --title '$TAG' --notes-file $RELEASE_DIR/release-notes.md --latest"
  echo ""
  echo "Or to let CI build all platforms:"
  echo "  scripts/release/cut_release_tag.sh $TAG --push"
  exit 0
fi

step "Publishing release"

echo "This will:"
echo "  1. Create and push tag $TAG"
echo "  2. Create GitHub release with your local $TARGET binary"
echo ""
bold "Note: This release will only contain the $TARGET binary."
bold "For a full multi-platform release, use: scripts/release/cut_release_tag.sh $TAG --push"
echo ""
read -rp "Continue? [y/N] " confirm
if [[ "$confirm" != [yY] ]]; then
  echo "Aborted."
  exit 0
fi

git tag -a "$TAG" -m "zeroclaw $TAG"
git push origin "$TAG"
green "Tag $TAG pushed"

gh release create "$TAG" \
  "$RELEASE_DIR/$ARCHIVE_NAME" \
  "$RELEASE_DIR/SHA256SUMS" \
  "$RELEASE_DIR/install.sh" \
  --repo "$REPO" \
  --title "$TAG" \
  --notes-file "$RELEASE_DIR/release-notes.md" \
  --latest

green "Release published: https://github.com/$REPO/releases/tag/$TAG"

# ── Step 9: Post-release triggers ─────────────────────────────────────
step "Post-release"

echo ""
echo "Release is live. The following can be triggered manually if needed:"
echo ""
echo "  # Trigger full CI build (all platforms + Docker + package managers):"
echo "  gh workflow run 'Release Stable' -f version=$VERSION"
echo ""
echo "  # Or trigger individual downstream jobs:"
echo "  gh workflow run 'Pub Scoop Manifest' -f release_tag=$TAG"
echo "  gh workflow run 'Pub AUR Package' -f release_tag=$TAG"
echo "  gh workflow run 'Pub Homebrew Core' -f release_tag=$TAG"
echo "  gh workflow run 'Sync Marketplace Templates' -f release_tag=$TAG"
echo "  gh workflow run 'Discord Announcement' -f release_tag=$TAG"
echo "  gh workflow run 'Tweet Release' -f release_tag=$TAG"
echo ""
bold "Done! 🎉"
