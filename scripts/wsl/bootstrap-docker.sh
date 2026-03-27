#!/usr/bin/env bash
# bootstrap-docker.sh — Install and configure Docker CE natively in WSL2
#
# Handles the case where Docker Desktop WSL integration is not active:
#   - /usr/bin/docker is a broken symlink → removes it, installs docker.io
#   - docker.service not present → installs docker.io (systemd=true required)
#   - docker service exists but not running → starts + enables it
#   - current user not in docker group → adds user, reports re-login needed
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
      grep '^#' "$0" | sed 's/^# \{0,1\}//' | head -10
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

# ── 1. Check WSL systemd ─────────────────────────────────────────────────────

if ! grep -qs 'systemd=true' /etc/wsl.conf 2>/dev/null; then
  die "systemd is not enabled in /etc/wsl.conf. Add [boot] systemd=true and restart WSL."
fi

# ── 2. Detect current Docker state ──────────────────────────────────────────

DOCKER_BROKEN_LINK=false
DOCKER_RUNNING=false
DOCKER_INSTALLED=false

if [[ -L "/usr/bin/docker" ]] && [[ ! -e "/usr/bin/docker" ]]; then
  DOCKER_BROKEN_LINK=true
  warn "Broken symlink detected: /usr/bin/docker → $(readlink /usr/bin/docker)"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  DOCKER_RUNNING=true
  ok "Docker is already running."
elif command -v docker >/dev/null 2>&1; then
  DOCKER_INSTALLED=true
  info "docker binary present but daemon not reachable."
fi

if [[ "$DOCKER_RUNNING" == true ]]; then
  ok "Nothing to do — Docker is already functional."
  if [[ "$SKIP_TEST" == false ]] && [[ "$DRY_RUN" == false ]]; then
    docker run --rm hello-world 2>&1 | grep -E "Hello|error" || true
  fi
  exit 0
fi

# ── 3. Remove broken Docker Desktop symlink ──────────────────────────────────

if [[ "$DOCKER_BROKEN_LINK" == true ]]; then
  info "Removing broken Docker Desktop symlinks under /usr/bin/"
  run sudo rm -f /usr/bin/docker
  run sudo rm -f /usr/bin/docker-compose 2>/dev/null || true
  run sudo rm -f /usr/bin/docker-credential-desktop.exe 2>/dev/null || true
fi

# ── 4. Install docker.io if not installed ────────────────────────────────────

if [[ "$DRY_RUN" == true ]]; then
  printf '[dry-run] sudo apt-get install -y docker.io\n'
elif ! dpkg -s docker.io >/dev/null 2>&1; then
  info "Installing docker.io..."
  sudo apt-get update -qq
  sudo apt-get install -y docker.io
else
  ok "docker.io already installed ($(dpkg -s docker.io | grep Version | awk '{print $2}'))"
fi

# ── 5. Enable and start docker service ──────────────────────────────────────

if [[ "$DRY_RUN" == true ]]; then
  printf '[dry-run] sudo systemctl enable docker && sudo systemctl start docker\n'
else
  if ! systemctl is-enabled docker >/dev/null 2>&1; then
    info "Enabling docker.service..."
    sudo systemctl enable docker
  fi
  if ! systemctl is-active docker >/dev/null 2>&1; then
    info "Starting docker.service..."
    sudo systemctl start docker
  fi
  sleep 1
  if systemctl is-active docker >/dev/null 2>&1; then
    ok "docker.service is running."
  else
    die "docker.service failed to start. Run: sudo systemctl status docker"
  fi
fi

# ── 6. Add user to docker group ──────────────────────────────────────────────

CURRENT_USER="${USER:-$(id -un)}"
NEED_RELOGIN=false
if [[ "$DRY_RUN" == true ]]; then
  printf '[dry-run] sudo usermod -aG docker %s\n' "$CURRENT_USER"
elif ! groups "$CURRENT_USER" | grep -qw docker; then
  info "Adding $CURRENT_USER to docker group..."
  sudo usermod -aG docker "$CURRENT_USER"
  warn "Open a new shell (or run 'newgrp docker') for group membership to take effect."
  NEED_RELOGIN=true
else
  ok "$CURRENT_USER already in docker group."
fi

# ── 7. Smoke test ────────────────────────────────────────────────────────────

if [[ "$SKIP_TEST" == false ]] && [[ "$DRY_RUN" == false ]]; then
  if [[ "$NEED_RELOGIN" == true ]]; then
    info "Skipping hello-world test — open a new shell first, then: docker run --rm hello-world"
  else
    info "Running smoke test: docker run --rm hello-world"
    if docker run --rm hello-world 2>&1 | grep -q "Hello from Docker"; then
      ok "Smoke test passed."
    else
      warn "Unexpected output. Check manually: docker run --rm hello-world"
    fi
  fi
fi

echo ""
info "Docker bootstrap complete."
if [[ "$DRY_RUN" == false ]]; then
  info "  Binary:  $(command -v docker 2>/dev/null || echo 'not yet in PATH — open new shell')"
  info "  Version: $(docker --version 2>/dev/null || echo 'open new shell to check')"
  info "  Service: $(systemctl is-active docker 2>/dev/null || echo 'unknown')"
fi
