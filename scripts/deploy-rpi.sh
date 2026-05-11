#!/usr/bin/env bash
# deploy-rpi.sh — cross-compile QuantClaw for Raspberry Pi and deploy via SSH.
#
# Cross-compilation (pick ONE — the script auto-detects):
#
#   Option A — cargo-zigbuild (recommended; works on Apple Silicon + Intel, no Docker)
#     brew install zig
#     cargo install cargo-zigbuild
#     rustup target add aarch64-unknown-linux-gnu
#
#   Option B — cross (Docker-based; requires Docker Desktop running)
#     cargo install cross
#
# Usage:
#   RPI_HOST=raspberrypi.local RPI_USER=pi ./scripts/deploy-rpi.sh
#
# Optional env vars:
#   RPI_HOST        — hostname or IP of the Pi        (default: raspberrypi.local)
#   RPI_USER        — SSH user on the Pi              (default: pi)
#   RPI_PORT        — SSH port                        (default: 22)
#   RPI_DIR         — remote deployment dir           (default: /home/$RPI_USER/quantclaw_rust_app)
#   SERVICE_NAME    — systemd unit name               (default: quantclaw-rust)
#   RPI_PASS        — SSH password (uses sshpass)     (default: prompt interactively)
#   CROSS_TOOL      — force "zigbuild" or "cross"     (default: auto-detect)

set -euo pipefail

RPI_HOST="${RPI_HOST:-raspberrypi.local}"
RPI_USER="${RPI_USER:-pi}"
RPI_PORT="${RPI_PORT:-22}"
RPI_DIR="${RPI_DIR:-/home/${RPI_USER}/quantclaw_rust_app}"
SERVICE_NAME="${SERVICE_NAME:-quantclaw-rust}"
TARGET="aarch64-unknown-linux-gnu"
FEATURES="hardware,peripheral-rpi"
BINARY="target/${TARGET}/release/quantclaw"
SSH_OPTS="-p ${RPI_PORT} -o StrictHostKeyChecking=no -o ConnectTimeout=10"
# scp uses -P (uppercase) for port; ssh uses -p (lowercase)
SCP_OPTS="-P ${RPI_PORT} -o StrictHostKeyChecking=no -o ConnectTimeout=10"

# If RPI_PASS is set, wrap ssh/scp with sshpass for non-interactive auth.
SSH_CMD="ssh"
SCP_CMD="scp"
if [[ -n "${RPI_PASS:-}" ]]; then
  if ! command -v sshpass &>/dev/null; then
    echo "ERROR: RPI_PASS is set but sshpass is not installed."
    echo "  brew install hudochenkov/sshpass/sshpass"
    exit 1
  fi
  SSH_CMD="sshpass -p ${RPI_PASS} ssh"
  SCP_CMD="sshpass -p ${RPI_PASS} scp"
fi

echo "==> Building QuantClaw for Raspberry Pi (${TARGET})"
echo "    Features: ${FEATURES}"
echo "    Target host: ${RPI_USER}@${RPI_HOST}:${RPI_PORT}"
echo "    Service: ${SERVICE_NAME}"
echo ""

# ── 1. Cross-compile — auto-detect best available tool ───────────────────────
# Prefer cargo-zigbuild: it works on Apple Silicon without Docker and avoids
# the rustup-toolchain-install errors that affect cross v0.2.x on arm64 Macs.
_detect_cross_tool() {
  if [[ "${CROSS_TOOL:-}" == "cross" ]]; then
    echo "cross"; return
  fi
  if [[ "${CROSS_TOOL:-}" == "zigbuild" ]]; then
    echo "zigbuild"; return
  fi
  if command -v cargo-zigbuild &>/dev/null && command -v zig &>/dev/null; then
    echo "zigbuild"; return
  fi
  if command -v cross &>/dev/null; then
    echo "cross"; return
  fi
  echo "none"
}

TOOL=$(_detect_cross_tool)

