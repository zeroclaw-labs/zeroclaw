#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
MANIFEST="${WSL_LIB_MANIFEST:-$SCRIPT_DIR/lib-manifest.txt}"
DRY_RUN=false
FORCE=false
MODE_OVERRIDE=""

usage() {
  cat <<'HELP'
Bootstrap required external libraries/assets into WSL.

Usage:
  scripts/wsl/bootstrap-libs.sh [options]

Options:
  --manifest <path>     Override manifest file (default: scripts/wsl/lib-manifest.txt)
  --mode <symlink|copy> Override per-entry mode from manifest
  --force               Replace existing target paths
  --dry-run             Print actions without changing filesystem
  -h, --help            Show help

Manifest format:
  mode|source|target

Examples:
  scripts/wsl/bootstrap-libs.sh
  scripts/wsl/bootstrap-libs.sh --dry-run
  scripts/wsl/bootstrap-libs.sh --mode copy --force
HELP
}

log() {
  printf '%s\n' "$*"
}

warn() {
  printf 'WARN: %s\n' "$*" >&2
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

trim() {
  local s="$1"
  s="${s#${s%%[![:space:]]*}}"
  s="${s%${s##*[![:space:]]}}"
  printf '%s' "$s"
}

expand_tilde() {
  local path="$1"
  if [[ "$path" == "~" ]]; then
    printf '%s' "$HOME"
  elif [[ "$path" == ~/* ]]; then
    printf '%s/%s' "$HOME" "${path#~/}"
  else
    printf '%s' "$path"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest)
      MANIFEST="$2"
      shift 2
      ;;
    --mode)
      MODE_OVERRIDE="$2"
      shift 2
      ;;
    --force)
      FORCE=true
      shift
      ;;
    --dry-run)
      DRY_RUN=true
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

if [[ -n "$MODE_OVERRIDE" && "$MODE_OVERRIDE" != "symlink" && "$MODE_OVERRIDE" != "copy" ]]; then
  die "--mode must be 'symlink' or 'copy'"
fi

[[ -f "$MANIFEST" ]] || die "Manifest not found: $MANIFEST"

created=0
replaced=0
skipped=0
missing=0

while IFS='|' read -r raw_mode raw_source raw_target; do
  line="$(trim "${raw_mode:-}")"
  [[ -z "$line" ]] && continue
  [[ "${line:0:1}" == "#" ]] && continue

  mode="$(trim "$raw_mode")"
  source_path="$(expand_tilde "$(trim "${raw_source:-}")")"
  target_path="$(expand_tilde "$(trim "${raw_target:-}")")"

  [[ -n "$MODE_OVERRIDE" ]] && mode="$MODE_OVERRIDE"

  if [[ "$mode" != "symlink" && "$mode" != "copy" ]]; then
    warn "Skipping invalid mode '$mode' for source '$source_path'"
    skipped=$((skipped + 1))
    continue
  fi

  if [[ -z "$source_path" || -z "$target_path" ]]; then
    warn "Skipping malformed line in manifest"
    skipped=$((skipped + 1))
    continue
  fi

  if [[ ! -e "$source_path" ]]; then
    warn "Missing source: $source_path"
    missing=$((missing + 1))
    continue
  fi

  target_parent="$(dirname "$target_path")"
  if [[ "$DRY_RUN" == true ]]; then
    log "[dry-run] ensure dir: $target_parent"
  else
    mkdir -p "$target_parent"
  fi

  if [[ -e "$target_path" || -L "$target_path" ]]; then
    if [[ "$FORCE" == true ]]; then
      if [[ "$DRY_RUN" == true ]]; then
        log "[dry-run] remove existing target: $target_path"
      else
        rm -rf "$target_path"
      fi
      replaced=$((replaced + 1))
    else
      warn "Target exists, skipping (use --force): $target_path"
      skipped=$((skipped + 1))
      continue
    fi
  fi

  case "$mode" in
    symlink)
      if [[ "$DRY_RUN" == true ]]; then
        log "[dry-run] ln -s $source_path $target_path"
      else
        ln -s "$source_path" "$target_path"
      fi
      ;;
    copy)
      if [[ "$DRY_RUN" == true ]]; then
        log "[dry-run] cp -a $source_path $target_path"
      else
        cp -a "$source_path" "$target_path"
      fi
      ;;
  esac

  created=$((created + 1))
done < "$MANIFEST"

log "Done. created=$created replaced=$replaced skipped=$skipped missing=$missing"
