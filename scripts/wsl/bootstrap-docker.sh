#!/usr/bin/env bash
# bootstrap-docker.sh — Install and configure Docker CE natively in WSL2
#
# Handles the case where Docker Desktop WSL integration is not active:
#   - If user is in docker group + broken symlink → enable Docker Desktop WSL integration
#   - If sudo available → removes broken symlink, installs docker.io natively
#   - Adds user to docker group if missing (requires sudo)
#
# Usage:
#   scripts/wsl/bootstrap-docker.sh [--dry-run] [--skip-test]

set -euo pipefail

DRY_RUN=false
SKIP_TEST=false

for arg in "$@"; do
  case "$arg" in
    --dry-run)   DRY_RUN=true ;;
    --skip-test) SKIP_TEST=true ;;
    --help|-h)
      grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -12
      exit 0
      ;;
  esac
done

info()  { printf '[docker-bootstrap] %s\n' "$*"; }
ok()    { printf '[docker-bootstrap] OK: %s\n' "$*"; }
warn()  { printf '[docker-bootstrap] WARN: %s\n' "$*"; }
die()   { printf '[docker-bootstrap] ERROR: %s\n' "$*" >&2; exit 1; }

run() {
  if [[ "$DRY_RUN" == true ]]; then
    printf '[dry-run] %s\n' "$*"
  else
    "$@"
  fi
}

HAS_SUDO=false
if sudo -n true 2>/dev/null; then
  HAS_SUDO=true
fi

IN_DOCKER_GROUP=false
if groups | grep -qw docker; then
  IN_DOCKER_GROUP=true
fi

# ── 1. Check if Docker is already running ────────────────────────────────────

if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  ok "Docker is already running."
  if [[ "$SKIP_TEST" == false ]] && [[ "$DRY_RUN" == false ]]; then
    docker run --rm hello-world 2>&1 | grep -E "Hello|error" || true
  fi
  exit 0
fi

# ── 2. Detect Docker Desktop broken symlink ──────────────────────────────────

DOCKER_BROKEN_LINK=false
if [[ -L "/usr/bin/docker" ]] && [[ ! -e "/usr/bin/docker" ]]; then
  DOCKER_BROKEN_LINK=true
  warn "Broken symlink: /usr/bin/docker → $(readlink /usr/bin/docker)"
fi

# ── 3. Docker Desktop integration path (no sudo needed) ─────────────────────

if [[ "$DOCKER_BROKEN_LINK" == true ]] && [[ "$IN_DOCKER_GROUP" == true ]] && [[ "$HAS_SUDO" == false ]]; then
  echo ""
  echo "┌─────────────────────────────────────────────────────────────────────────┐"
  echo "│  Docker Desktop WSL Integration — Action Required                       │"
  echo "│                                                                         │"
  echo "│  You are already in the 'docker' group. Docker Desktop is installed.   │"
  echo "│  The broken symlink will resolve once WSL integration is enabled.      │"
  echo "│                                                                         │"
  echo "│  On Windows:                                                            │"
  echo "│    1. Open Docker Desktop                                               │"
  echo "│    2. Settings → Resources → WSL Integration                           │"
  echo "│    3. Enable toggle for: $(uname -n) / Ubuntu                          │"
  echo "│    4. Click Apply & Restart                                             │"
  echo "│                                                                         │"
  echo "│  Then run: docker run --rm hello-world                                 │"
  echo "└─────────────────────────────────────────────────────────────────────────┘"
  echo ""
  exit 0
fi

# ── 4. Native install path (requires sudo) ───────────────────────────────────

if [[ "$HAS_SUDO" == false ]]; then
  die "No sudo access and Docker Desktop integration not the issue. Cannot proceed automatically."
fi

if ! grep -qs 'systemd=true' /etc/wsl.conf 2>/dev/null; then
  die "systemd is not enabled in /etc/wsl.conf. Add [boot] systemd=true and restart WSL."
fi

if [[ "$DOCKER_BROKEN_LINK" == true ]]; then
  info "Removing broken Docker Desktop symlinks under /usr/bin/"
  run sudo rm -f /usr/bin/docker
  run sudo rm -f /usr/bin/docker-compose 2>/dev/null || true
  run sudo rm -f /usr/bin/docker-credential-desktop.exe 2>/dev/null || true
fi

if [[ "$DRY_RUN" == true ]]; then
  printf '[dry-run] sudo apt-get install -y docker.io\n'
elif ! dpkg -s docker.io >/dev/null 2>&1; then
  info "Installing docker.io..."
  sudo apt-get update -qq
  sudo apt-get install -y docker.io
else
  ok "docker.io already installed ($(dpkg -s docker.io | grep Version | awk '{print $2}'))"
fi

if [[ "$DRY_RUN" == true ]]; then
  printf '[dry-run] sudo systemctl enable docker && sudo systemctl start docker\n'
else
  systemctl is-enabled docker >/dev/null 2>&1 || sudo systemctl enable docker
  systemctl is-active docker >/dev/null 2>&1 || sudo systemctl start docker
  sleep 1
  systemctl is-active docker >/dev/null 2>&1 || die "docker.service failed to start. Run: sudo systemctl status docker"
  ok "docker.service is running."
fi

CURRENT_USER="${USER:-$(id -un)}"
NEED_RELOGIN=false
if [[ "$DRY_RUN" == true ]]; then
  printf '[dry-run] sudo usermod -aG docker %s\n' "$CURRENT_USER"
elif ! groups "$CURRENT_USER" | grep -qw docker; then
  run sudo usermod -aG docker "$CURRENT_USER"
  warn "Open a new shell (or run 'newgrp docker') for group membership to take effect."
  NEED_RELOGIN=true
else
  ok "$CURRENT_USER already in docker group."
fi

if [[ "$SKIP_TEST" == false ]] && [[ "$DRY_RUN" == false ]]; then
  if [[ "$NEED_RELOGIN" == true ]]; then
    info "Skipping smoke test — open new shell first, then: docker run --rm hello-world"
  else
    info "Running smoke test: docker run --rm hello-world"
    docker run --rm hello-world 2>&1 | grep -q "Hello from Docker" && ok "Smoke test passed." || warn "Check manually: docker run --rm hello-world"
  fi
fi

echo ""
info "Docker bootstrap complete."
if [[ "$DRY_RUN" == false ]]; then
  info "  Binary:  $(command -v docker 2>/dev/null || echo 'open new shell')"
  info "  Version: $(docker --version 2>/dev/null || echo 'open new shell')"
  info "  Service: $(systemctl is-active docker 2>/dev/null || echo 'unknown')"
fi
