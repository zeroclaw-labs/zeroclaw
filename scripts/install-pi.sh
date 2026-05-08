#!/usr/bin/env bash
# install-pi.sh — build and install QuantClaw on the Raspberry Pi.
#
# Supported inputs:
# - a checked-out repository directory
# - an archive containing either a binary bundle or a source tree
#
# Examples:
#   ./scripts/install-pi.sh /home/quant/quantclaw_rust_app/repo
#   ./scripts/install-pi.sh /tmp/quantclaw-rpi.tar.gz

set -euo pipefail

APP_USER="${APP_USER:-quant}"
APP_HOME="${APP_HOME:-/home/${APP_USER}}"
APP_DIR="${APP_DIR:-${APP_HOME}/quantclaw_rust_app}"
CONFIG_DIR="${CONFIG_DIR:-${APP_HOME}/.quantclaw}"
SERVICE_NAME="quantclaw"
SERVICE_DEST="/etc/systemd/system/${SERVICE_NAME}.service"
CONFIG_DEST="${CONFIG_DIR}/config.toml"
ENV_DEST="${APP_DIR}/.env"
GATEWAY_HOST="${GATEWAY_HOST:-0.0.0.0}"
GATEWAY_PORT="${GATEWAY_PORT:-42617}"
CHANNEL_WEBHOOK_PORT="${CHANNEL_WEBHOOK_PORT:-42618}"
BUILD_FEATURES="${BUILD_FEATURES:-hardware,peripheral-rpi}"
INPUT_PATH="${1:-${INPUT_PATH:-}}"
TMP_PARENT="${TMP_PARENT:-${APP_HOME}/.cache/quantclaw-install}"
TMP_DIR=""
SEARCH_ROOT=""
SOURCE_ROOT=""
BINARY_SOURCE=""
SERVICE_TEMPLATE=""
CONFIG_TEMPLATE=""
RULES_SOURCE=""

cleanup() {
  if [[ -n "${TMP_DIR}" && -d "${TMP_DIR}" ]]; then
    rm -rf "${TMP_DIR}"
  fi
}

die() {
  echo "ERROR: $*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Missing required command: $1"
}

ensure_tmp_dir() {
  if [[ -z "${TMP_DIR}" ]]; then
    mkdir -p "${TMP_PARENT}"
    TMP_DIR="$(mktemp -d "${TMP_PARENT}/install.XXXXXX")"
  fi
}

locate_first() {
  local search_root="$1"
  local pattern="$2"
  find "${search_root}" -type f -path "${pattern}" | head -n 1
}

resolve_source_root() {
  local search_root="$1"
  local manifest=""

  if [[ -f "${search_root}/Cargo.toml" ]]; then
    echo "${search_root}"
    return
  fi

  manifest="$(grep -Rsl '^\[workspace\]' "${search_root}" --include Cargo.toml | head -n 1 || true)"
  if [[ -n "${manifest}" ]]; then
    dirname "${manifest}"
    return
  fi

  manifest="$(grep -Rsl '^name = "quantspeed"' "${search_root}" --include Cargo.toml | head -n 1 || true)"
  if [[ -n "${manifest}" ]]; then
    dirname "${manifest}"
    return
  fi

  manifest="$(locate_first "${search_root}" "*/Cargo.toml")"
  [[ -n "${manifest}" ]] || die "Input contains neither a quantclaw binary nor a Cargo.toml source tree"
  dirname "${manifest}"
}

render_template() {
  local source="$1"
  local destination="$2"

  sed \
    -e "s|__RPI_USER__|${APP_USER}|g" \
    -e "s|__RPI_DIR__|${APP_DIR}|g" \
    -e "s|__RPI_HOME__|${APP_HOME}|g" \
    -e "s|__GATEWAY_HOST__|${GATEWAY_HOST}|g" \
    -e "s|__GATEWAY_PORT__|${GATEWAY_PORT}|g" \
    -e "s|__CHANNEL_WEBHOOK_PORT__|${CHANNEL_WEBHOOK_PORT}|g" \
    "${source}" > "${destination}"
}

resolve_input() {
  local input_path="$1"

  if [[ -d "${input_path}" ]]; then
    SEARCH_ROOT="${input_path}"
    SOURCE_ROOT="${input_path}"
    return
  fi

  [[ -f "${input_path}" ]] || die "Input path not found: ${input_path}"

  require_cmd tar
  ensure_tmp_dir
  tar -xf "${input_path}" -C "${TMP_DIR}"
  SEARCH_ROOT="${TMP_DIR}"
}

