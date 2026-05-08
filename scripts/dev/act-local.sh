#!/usr/bin/env sh
# act-local.sh — discover and run GitHub Actions workflows locally via act.
#
# Powers the release-runbook "Step 3 — Dry-run the release workflows
# locally" instruction. Walks .github/workflows/, lets a maintainer pick
# a job (or --all), pre-fetches pinned action SHAs that act's shallow
# clone can't resolve, ensures .secrets exists, and threads
# --artifact-server-path plus a real GITHUB_TOKEN into every run. The
# token is exported into the environment so act resolves it via
# `-s GITHUB_TOKEN` (no token value lands in argv or process tables).
#
# Mutating release jobs (publish, docker push, external dispatch) are
# excluded from --all by default — act does not honor GitHub's
# environment-protection gates, so a successful local run with a real
# token could perform a real release. Use --include-mutating only when
# you have explicitly confirmed the workflow steps will not reach a
# mutation surface, or pass the explicit <wf>:<job> form (which always
# runs what you ask for).
#
# POSIX sh — no bash required. Works on dash, busybox ash, mksh.
#
# Usage:
#   ./scripts/dev/act-local.sh                       # interactive picker
#   ./scripts/dev/act-local.sh --list                # list discovered jobs
#   ./scripts/dev/act-local.sh <wf>:<job>            # explicit (e.g. release-stable-manual:web)
#   ./scripts/dev/act-local.sh <job>                 # short form (errors on collision)
#   ./scripts/dev/act-local.sh --all                 # every act-runnable job (mutating skipped)
#   ./scripts/dev/act-local.sh --all --include-mutating
#                                                    # combined: also runs publish/docker/dispatch
#   ./scripts/dev/act-local.sh --no-prefetch         # skip the SHA pre-fetch
#   ./scripts/dev/act-local.sh --help

set -eu

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ARTIFACT_DIR="${ACT_LOCAL_ARTIFACT_DIR:-/tmp/act-artifacts}"
ACT_CACHE_DIR="${HOME}/.cache/act"
PREFETCH=true
INCLUDE_MUTATING=false
# Resolved at setup time. Prefers a standalone `act` on PATH, falls
# back to `gh act` (the gh-act extension) — that's the install path
# the runbook recommends, so make sure it works without forcing a
# second download.
ACT_BIN=""

# Jobs that mutate external state when run with a real GITHUB_TOKEN —
# create GitHub releases, push container images, or dispatch to other
# repositories. act does NOT honor the environment-protection gates
# that guard these on real GitHub Actions, so reaching them locally
# with a real token can perform the real mutation. --all skips this
# list by default; --include-mutating opts back in (and the explicit
# <wf>:<job> form always runs what you ask for, on the assumption you
# meant it).
MUTATING_JOBS="\
release-stable-manual:publish
release-stable-manual:docker
release-stable-manual:redeploy-website"

log()  { printf '==> %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  sed -n '4,31p' "$0" | sed 's/^#//; s/^ //'
  exit 0
}

is_mutating_job() {
  printf '%s\n' "$MUTATING_JOBS" | grep -qx "$1"
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "$1 not found — install from $2"
}

# ── Setup ──────────────────────────────────────────────────────────

ensure_setup() {
  require_tool docker https://docs.docker.com/engine/install/
  require_tool gh     https://cli.github.com
  require_tool git    https://git-scm.com/

  if command -v act >/dev/null 2>&1; then
    ACT_BIN="act"
  elif gh extension list 2>/dev/null | grep -q 'gh act'; then
    ACT_BIN="gh act"
  else
    die "act not found. Install via: gh extension install nektos/gh-act
       or download a binary from https://nektosact.com/installation/"
  fi

  if [ ! -f "$REPO_ROOT/.secrets" ]; then
    log "creating .secrets (gitignored, empty by default)"
    : > "$REPO_ROOT/.secrets"
  fi

  mkdir -p "$ARTIFACT_DIR"
}

# ── Workflow + job discovery ───────────────────────────────────────

