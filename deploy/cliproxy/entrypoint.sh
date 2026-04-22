#!/usr/bin/env bash
# deploy/cliproxy/entrypoint.sh — boots adi-cliproxy.
#
# 1. Start tailscaled in native TUN mode, join the tailnet with tag:fly-gw.
# 2. Seed /data/config.yaml on first boot from the baked-in seed.
# 3. Inject CLIPROXY_API_KEY and CLIPROXY_MGMT_KEY from Fly secrets (or
#    auto-generate and persist).
# 4. Exec cliproxy in the foreground.

set -euo pipefail

log() { echo "[cliproxy-entrypoint] $*"; }

TAILSCALE_DIR="/data/tailscale"
TAILSCALE_STATE="${TAILSCALE_DIR}/tailscaled.state"
TAILSCALE_SOCKET="${TAILSCALE_DIR}/tailscaled.sock"
TAILSCALE_LOG="${TAILSCALE_DIR}/tailscaled.log"

CLIPROXY_CONFIG="/data/config.yaml"
CLIPROXY_SEED="/opt/cliproxy.config.seed.yaml"
CLIPROXY_LOG="/data/cliproxy.log"

mkdir -p /data/auth "$TAILSCALE_DIR"

# ── Tailscale ─────────────────────────────────────────────────────
if [[ -n "${TAILSCALE_AUTHKEY:-}" ]]; then
  log "starting tailscaled"
  /usr/local/bin/tailscaled \
    --state="$TAILSCALE_STATE" \
    --socket="$TAILSCALE_SOCKET" \
    --statedir="$TAILSCALE_DIR" \
    --tun=userspace-networking \
    >>"$TAILSCALE_LOG" 2>&1 &
  tailscaled_pid=$!

  # Wait for socket
  for _ in $(seq 1 30); do
    [[ -S "$TAILSCALE_SOCKET" ]] && break
    sleep 0.5
  done

  log "tailscale up (hostname=adi-cliproxy, tag:fly-gw)"
  /usr/local/bin/tailscale --socket="$TAILSCALE_SOCKET" up \
    --authkey="$TAILSCALE_AUTHKEY" \
    --hostname="adi-cliproxy" \
    --advertise-tags="tag:fly-gw" \
    --accept-dns=false \
    --reset
else
  log "TAILSCALE_AUTHKEY unset; skipping Tailscale. Proxy will be unreachable from peers."
fi

# ── Seed config ───────────────────────────────────────────────────
if [[ ! -f "$CLIPROXY_CONFIG" ]]; then
  log "seeding $CLIPROXY_CONFIG from $CLIPROXY_SEED"
  cp "$CLIPROXY_SEED" "$CLIPROXY_CONFIG"
fi

# ── Inject secrets into config ────────────────────────────────────
# CLIPROXY_API_KEY: required so shane + meg can authenticate
if [[ -z "${CLIPROXY_API_KEY:-}" ]]; then
  if [[ ! -f /data/.api-key-auto ]]; then
    CLIPROXY_API_KEY=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
    echo "$CLIPROXY_API_KEY" >/data/.api-key-auto
    chmod 600 /data/.api-key-auto
    log "generated random CLIPROXY_API_KEY; stored at /data/.api-key-auto"
  fi
  CLIPROXY_API_KEY=$(</data/.api-key-auto)
fi
# Rewrite api-keys list — idempotent; matches the seed's `api-keys: []` line.
sed -i -E "s|^api-keys: .*|api-keys: [\"${CLIPROXY_API_KEY}\"]|" "$CLIPROXY_CONFIG"

# CLIPROXY_MGMT_KEY: optional
if [[ -n "${CLIPROXY_MGMT_KEY:-}" ]]; then
  sed -i -E "s|^  secret-key: .*|  secret-key: \"${CLIPROXY_MGMT_KEY}\"|" "$CLIPROXY_CONFIG"
fi

# ── Launch cliproxy ───────────────────────────────────────────────
log "starting cliproxy (config=$CLIPROXY_CONFIG, auth-dir=/data/auth)"
exec /usr/local/bin/cliproxy --config "$CLIPROXY_CONFIG"
