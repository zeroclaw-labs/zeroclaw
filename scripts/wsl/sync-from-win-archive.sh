#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." >/dev/null 2>&1 && pwd)"

SOURCE_REPO="${WIN_ARCHIVE_REPO:-/home/lasve046/pilot/zeroclaw-win-archive}"
DRY_RUN=false
DELETE=false
ALLOW_OLDER_SOURCE=false

usage() {
  cat <<'HELP'
Sync files from the archived/legacy repo into this WSL-primary repo.

Usage:
  scripts/wsl/sync-from-win-archive.sh [options]

Options:
  --source <path>         Source repository path
  --delete                Delete files in destination that no longer exist in source
  --dry-run               Show changes without applying
  --allow-older-source    Allow syncing even if source commit is older than destination
  -h, --help              Show help

Safety behavior:
  - Refuses sync when source HEAD is an ancestor of destination HEAD.
    This prevents older archive content from overwriting newer WSL content.
  - Use --allow-older-source only for intentional rollback/replay scenarios.
HELP
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source)
      SOURCE_REPO="$2"
      shift 2
      ;;
    --delete)
      DELETE=true
      shift
      ;;
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --allow-older-source)
      ALLOW_OLDER_SOURCE=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "Unknown argument: $1"
      ;;
  esac
done

command -v rsync >/dev/null 2>&1 || die "rsync is required"

SOURCE_REPO="$(cd "$SOURCE_REPO" >/dev/null 2>&1 && pwd)"
DEST_REPO="$REPO_ROOT"

[[ -d "$SOURCE_REPO/.git" ]] || die "Source is not a git repo: $SOURCE_REPO"
[[ -d "$DEST_REPO/.git" ]] || die "Destination is not a git repo: $DEST_REPO"

if [[ "$SOURCE_REPO" == "$DEST_REPO" ]]; then
  die "Source and destination are the same path"
fi

source_head="$(git -C "$SOURCE_REPO" rev-parse HEAD)"
dest_head="$(git -C "$DEST_REPO" rev-parse HEAD)"

if [[ "$ALLOW_OLDER_SOURCE" == false ]]; then
  if git -C "$DEST_REPO" merge-base --is-ancestor "$source_head" "$dest_head"; then
    printf 'Source HEAD: %s\n' "$source_head"
    printf 'Dest HEAD:   %s\n' "$dest_head"
    die "Refusing sync: source is older or equal to destination. Use --allow-older-source to override."
  fi
fi

RSYNC_ARGS=(
  -a
  --checksum
  --no-times
  --human-readable
  --itemize-changes
  --exclude .git/
  --exclude target/
  --exclude web/node_modules/
  --exclude .cache/
  --exclude .zeroclaw/
  --exclude LEGACY_WINDOWS_ARCHIVE.md
)

if [[ "$DELETE" == true ]]; then
  RSYNC_ARGS+=(--delete)
fi

if [[ "$DRY_RUN" == true ]]; then
  RSYNC_ARGS+=(--dry-run)
fi

printf 'Sync source: %s\n' "$SOURCE_REPO"
printf 'Sync dest:   %s\n' "$DEST_REPO"
printf 'Mode:        %s\n' "$([[ "$DRY_RUN" == true ]] && echo dry-run || echo apply)"

rsync "${RSYNC_ARGS[@]}" "$SOURCE_REPO/" "$DEST_REPO/"

printf 'Sync complete.\n'
