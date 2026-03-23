#!/usr/bin/env bash
# cargo-clean-stale.sh — Prune stale build artifacts to reclaim disk space.
#
# Safe to run at any time; only removes caches that cargo rebuilds on demand.
# Designed to run as a daily cron job or post-branch-finish hook.
#
# Usage:
#   ./dev/cargo-clean-stale.sh          # default: clean this repo
#   ./dev/cargo-clean-stale.sh --dry-run  # show what would be removed
#   CARGO_STALE_DAYS=3 ./dev/cargo-clean-stale.sh  # custom age threshold

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${REPO_ROOT}/target"
DRY_RUN="${1:-}"
STALE_DAYS="${CARGO_STALE_DAYS:-7}"

log() { echo "[cargo-clean] $*"; }

if [[ ! -d "$TARGET_DIR" ]]; then
    log "No target directory found at ${TARGET_DIR}, nothing to do."
    exit 0
fi

before=$(du -sk "$TARGET_DIR" 2>/dev/null | awk '{print $1}')

# 1. Wipe incremental compilation caches (rebuilt in seconds on next compile).
#    These are the biggest offender: 17G+ for debug alone.
for profile in debug release; do
    inc_dir="${TARGET_DIR}/${profile}/incremental"
    if [[ -d "$inc_dir" ]]; then
        size=$(du -sh "$inc_dir" 2>/dev/null | awk '{print $1}')
        if [[ "$DRY_RUN" == "--dry-run" ]]; then
            log "[dry-run] Would remove ${inc_dir} (${size})"
        else
            rm -rf "$inc_dir"
            log "Removed ${profile}/incremental (${size})"
        fi
    fi
done

# 2. Prune stale .o/.d/.rmeta files in debug/deps older than STALE_DAYS.
deps_dir="${TARGET_DIR}/debug/deps"
if [[ -d "$deps_dir" ]]; then
    stale_count=$(find "$deps_dir" -type f \( -name '*.o' -o -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) -mtime +"$STALE_DAYS" 2>/dev/null | wc -l | tr -d ' ')
    if [[ "$stale_count" -gt 0 ]]; then
        if [[ "$DRY_RUN" == "--dry-run" ]]; then
            stale_size=$(find "$deps_dir" -type f \( -name '*.o' -o -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) -mtime +"$STALE_DAYS" -exec du -ck {} + 2>/dev/null | tail -1 | awk '{print $1}')
            log "[dry-run] Would prune ${stale_count} stale dep files (${stale_size}K) older than ${STALE_DAYS}d"
        else
            find "$deps_dir" -type f \( -name '*.o' -o -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) -mtime +"$STALE_DAYS" -delete 2>/dev/null
            log "Pruned ${stale_count} stale dep files older than ${STALE_DAYS}d from debug/deps"
        fi
    fi
fi

# 3. Clean worktree targets that no longer have an active worktree.
worktree_dir="${REPO_ROOT}/.claude/worktrees"
if [[ -d "$worktree_dir" ]]; then
    for wt in "$worktree_dir"/*/; do
        wt_target="${wt}target"
        if [[ -d "$wt_target" ]]; then
            wt_name=$(basename "$wt")
            size=$(du -sh "$wt_target" 2>/dev/null | awk '{print $1}')
            # Check if the worktree is still registered with git
            if ! git -C "$REPO_ROOT" worktree list --porcelain 2>/dev/null | grep -q "$wt_name"; then
                if [[ "$DRY_RUN" == "--dry-run" ]]; then
                    log "[dry-run] Would remove orphaned worktree target ${wt_name} (${size})"
                else
                    rm -rf "$wt_target"
                    log "Removed orphaned worktree target ${wt_name} (${size})"
                fi
            else
                # Active worktree — still clean incremental caches
                for profile in debug release; do
                    wt_inc="${wt_target}/${profile}/incremental"
                    if [[ -d "$wt_inc" ]]; then
                        inc_size=$(du -sh "$wt_inc" 2>/dev/null | awk '{print $1}')
                        if [[ "$DRY_RUN" == "--dry-run" ]]; then
                            log "[dry-run] Would remove worktree ${wt_name} ${profile}/incremental (${inc_size})"
                        else
                            rm -rf "$wt_inc"
                            log "Removed worktree ${wt_name} ${profile}/incremental (${inc_size})"
                        fi
                    fi
                done
            fi
        fi
    done
fi

# 4. Clean flycheck artifacts (rust-analyzer background checks).
flycheck_dir="${TARGET_DIR}/flycheck0"
if [[ -d "$flycheck_dir" ]]; then
    size=$(du -sh "$flycheck_dir" 2>/dev/null | awk '{print $1}')
    if [[ "$DRY_RUN" == "--dry-run" ]]; then
        log "[dry-run] Would remove flycheck0 (${size})"
    else
        rm -rf "$flycheck_dir"
        log "Removed flycheck0 (${size})"
    fi
fi

after=$(du -sk "$TARGET_DIR" 2>/dev/null | awk '{print $1}')
saved=$(( (before - after) / 1024 ))
log "Done. Reclaimed ~${saved}MB (${before}K -> ${after}K)"
