#!/usr/bin/env bash
set -euo pipefail

PRIMARY_REPO="${ZEROCLAW_PRIMARY_REPO:-/home/lasve046/projects/zeroclaw-wsl}"
ARCHIVE_REPO="${ZEROCLAW_WIN_ARCHIVE_REPO:-/home/lasve046/pilot/zeroclaw-win-archive}"
ENV_FILE="${ZEROCLAW_WSL_CONFIG_DIR:-$HOME/.config/zeroclaw-wsl}/env.sh"
WRAPPER="$HOME/.local/bin/zcwsl"

failures=0

ok() {
  printf 'OK: %s\n' "$*"
}

warn() {
  printf 'WARN: %s\n' "$*"
}

check_path_exists() {
  local path="$1"
  local label="$2"
  if [[ -e "$path" ]]; then
    ok "$label exists: $path"
  else
    warn "$label missing: $path"
    failures=$((failures + 1))
  fi
}

check_cmd() {
  local c="$1"
  if command -v "$c" >/dev/null 2>&1; then
    ok "command available: $c"
  else
    warn "command missing: $c"
    failures=$((failures + 1))
  fi
}

check_docker() {
  # Detect broken Docker Desktop symlink (common after Docker Desktop WSL integration)
  if [[ -L "/usr/bin/docker" ]] && [[ ! -e "/usr/bin/docker" ]]; then
    warn "docker: broken symlink at /usr/bin/docker (Docker Desktop integration not active)"
    warn "  Fix: scripts/wsl/bootstrap-docker.sh"
    failures=$((failures + 1))
  elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    ok "docker available and daemon running"
  elif command -v docker >/dev/null 2>&1; then
    warn "docker binary present but daemon not reachable (service down?)"
    warn "  Fix: sudo systemctl start docker  OR  scripts/wsl/bootstrap-docker.sh"
    failures=$((failures + 1))
  else
    warn "docker not installed"
    warn "  Fix: scripts/wsl/bootstrap-docker.sh"
    failures=$((failures + 1))
  fi
}

check_cmd git
check_cmd rsync
check_docker

check_path_exists "$PRIMARY_REPO/.git" "primary repo"
check_path_exists "$ARCHIVE_REPO/.git" "archive repo"
check_path_exists "$ENV_FILE" "global env file"
check_path_exists "$WRAPPER" "global wrapper"
check_path_exists "$PRIMARY_REPO/scripts/wsl/proceed.sh" "proceed script"
check_path_exists "$PRIMARY_REPO/scripts/wsl/lib-manifest.txt" "libs manifest"

if [[ -x "$WRAPPER" ]]; then
  ok "wrapper executable"
else
  warn "wrapper not executable: $WRAPPER"
  failures=$((failures + 1))
fi

for link in \
  "$HOME/libs/bin/qf-site-navigator-rs" \
  "$HOME/libs/infra/qf_port_authority" \
  "$HOME/libs/docs/qf_bricks_buddy-kb"; do
  if [[ -L "$link" ]]; then
    ok "symlink present: $link -> $(readlink "$link")"
  elif [[ -e "$link" ]]; then
    ok "path present (non-link): $link"
  else
    warn "missing expected libs path: $link"
    failures=$((failures + 1))
  fi
done

if [[ "$failures" -eq 0 ]]; then
  echo "Global WSL resources doctor passed."
  exit 0
fi

echo "Global WSL resources doctor found $failures issue(s)."
exit 1
