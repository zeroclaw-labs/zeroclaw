#!/usr/bin/env bash
set -euo pipefail

PURGE_DATA=0
YES=0
DRY_RUN=0

print_usage() {
  cat <<'USAGE'
Usage: ./zeroclaw_uninstall.sh [options]

Options:
  --purge-data   Remove runtime data/config directories (destructive)
  -y, --yes      Non-interactive mode (skip confirmation)
  --dry-run      Print actions without executing
  -h, --help     Show this help
USAGE
}

log() {
  printf '%s\n' "$*"
}

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

resolve_cmd() {
  command -v "$1" 2>/dev/null || true
}

run_cmd() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf '[dry-run] %q' "$1"
    shift || true
    for arg in "$@"; do
      printf ' %q' "$arg"
    done
    printf '\n'
    return 0
  fi
  "$@"
}

run_sudo_if_needed() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    if [[ "$(id -u)" -eq 0 ]]; then
      run_cmd "$@"
    elif have_cmd sudo; then
      run_cmd sudo "$@"
    else
      printf '[dry-run] (need sudo) '
      printf '%q ' "$@"
      printf '\n'
    fi
    return 0
  fi

  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  elif have_cmd sudo; then
    sudo "$@"
  else
    log "warning: sudo not found, skipped: $*"
  fi
}

remove_file_if_exists() {
  local path="$1"
  if [[ -e "$path" || -L "$path" ]]; then
    run_cmd rm -f "$path"
  fi
}

remove_dir_if_exists() {
  local path="$1"
  if [[ -d "$path" ]]; then
    run_cmd rm -rf "$path"
  fi
}

for arg in "$@"; do
  case "$arg" in
    --purge-data)
      PURGE_DATA=1
      ;;
    -y|--yes)
      YES=1
      ;;
    --dry-run)
      DRY_RUN=1
      ;;
    -h|--help)
      print_usage
      exit 0
      ;;
    *)
      log "error: unknown option: $arg"
      print_usage
      exit 2
      ;;
  esac
done

if [[ "$YES" -eq 0 ]]; then
  log "This will uninstall ZeroClaw service/binary."
  if [[ "$PURGE_DATA" -eq 1 ]]; then
    log "Data purge is enabled: ~/.zeroclaw (and system OpenRC dirs if present) will be removed."
  fi
  read -r -p "Continue? [y/N] " answer
  case "$answer" in
    y|Y|yes|YES)
      ;;
    *)
      log "Cancelled."
      exit 0
      ;;
  esac
fi

ZEROCLAW_BIN="$(resolve_cmd zeroclaw)"

log "[1/4] Uninstalling service (if installed)..."
if [[ -n "$ZEROCLAW_BIN" && -x "$ZEROCLAW_BIN" ]]; then
  run_cmd "$ZEROCLAW_BIN" service stop || true
  run_cmd "$ZEROCLAW_BIN" service uninstall || true
  # OpenRC actions are only relevant when OpenRC tooling exists.
  if have_cmd rc-service || have_cmd rc-update; then
    run_sudo_if_needed "$ZEROCLAW_BIN" service --service-init openrc stop || true
    run_sudo_if_needed "$ZEROCLAW_BIN" service --service-init openrc uninstall || true
  fi
fi

if have_cmd systemctl; then
  run_cmd systemctl --user stop zeroclaw.service || true
  run_cmd systemctl --user disable zeroclaw.service || true
  remove_file_if_exists "$HOME/.config/systemd/user/zeroclaw.service"
  run_cmd systemctl --user daemon-reload || true
fi

if have_cmd rc-service; then
  run_sudo_if_needed rc-service zeroclaw stop || true
fi
if have_cmd rc-update; then
  run_sudo_if_needed rc-update del zeroclaw default || true
fi
run_sudo_if_needed rm -f /etc/init.d/zeroclaw || true

log "[2/4] Removing binary..."
if have_cmd cargo; then
  run_cmd cargo uninstall zeroclaw || true
fi
remove_file_if_exists "$HOME/.cargo/bin/zeroclaw"
run_sudo_if_needed rm -f /usr/local/bin/zeroclaw || true
run_sudo_if_needed rm -f /usr/bin/zeroclaw || true

log "[3/4] Cleaning optional data..."
if [[ "$PURGE_DATA" -eq 1 ]]; then
  remove_dir_if_exists "$HOME/.zeroclaw"
  run_sudo_if_needed rm -rf /etc/zeroclaw || true
  run_sudo_if_needed rm -rf /var/log/zeroclaw || true
else
  log "Skipped data purge (use --purge-data to remove ~/.zeroclaw)."
fi

log "[4/4] Verifying..."
hash -r 2>/dev/null || true
declare -a remaining_bins=()
for path in "$HOME/.cargo/bin/zeroclaw" "/usr/local/bin/zeroclaw" "/usr/bin/zeroclaw"; do
  if [[ -e "$path" || -L "$path" ]]; then
    remaining_bins+=("$path")
  fi
done

if [[ "${#remaining_bins[@]}" -gt 0 ]]; then
  log "warning: zeroclaw binary still exists at:"
  for path in "${remaining_bins[@]}"; do
    log "  - $path"
  done
  exit 1
fi

log "Done. ZeroClaw uninstall completed."
