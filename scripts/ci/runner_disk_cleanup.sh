#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/ci/runner_disk_cleanup.sh [options]

Safely reclaim disk space on self-hosted runner hosts.
Defaults to dry-run mode.

Options:
  --runner-root <path>          Runner root (default: $RUNNER_ROOT or /home/ubuntu/actions-runner-pool)
  --work-retention-days <n>     Keep workspace dirs newer than n days (default: 2)
  --diag-retention-days <n>     Keep diagnostic logs newer than n days (default: 7)
  --docker-prune                Include docker system prune -af --volumes
  --apply                       Execute deletions (default: dry-run)
  --force                       Allow apply even if runner worker/listener processes are detected
  -h, --help                    Show this help text
EOF
}

RUNNER_ROOT="${RUNNER_ROOT:-/home/ubuntu/actions-runner-pool}"
WORK_RETENTION_DAYS=2
DIAG_RETENTION_DAYS=7
DOCKER_PRUNE=false
APPLY=false
FORCE=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runner-root)
      RUNNER_ROOT="${2:-}"
      shift 2
      ;;
    --work-retention-days)
      WORK_RETENTION_DAYS="${2:-}"
      shift 2
      ;;
    --diag-retention-days)
      DIAG_RETENTION_DAYS="${2:-}"
      shift 2
      ;;
    --docker-prune)
      DOCKER_PRUNE=true
      shift
      ;;
    --apply)
      APPLY=true
      shift
      ;;
    --force)
      FORCE=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ! -d "$RUNNER_ROOT" ]]; then
  echo "Runner root does not exist: $RUNNER_ROOT" >&2
  exit 2
fi

if ! [[ "$WORK_RETENTION_DAYS" =~ ^[0-9]+$ ]]; then
  echo "Invalid --work-retention-days: $WORK_RETENTION_DAYS" >&2
  exit 2
fi
if ! [[ "$DIAG_RETENTION_DAYS" =~ ^[0-9]+$ ]]; then
  echo "Invalid --diag-retention-days: $DIAG_RETENTION_DAYS" >&2
  exit 2
fi

if [[ "$APPLY" == true && "$FORCE" != true ]]; then
  if pgrep -fa 'Runner\.Worker|Runner\.Listener' >/dev/null 2>&1; then
    echo "Active runner processes detected. Re-run with --force only after draining jobs." >&2
    exit 3
  fi
fi

collect_candidates() {
  local list_file="$1"
  : > "$list_file"

  # Old diagnostic logs.
  find "$RUNNER_ROOT" -type f -path '*/_diag/*' -mtime +"$DIAG_RETENTION_DAYS" -print 2>/dev/null >> "$list_file" || true

  # Stale temp artifacts.
  find "$RUNNER_ROOT" -type f -path '*/_work/_temp/*' -mtime +1 -print 2>/dev/null >> "$list_file" || true
  find "$RUNNER_ROOT" -type d -path '*/_work/_temp/*' -mtime +1 -print 2>/dev/null >> "$list_file" || true

  # Stale repository workspaces under _work (exclude internal underscore dirs).
  find "$RUNNER_ROOT" -mindepth 3 -maxdepth 3 -type d -path '*/_work/*' ! -name '_*' -mtime +"$WORK_RETENTION_DAYS" -print 2>/dev/null >> "$list_file" || true

  sort -u -o "$list_file" "$list_file"
}

human_bytes() {
  local bytes="$1"
  awk -v b="$bytes" '
    function human(x) {
      s="B KiB MiB GiB TiB PiB"
      split(s, a, " ")
      i=1
      while (x>=1024 && i<6) {x/=1024; i++}
      return sprintf("%.2f %s", x, a[i])
    }
    BEGIN { print human(b) }
  '
}

CANDIDATES_FILE="$(mktemp)"
trap 'rm -f "$CANDIDATES_FILE"' EXIT
collect_candidates "$CANDIDATES_FILE"

TOTAL_BYTES=0
COUNT=0
while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  if [[ ! -e "$path" ]]; then
    continue
  fi
  COUNT=$((COUNT + 1))
done < "$CANDIDATES_FILE"

if [[ "$COUNT" -gt 0 ]]; then
  TOTAL_BYTES="$(tr '\n' '\0' < "$CANDIDATES_FILE" | xargs -0 -r du -sb 2>/dev/null | awk '{s+=$1} END{print s+0}')"
fi

echo "Runner root: $RUNNER_ROOT"
echo "Mode: $([[ "$APPLY" == true ]] && echo apply || echo dry-run)"
echo "Retention: workspace>${WORK_RETENTION_DAYS}d diag>${DIAG_RETENTION_DAYS}d"
echo "Candidates: $COUNT"
echo "Estimated reclaim: $(human_bytes "$TOTAL_BYTES")"

if [[ "$COUNT" -gt 0 ]]; then
  echo "Sample candidates:"
  sed -n '1,20p' "$CANDIDATES_FILE"
  if [[ "$COUNT" -gt 20 ]]; then
    echo "... ($((COUNT - 20)) more)"
  fi
fi

if [[ "$APPLY" != true ]]; then
  echo "Dry-run only. Re-run with --apply to execute cleanup."
  exit 0
fi

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  if [[ -e "$path" ]]; then
    rm -rf "$path"
  fi
done < "$CANDIDATES_FILE"

if [[ "$DOCKER_PRUNE" == true ]]; then
  if command -v docker >/dev/null 2>&1; then
    docker system prune -af --volumes || true
  else
    echo "docker command not found; skipping docker prune." >&2
  fi
fi

echo "Cleanup completed."
