#!/usr/bin/env bash
set -euo pipefail

DRY_RUN=false
NO_BASHRC=false
WITH_DOCKER=false
PRIMARY_REPO="${ZEROCLAW_PRIMARY_REPO:-/home/lasve046/projects/zeroclaw-wsl}"
ARCHIVE_REPO="${ZEROCLAW_WIN_ARCHIVE_REPO:-/home/lasve046/pilot/zeroclaw-win-archive}"
CONFIG_DIR="${ZEROCLAW_WSL_CONFIG_DIR:-$HOME/.config/zeroclaw-wsl}"
ENV_FILE="$CONFIG_DIR/env.sh"
BASHRC_FILE="$HOME/.bashrc"
BIN_DIR="$HOME/.local/bin"
WRAPPER="$BIN_DIR/zcwsl"

usage() {
  cat <<'HELP'
Setup global WSL resources for ZeroClaw operations.

Usage:
  scripts/wsl/setup-global-resources.sh [options]

Options:
  --dry-run            Print planned actions without writing
  --no-bashrc          Do not update ~/.bashrc
  --with-docker        Run bootstrap-docker.sh after setup
  --primary <path>     Override WSL primary repo path
  --archive <path>     Override legacy archive repo path
  -h, --help           Show this help
HELP
}

log() {
  printf '%s\n' "$*"
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

ensure_dir() {
  local d="$1"
  if [[ "$DRY_RUN" == true ]]; then
    log "[dry-run] mkdir -p $d"
  else
    mkdir -p "$d"
  fi
}

write_env_file() {
  if [[ "$DRY_RUN" == true ]]; then
    log "[dry-run] write $ENV_FILE"
    return
  fi

  cat > "$ENV_FILE" <<EOT
# ZeroClaw WSL global resources
export ZEROCLAW_WSL_MODE=1
export ZEROCLAW_PRIMARY_REPO="$PRIMARY_REPO"
export ZEROCLAW_WIN_ARCHIVE_REPO="$ARCHIVE_REPO"
export ZEROCLAW_LIB_MANIFEST="$PRIMARY_REPO/scripts/wsl/lib-manifest.txt"
export PATH="$BIN_DIR:\$PATH"
alias zcroot='cd "$PRIMARY_REPO"'
alias zcarchive='cd "$ARCHIVE_REPO"'
alias zcproceed='"$PRIMARY_REPO/scripts/wsl/proceed.sh"'
EOT
}

write_wrapper_file() {
  if [[ "$DRY_RUN" == true ]]; then
    log "[dry-run] write $WRAPPER"
    return
  fi

  cat > "$WRAPPER" <<EOT
#!/usr/bin/env bash
set -euo pipefail
exec "$PRIMARY_REPO/scripts/wsl/proceed.sh" "\$@"
EOT
  chmod +x "$WRAPPER"
}

update_bashrc_block() {
  local marker_start="# >>> zeroclaw-wsl globals >>>"
  local marker_end="# <<< zeroclaw-wsl globals <<<"
  local block

  block="$marker_start
[ -f \"$ENV_FILE\" ] && . \"$ENV_FILE\"
$marker_end"

  if [[ "$DRY_RUN" == true ]]; then
    log "[dry-run] ensure source block in $BASHRC_FILE"
    return
  fi

  if grep -Fq "$marker_start" "$BASHRC_FILE"; then
    awk -v s="$marker_start" -v e="$marker_end" -v b="$block" '
      BEGIN {inblock=0}
      index($0,s)==1 {print b; inblock=1; next}
      index($0,e)==1 {inblock=0; next}
      inblock==0 {print}
    ' "$BASHRC_FILE" > "$BASHRC_FILE.tmp"
    mv "$BASHRC_FILE.tmp" "$BASHRC_FILE"
  else
    printf '\n%s\n' "$block" >> "$BASHRC_FILE"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --no-bashrc)
      NO_BASHRC=true
      shift
      ;;
    --with-docker)
      WITH_DOCKER=true
      shift
      ;;
    --primary)
      PRIMARY_REPO="$2"
      shift 2
      ;;
    --archive)
      ARCHIVE_REPO="$2"
      shift 2
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

[[ -d "$PRIMARY_REPO" ]] || die "Primary repo not found: $PRIMARY_REPO"
[[ -d "$ARCHIVE_REPO" ]] || die "Archive repo not found: $ARCHIVE_REPO"
[[ -f "$PRIMARY_REPO/scripts/wsl/lib-manifest.txt" ]] || die "Manifest missing in primary repo"
[[ -x "$PRIMARY_REPO/scripts/wsl/proceed.sh" ]] || die "Proceed script missing or not executable"

ensure_dir "$CONFIG_DIR"
ensure_dir "$BIN_DIR"
write_env_file
write_wrapper_file

if [[ "$NO_BASHRC" == false ]]; then
  update_bashrc_block
fi

log "Global WSL resources configured."
log "- env file: $ENV_FILE"
log "- wrapper:  $WRAPPER"
if [[ "$NO_BASHRC" == false ]]; then
  log "- bashrc source block: enabled"
else
  log "- bashrc source block: skipped (--no-bashrc)"
fi
log "Next: run 'source $ENV_FILE' in current shell or open a new terminal."

if [[ "$WITH_DOCKER" == true ]]; then
  log "Running Docker bootstrap..."
  bash "$PRIMARY_REPO/scripts/wsl/bootstrap-docker.sh" ${DRY_RUN:+--dry-run}
else
  log "Tip: run 'scripts/wsl/bootstrap-docker.sh' to install Docker CE natively."
fi