case "${TOOL}" in
  zigbuild)
    echo "==> Using cargo-zigbuild (Zig cross-linker)"
    # Ensure the target sysroot is registered with rustup.
    rustup target add "${TARGET}" 2>/dev/null || true
    cargo zigbuild \
      --target "${TARGET}" \
      --features "${FEATURES}" \
      --release
    ;;
  cross)
    echo "==> Using cross (Docker-based)"
    # Verify Docker is running before handing off — gives a clear error message
    # instead of the confusing rustup-toolchain failure from cross v0.2.x.
    if ! docker info &>/dev/null; then
      echo ""
      echo "ERROR: Docker is not running."
      echo "  Start Docker Desktop and retry, or install cargo-zigbuild instead:"
      echo "    brew install zig && cargo install cargo-zigbuild"
      echo "    rustup target add ${TARGET}"
      exit 1
    fi
    cross build \
      --target "${TARGET}" \
      --features "${FEATURES}" \
      --release
    ;;
  none)
    echo ""
    echo "ERROR: No cross-compilation tool found."
    echo ""
    echo "Install one of the following and retry:"
    echo ""
    echo "  Option A — cargo-zigbuild (recommended; works on Apple Silicon, no Docker):"
    echo "    brew install zig"
    echo "    cargo install cargo-zigbuild"
    echo "    rustup target add ${TARGET}"
    echo ""
    echo "  Option B — cross (requires Docker Desktop running):"
    echo "    cargo install cross"
    echo ""
    exit 1
    ;;
esac

echo ""
echo "==> Build complete: ${BINARY}"
ls -lh "${BINARY}"