build_from_source() {
  local source_root="$1"

  require_cmd cargo
  require_cmd rustc

  echo ""
  echo "==> Building QuantClaw from source on Pi"
  echo "    Source:   ${source_root}"
  echo "    Features: ${BUILD_FEATURES}"

  (
    cd "${source_root}"
    cargo build --release --features "${BUILD_FEATURES}"
  )
}

trap cleanup EXIT

[[ -n "${INPUT_PATH}" ]] || die "Usage: ./scripts/install-pi.sh /path/to/repo-or-archive"

require_cmd sed
require_cmd grep
require_cmd install
require_cmd sudo
require_cmd systemctl

echo "==> Installing QuantClaw from ${INPUT_PATH}"
echo "    App dir: ${APP_DIR}"
echo "    Config:  ${CONFIG_DEST}"
echo "    Gateway: http://${GATEWAY_HOST}:${GATEWAY_PORT}"
echo ""

resolve_input "${INPUT_PATH}"

BINARY_SOURCE="$(locate_first "${SEARCH_ROOT}" "*/quantclaw")"
SERVICE_TEMPLATE="$(locate_first "${SEARCH_ROOT}" "*/scripts/quantclaw.service")"
CONFIG_TEMPLATE="$(locate_first "${SEARCH_ROOT}" "*/scripts/rpi-config.toml")"
RULES_SOURCE="$(locate_first "${SEARCH_ROOT}" "*/scripts/99-act-led.rules" || true)"

[[ -n "${SERVICE_TEMPLATE}" ]] || die "Input does not contain scripts/quantclaw.service"
[[ -n "${CONFIG_TEMPLATE}" ]] || die "Input does not contain scripts/rpi-config.toml"

if [[ -z "${BINARY_SOURCE}" ]]; then
  SOURCE_ROOT="$(resolve_source_root "${SEARCH_ROOT}")"
  build_from_source "${SOURCE_ROOT}"
  BINARY_SOURCE="${SOURCE_ROOT}/target/release/quantclaw"
fi

[[ -f "${BINARY_SOURCE}" ]] || die "Expected binary not found after extraction/build: ${BINARY_SOURCE}"

echo "==> Preparing directories"
mkdir -p "${APP_DIR}" "${CONFIG_DIR}"
ensure_tmp_dir

echo ""
echo "==> Installing binary"
install -m 0755 "${BINARY_SOURCE}" "${APP_DIR}/quantclaw"

if [[ ! -f "${ENV_DEST}" ]]; then
  echo ""
  echo "==> Creating ${ENV_DEST}"
  install -m 0600 /dev/null "${ENV_DEST}"
  cat > "${ENV_DEST}" <<'EOF'
# Set your provider credentials here.
ANTHROPIC_API_KEY=sk-ant-
EOF
fi

echo ""
echo "==> Rendering config"
EXISTING_API_KEY="$(grep -m1 '^api_key' "${CONFIG_DEST}" 2>/dev/null || true)"
TMP_CONFIG="${TMP_DIR}/config.toml"
render_template "${CONFIG_TEMPLATE}" "${TMP_CONFIG}"
if [[ -n "${EXISTING_API_KEY}" ]]; then
  sed -i "s|^# api_key = .*|${EXISTING_API_KEY}|" "${TMP_CONFIG}"
fi
install -m 0600 "${TMP_CONFIG}" "${CONFIG_DEST}"

echo ""
echo "==> Rendering systemd service"
TMP_SERVICE="${TMP_DIR}/quantclaw.service"
render_template "${SERVICE_TEMPLATE}" "${TMP_SERVICE}"
sudo install -m 0644 "${TMP_SERVICE}" "${SERVICE_DEST}"

echo ""
echo "==> Ensuring runtime permissions"
sudo usermod -aG gpio "${APP_USER}" || true
if [[ -n "${RULES_SOURCE}" ]]; then
  sudo install -m 0644 "${RULES_SOURCE}" /etc/udev/rules.d/99-act-led.rules
  sudo udevadm control --reload-rules || true
fi

echo ""
echo "==> Enabling service"
sudo systemctl daemon-reload
sudo systemctl enable --now "${SERVICE_NAME}"
sudo systemctl status "${SERVICE_NAME}" --no-pager || true

echo ""
echo "==> Install complete"
echo "    Binary:  ${APP_DIR}/quantclaw"
echo "    Config:  ${CONFIG_DEST}"
echo "    Service: ${SERVICE_DEST}"
if [[ -n "${SOURCE_ROOT}" ]]; then
  echo "    Source:  ${SOURCE_ROOT}"
fi
echo "    Health:  http://$(hostname -I | awk '{print $1}'):${GATEWAY_PORT}/health"
echo ""
echo "    Logs: sudo journalctl -u ${SERVICE_NAME} -f"
