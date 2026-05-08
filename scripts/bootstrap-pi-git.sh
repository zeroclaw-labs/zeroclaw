#!/usr/bin/env bash
# bootstrap-pi-git.sh — clone or update QuantClaw on Raspberry Pi, then install it.

set -euo pipefail

APP_USER="${APP_USER:-quant}"
APP_HOME="${APP_HOME:-/home/${APP_USER}}"
APP_DIR="${APP_DIR:-${APP_HOME}/quantclaw_rust_app}"
REPO_DIR="${REPO_DIR:-${APP_DIR}/repo}"
REPO_URL="${REPO_URL:-https://gitea.tangledup-ai.com/Therianclouds/QuantClaw_Rust.git}"
REPO_BRANCH="${REPO_BRANCH:-master}"

die() {
  echo "ERROR: $*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Missing required command: $1"
}

cleanup_previous_attempts() {
  echo "==> Cleaning temporary deployment artifacts"
  rm -f /tmp/quantclaw-rpi*.tar.gz /tmp/install-pi.sh
  rm -rf "${APP_HOME}/.cache/quantclaw-install"
}

sync_repo() {
  mkdir -p "${APP_DIR}"

  if [[ -d "${REPO_DIR}/.git" ]]; then
    echo "==> Updating existing repository in ${REPO_DIR}"
    git -C "${REPO_DIR}" fetch --all --prune
    git -C "${REPO_DIR}" checkout "${REPO_BRANCH}"
    git -C "${REPO_DIR}" pull --ff-only origin "${REPO_BRANCH}"
  elif [[ -e "${REPO_DIR}" ]]; then
    die "Repo path exists but is not a git checkout: ${REPO_DIR}"
  else
    echo "==> Cloning ${REPO_URL} (${REPO_BRANCH}) into ${REPO_DIR}"
    git clone --branch "${REPO_BRANCH}" --single-branch "${REPO_URL}" "${REPO_DIR}"
  fi
}

require_cmd git

echo "==> Bootstrapping QuantClaw from git"
echo "    Repo URL:    ${REPO_URL}"
echo "    Repo branch: ${REPO_BRANCH}"
echo "    App dir:     ${APP_DIR}"
echo "    Repo dir:    ${REPO_DIR}"
echo ""

cleanup_previous_attempts
sync_repo

echo ""
echo "==> Running installer from checked-out repository"
chmod +x "${REPO_DIR}/scripts/install-pi.sh"
"${REPO_DIR}/scripts/install-pi.sh" "${REPO_DIR}"
