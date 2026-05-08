#!/usr/bin/env bash
# bootstrap-pi-git.sh — clone or update QuantClaw on Raspberry Pi, then install it.

set -euo pipefail

APP_USER="${APP_USER:-quant}"
APP_HOME="${APP_HOME:-/home/${APP_USER}}"
APP_DIR="${APP_DIR:-${APP_HOME}/quantclaw_rust_app}"
REPO_DIR="${REPO_DIR:-${APP_DIR}/repo}"
REPO_URL="${REPO_URL:-https://gitea.tangledup-ai.com/Therianclouds/QuantClaw_Rust.git}"
REPO_BRANCH="${REPO_BRANCH:-master}"
RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-1.87.0}"
SWAPFILE_PATH="${SWAPFILE_PATH:-/swapfile_quantclaw}"
SWAPFILE_SIZE_MB="${SWAPFILE_SIZE_MB:-2048}"
APT_PACKAGES="${APT_PACKAGES:-build-essential pkg-config libssl-dev cmake clang curl ca-certificates git}"

die() {
  echo "ERROR: $*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Missing required command: $1"
}

ensure_swap() {
  local fstab_line="${SWAPFILE_PATH} none swap sw 0 0"

  echo "==> Ensuring swap space (${SWAPFILE_SIZE_MB} MiB)"
  if sudo swapon --show=NAME --noheadings | grep -Fxq "${SWAPFILE_PATH}"; then
    echo "    Swap already active at ${SWAPFILE_PATH}"
  else
    if [[ ! -f "${SWAPFILE_PATH}" ]]; then
      sudo fallocate -l "${SWAPFILE_SIZE_MB}M" "${SWAPFILE_PATH}" || \
        sudo dd if=/dev/zero of="${SWAPFILE_PATH}" bs=1M count="${SWAPFILE_SIZE_MB}" status=progress
      sudo chmod 600 "${SWAPFILE_PATH}"
      sudo mkswap "${SWAPFILE_PATH}"
    fi
    sudo swapon "${SWAPFILE_PATH}"
  fi

  if ! grep -Fq "${fstab_line}" /etc/fstab; then
    echo "    Persisting swap entry in /etc/fstab"
    echo "${fstab_line}" | sudo tee -a /etc/fstab >/dev/null
  fi

  free -h
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

ensure_system_packages() {
  echo "==> Installing system packages"
  sudo apt update
  # shellcheck disable=SC2086
  sudo apt install -y ${APT_PACKAGES}
}

ensure_rust_toolchain() {
  echo "==> Ensuring Rust toolchain ${RUST_TOOLCHAIN}"

  if [[ -x "${APP_HOME}/.cargo/bin/rustc" ]]; then
    . "${APP_HOME}/.cargo/env"
  fi

  if command -v rustc >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
    local current_version=""
    current_version="$(rustc --version 2>/dev/null | awk '{print $2}')"
    if [[ "${current_version}" == "${RUST_TOOLCHAIN}" ]]; then
      echo "    Rust already installed: ${current_version}"
      return
    fi
  fi

  rm -rf "${APP_HOME}/.rustup/toolchains/${RUST_TOOLCHAIN}-aarch64-unknown-linux-gnu"
  rm -rf "${APP_HOME}/.rustup/tmp"

  export RUSTUP_IO_THREADS="${RUSTUP_IO_THREADS:-1}"
  export RUSTUP_UNPACK_RAM="${RUSTUP_UNPACK_RAM:-67108864}"

  curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}"
  . "${APP_HOME}/.cargo/env"

  rustc --version
  cargo --version
}

require_cmd git
require_cmd sudo
require_cmd curl

echo "==> Bootstrapping QuantClaw from git"
echo "    Repo URL:    ${REPO_URL}"
echo "    Repo branch: ${REPO_BRANCH}"
echo "    App dir:     ${APP_DIR}"
echo "    Repo dir:    ${REPO_DIR}"
echo ""

cleanup_previous_attempts
ensure_swap
ensure_system_packages
ensure_rust_toolchain
sync_repo

echo ""
echo "==> Running installer from checked-out repository"
chmod +x "${REPO_DIR}/scripts/install-pi.sh"
"${REPO_DIR}/scripts/install-pi.sh" "${REPO_DIR}"