# Print a workflow file's job IDs, one per line, only if the workflow
# has a standalone trigger (push / pull_request / workflow_dispatch /
# schedule). workflow_call-only files are skipped — they need a parent
# invocation and aren't useful to run in isolation through act.
discover_workflow_jobs() {
  workflow_file="$1"
  if ! grep -qE '^[[:space:]]*(push|pull_request|workflow_dispatch|schedule):' \
       "$workflow_file"; then
    return
  fi
  # `act -W <file> -l` prints a header row plus one row per job. We
  # want column 2 (Job ID). $ACT_BIN may be unset when discover is
  # called from a context that doesn't pre-resolve (e.g. resolve_job
  # short-form lookup); fall back to plain `act` then.
  ${ACT_BIN:-act} -W "$workflow_file" -l 2>/dev/null \
    | awk 'NR > 1 && NF >= 2 && $2 != "" { print $2 }'
}

# Print every "<workflow-stem>:<job-id>" pair, grouped by workflow.
discover_jobs() {
  for workflow_file in "$REPO_ROOT"/.github/workflows/*.yml; do
    [ -f "$workflow_file" ] || continue
    stem=$(basename "$workflow_file" .yml)
    discover_workflow_jobs "$workflow_file" \
      | while IFS= read -r job; do
          [ -n "$job" ] && printf '%s:%s\n' "$stem" "$job"
        done
  done
}

list_jobs() {
  prev_stem=""
  discover_jobs | while IFS=: read -r stem job; do
    if [ "$stem" != "$prev_stem" ]; then
      [ -n "$prev_stem" ] && echo
      printf '%s:\n' "$stem"
      prev_stem="$stem"
    fi
    printf '  %s\n' "$job"
  done
}

resolve_job() {
  query="$1"
  case "$query" in
    *:*)
      # Explicit <workflow>:<job> — verify it exists.
      stem=${query%%:*}
      job=${query#*:}
      if discover_workflow_jobs "$REPO_ROOT/.github/workflows/$stem.yml" \
           2>/dev/null | grep -qx "$job"; then
        printf '%s\n' "$query"
        return 0
      fi
      die "no such job: $query (try --list)"
      ;;
    *)
      # Short form — must resolve to exactly one match.
      matches=$(discover_jobs | awk -F: -v q="$query" '$2 == q { print }')
      count=$(printf '%s\n' "$matches" | grep -c . || true)
      if [ "$count" = 0 ]; then
        die "no job named '$query' (try --list)"
      elif [ "$count" -gt 1 ]; then
        printf 'error: ambiguous job '\''%s'\'' — defined in:\n' \
          "$query" >&2
        printf '  %s\n' $matches >&2
        printf 'use <workflow>:<job> form, e.g. %s\n' \
          "$(printf '%s' "$matches" | head -1)" >&2
        exit 1
      fi
      printf '%s\n' "$matches"
      ;;
  esac
}

# ── Action SHA pre-fetch ───────────────────────────────────────────
#
# Extract every `uses: <owner>/<repo>@<sha>` line from the workflow
# files, dedupe, and pre-clone each into ~/.cache/act/<owner>-<repo>@<sha>.
# act's default shallow clone fails on arbitrary SHAs (we hit this live
# with dtolnay/rust-toolchain@631a55b1...). Idempotent: pre-fetch is a
# no-op when the cache dir already has action.yml.

prefetch_actions() {
  [ "$PREFETCH" = true ] || return 0

  mkdir -p "$ACT_CACHE_DIR"
  grep -hoE 'uses:[[:space:]]+[a-zA-Z0-9_./-]+@[a-f0-9]{40}' \
    "$REPO_ROOT"/.github/workflows/*.yml \
    | awk '{ print $2 }' \
    | sort -u \
    | while IFS=@ read -r action sha; do
        slug=$(printf '%s' "$action" | tr '/' '-')
        target="$ACT_CACHE_DIR/${slug}@${sha}"
        if [ -f "$target/action.yml" ] || [ -f "$target/action.yaml" ]; then
          continue
        fi
        short=$(printf '%s' "$sha" | cut -c1-7)
        log "pre-fetch ${action}@${short}"
        mkdir -p "$target"
        (
          cd "$target"
          if [ ! -d .git ]; then
            git init --quiet
            git remote add origin "https://github.com/${action}.git"
          fi
          git fetch --quiet --depth 1 origin "$sha"
          git checkout --quiet "$sha"
        ) || die "pre-fetch failed for ${action}@${short}"
      done
}

# ── Run a single job ───────────────────────────────────────────────

cargo_toml_version() {
  awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{
         split($0,a,"\""); print a[2]; exit }' \
    "$REPO_ROOT/Cargo.toml"
}

# Detect whether a workflow file has a `version:` workflow_dispatch
# input. If so, we'll auto-derive it from Cargo.toml.
workflow_has_version_input() {
  awk '
    /^on:/ { in_on=1; next }
    in_on && /^[a-z]/ { exit }
    in_on && /workflow_dispatch:/ { in_wd=1; next }
    in_wd && /^[[:space:]]+inputs:/ { in_inputs=1; next }
    in_inputs && /^[[:space:]]+version:/ { found=1; exit }
    in_inputs && /^[[:space:]]{0,4}[a-z]/ && !/^[[:space:]]+inputs:/ { in_inputs=0 }
    END { exit !found }
  ' "$1"
}

run_one() {
  pair="$1"
  stem=${pair%%:*}
  job=${pair#*:}
  workflow_file="$REPO_ROOT/.github/workflows/$stem.yml"
  [ -f "$workflow_file" ] || die "workflow file missing: $workflow_file"

  # Export the token into the environment so act resolves `-s
  # GITHUB_TOKEN` (no value) from getenv. Keeps the credential out of
  # argv, the shell history, and the kernel's process table.
  GITHUB_TOKEN=$(gh auth token)
  export GITHUB_TOKEN

  if is_mutating_job "$pair"; then
    log "WARNING: ${pair} is a mutating release job (publishes / pushes / dispatches)."
    log "         act does not honor environment-protection gates; a successful run"
    log "         with this token could create a real release. Continuing because"
    log "         you asked for this job explicitly."
  fi

  # Build the act command via positional params (POSIX sh has no arrays).
  set -- workflow_dispatch \
         -j "$job" \
         -W "$workflow_file" \
         -s GITHUB_TOKEN \
         --artifact-server-path "$ARTIFACT_DIR"

  if workflow_has_version_input "$workflow_file"; then
    version=$(cargo_toml_version)
    if [ -n "$version" ]; then
      set -- "$@" --input "version=$version"
    fi
  fi

  log "run ${stem}:${job}"
  $ACT_BIN "$@"
}

run_all() {
  if [ "$INCLUDE_MUTATING" = true ]; then
    log "running all act-runnable jobs (including mutating release jobs)"
  else
    log "running all act-runnable jobs (mutating release jobs skipped)"
  fi
  discover_jobs | while IFS= read -r pair; do
    [ -n "$pair" ] || continue
    if [ "$INCLUDE_MUTATING" != true ] && is_mutating_job "$pair"; then
      log "skip ${pair} (mutating; pass --include-mutating or run explicitly to override)"
      continue
    fi
    run_one "$pair"
  done
}

# ── Interactive picker ─────────────────────────────────────────────

interactive_pick() {
  pairs=$(discover_jobs)
  [ -n "$pairs" ] || die "no act-runnable jobs discovered"

  printf '%s\n' "Available jobs:" >&2
  printf '%s\n' "$pairs" \
    | awk '{ printf "  [%2d] %s\n", NR, $0 }' >&2
  printf '%s\n' "  [ 0] all" >&2
  printf '\n  pick a number: ' >&2
  read -r choice
  case "$choice" in
    0)         run_all; return ;;
    ''|*[!0-9]*) die "not a number: $choice" ;;
  esac
  selected=$(printf '%s\n' "$pairs" | awk -v n="$choice" 'NR == n')
  [ -n "$selected" ] || die "no job at index $choice"
  prefetch_actions
  run_one "$selected"
}

# ── Main ───────────────────────────────────────────────────────────

main() {
  cmd="${1:-}"
  case "$cmd" in
    -h|--help)
      usage
      ;;
  esac

  ensure_setup

  case "$cmd" in
    -l|--list)
      list_jobs
      ;;
    --no-prefetch)
      PREFETCH=false
      shift
      main "$@"
      ;;
    --include-mutating)
      INCLUDE_MUTATING=true
      shift
      main "$@"
      ;;
    -a|--all)
      prefetch_actions
      run_all
      ;;
    '')
      prefetch_actions
      interactive_pick
      ;;
    *)
      pair=$(resolve_job "$cmd")
      prefetch_actions
      run_one "$pair"
      ;;
  esac
}

main "$@"
