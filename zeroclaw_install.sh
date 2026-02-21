#!/usr/bin/env sh
set -eu

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

run_privileged() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif have_cmd sudo; then
    sudo "$@"
  else
    echo "error: sudo is required to install missing dependencies." >&2
    exit 1
  fi
}

is_container_runtime() {
  if [ -f /.dockerenv ] || [ -f /run/.containerenv ]; then
    return 0
  fi

  if [ -r /proc/1/cgroup ] && grep -Eq '(docker|containerd|kubepods|podman|lxc)' /proc/1/cgroup; then
    return 0
  fi

  return 1
}

run_pacman() {
  if ! is_container_runtime; then
    run_privileged pacman "$@"
    return $?
  fi

  PACMAN_CFG_TMP="$(mktemp /tmp/zeroclaw-pacman.XXXXXX.conf)"
  cp /etc/pacman.conf "$PACMAN_CFG_TMP"
  if ! grep -Eq '^[[:space:]]*DisableSandboxSyscalls([[:space:]]|$)' "$PACMAN_CFG_TMP"; then
    printf '\nDisableSandboxSyscalls\n' >> "$PACMAN_CFG_TMP"
  fi

  if run_privileged pacman --config "$PACMAN_CFG_TMP" "$@"; then
    PACMAN_RC=0
  else
    PACMAN_RC=$?
  fi
  rm -f "$PACMAN_CFG_TMP"
  return "$PACMAN_RC"
}

ensure_bash() {
  if have_cmd bash; then
    return 0
  fi

  echo "==> bash not found; attempting to install it"
  if have_cmd apk; then
    run_privileged apk add --no-cache bash
  elif have_cmd apt-get; then
    run_privileged apt-get update -qq
    run_privileged apt-get install -y bash
  elif have_cmd dnf; then
    run_privileged dnf install -y bash
  elif have_cmd pacman; then
    run_pacman -Sy --noconfirm
    run_pacman -S --noconfirm --needed bash
  else
    echo "error: unsupported package manager; install bash manually and retry." >&2
    exit 1
  fi
}

ROOT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" >/dev/null 2>&1 && pwd || pwd)"
BOOTSTRAP_SCRIPT="$ROOT_DIR/scripts/bootstrap.sh"

if [ ! -f "$BOOTSTRAP_SCRIPT" ]; then
  echo "error: scripts/bootstrap.sh not found from repository root." >&2
  exit 1
fi

ensure_bash

if [ "$#" -eq 0 ]; then
  exec bash "$BOOTSTRAP_SCRIPT" --guided
fi

exec bash "$BOOTSTRAP_SCRIPT" "$@"