# ── 2. Stop running service (if any) so binary can be overwritten ─────────────
echo ""
echo "==> Stopping existing quantclaw services (if running)"
# shellcheck disable=SC2029
${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
  "sudo systemctl stop quantclaw 2>/dev/null || true; sudo systemctl stop ${SERVICE_NAME} 2>/dev/null || true"

# ── 3. Create remote directory ────────────────────────────────────────────────
echo ""
echo "==> Creating remote directory ${RPI_DIR}"
# shellcheck disable=SC2029
${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" "mkdir -p ${RPI_DIR}"

# ── 4. Deploy binary ──────────────────────────────────────────────────────────
echo ""
echo "==> Deploying binary to ${RPI_USER}@${RPI_HOST}:${RPI_DIR}/quantclaw"
${SCP_CMD} ${SCP_OPTS} "${BINARY}" "${RPI_USER}@${RPI_HOST}:${RPI_DIR}/quantclaw"
# shellcheck disable=SC2029
${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
  "sudo install -m 755 ${RPI_DIR}/quantclaw /usr/local/bin/quantclaw"

# ── 4. Create .env skeleton (if it doesn't exist) ────────────────────────────
ENV_DEST="${RPI_DIR}/.env"
echo ""
echo "==> Checking for ${ENV_DEST}"
# shellcheck disable=SC2029
if ${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" "[ -f ${ENV_DEST} ]"; then
  echo "    .env already exists — skipping"
else
  echo "    Creating .env skeleton with 600 permissions"
  # shellcheck disable=SC2029
  ${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
    "mkdir -p ${RPI_DIR} && \
     printf '# Provider key (set one)\nOPENAI_API_KEY=\n' > ${ENV_DEST} && \
     chmod 600 ${ENV_DEST}"
  echo "    IMPORTANT: edit ${ENV_DEST} on the Pi and set OPENAI_API_KEY or OPENROUTER_API_KEY"
fi

# ── 5. Deploy config ─────────────────────────────────────────────────────────
CONFIG_DEST="${RPI_DIR}/.quantclaw/config.toml"
echo ""
echo "==> Deploying config to ${CONFIG_DEST}"
# shellcheck disable=SC2029
${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" "mkdir -p ${RPI_DIR}/.quantclaw/workspace ${RPI_DIR}/releases"
# Preserve existing api_key from the remote config if present.
# shellcheck disable=SC2029
EXISTING_API_KEY=$(${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
  "grep -m1 '^api_key' ${CONFIG_DEST} 2>/dev/null || true")
${SCP_CMD} ${SCP_OPTS} "scripts/rpi-config.toml" "${RPI_USER}@${RPI_HOST}:${CONFIG_DEST}"
if [[ -n "${EXISTING_API_KEY}" ]]; then
  echo "    Restoring existing api_key from previous config"
  # shellcheck disable=SC2029
  ${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
    "sed -i 's|^# api_key = .*|${EXISTING_API_KEY}|' ${CONFIG_DEST}"
fi

# ── 6. Deploy web assets (if present) ────────────────────────────────────────
echo ""
if [[ -d "web/dist" ]]; then
  echo "==> Deploying web assets to /usr/local/share/quantclaw/web/dist"
  TMP_WEB_TARBALL="$(mktemp -t quantclaw-web.XXXXXX.tar.gz)"
  tar czf "${TMP_WEB_TARBALL}" -C web dist
  ${SCP_CMD} ${SCP_OPTS} "${TMP_WEB_TARBALL}" "${RPI_USER}@${RPI_HOST}:/tmp/quantclaw-web-dist.tar.gz"
  # shellcheck disable=SC2029
  ${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
    "sudo mkdir -p /usr/local/share/quantclaw/web && \
     sudo tar xzf /tmp/quantclaw-web-dist.tar.gz -C /usr/local/share/quantclaw/web && \
     rm -f /tmp/quantclaw-web-dist.tar.gz"
  rm -f "${TMP_WEB_TARBALL}"
else
  echo "==> WARNING: web/dist not found; gateway will start without packaged dashboard assets"
fi

# ── 7. Deploy and enable systemd service ─────────────────────────────────────
echo ""
echo "==> Installing systemd service (requires sudo on the Pi)"
${SCP_CMD} ${SCP_OPTS} "scripts/quantclaw-rust.service" "${RPI_USER}@${RPI_HOST}:/tmp/${SERVICE_NAME}.service"
# shellcheck disable=SC2029
${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
  "sudo rm -f /etc/systemd/system/quantclaw.service && \
   sudo systemctl disable quantclaw 2>/dev/null || true && \
   sudo mv /tmp/${SERVICE_NAME}.service /etc/systemd/system/${SERVICE_NAME}.service && \
   sudo sed -i \
     -e 's|^User=.*|User=${RPI_USER}|' \
     -e 's|^WorkingDirectory=.*|WorkingDirectory=${RPI_DIR}/current|' \
     -e 's|^ExecStart=.*|ExecStart=/usr/local/bin/quantclaw gateway --config-dir ${RPI_DIR}/.quantclaw|' \
     -e 's|^EnvironmentFile=.*|EnvironmentFile=${RPI_DIR}/.env|' \
     -e 's|^Environment=HOME=.*|Environment=HOME=/home/${RPI_USER}|' \
     /etc/systemd/system/${SERVICE_NAME}.service && \
   if ! grep -q '^Environment=QUANTCLAW_CONFIG_DIR=' /etc/systemd/system/${SERVICE_NAME}.service; then \
      echo 'Environment=QUANTCLAW_CONFIG_DIR=${RPI_DIR}/.quantclaw' | sudo tee -a /etc/systemd/system/${SERVICE_NAME}.service >/dev/null; \
   fi && \
   ln -sfn ${RPI_DIR} ${RPI_DIR}/current >/dev/null 2>&1 || true && \
   sudo systemctl daemon-reload && \
   sudo systemctl enable ${SERVICE_NAME} && \
   sudo systemctl restart ${SERVICE_NAME} && \
   sudo systemctl status ${SERVICE_NAME} --no-pager || true"

# ── 8. Runtime permissions ───────────────────────────────────────────────────
echo ""
echo "==> Granting ${RPI_USER} access to GPIO group"
# shellcheck disable=SC2029
${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
  "sudo usermod -aG gpio ${RPI_USER} || true"

# ── 9. Reset ACT LED trigger so QuantClaw can control it ──────────────────────
echo ""
echo "==> Installing udev rule for ACT LED sysfs access by gpio group"
${SCP_CMD} ${SCP_OPTS} "scripts/99-act-led.rules" "${RPI_USER}@${RPI_HOST}:/tmp/99-act-led.rules"
# shellcheck disable=SC2029
${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
  "sudo mv /tmp/99-act-led.rules /etc/udev/rules.d/99-act-led.rules && \
   sudo udevadm control --reload-rules && \
   sudo chgrp gpio /sys/class/leds/ACT/brightness /sys/class/leds/ACT/trigger 2>/dev/null || true && \
   sudo chmod g+w /sys/class/leds/ACT/brightness /sys/class/leds/ACT/trigger 2>/dev/null || true"

echo ""
echo "==> Resetting ACT LED trigger (none)"
# shellcheck disable=SC2029
${SSH_CMD} ${SSH_OPTS} "${RPI_USER}@${RPI_HOST}" \
  "echo none | sudo tee /sys/class/leds/ACT/trigger > /dev/null 2>&1 || true"

echo ""
echo "==> Deployment complete!"
echo ""
echo "    QuantClaw is running at http://${RPI_HOST}:42617"
echo "    POST /api/chat  — chat with the agent"
echo "    GET  /health    — health check"
echo ""
echo "    To check logs: ssh ${RPI_USER}@${RPI_HOST} 'journalctl -u ${SERVICE_NAME} -f'"
