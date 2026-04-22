#!/usr/bin/env bash
# deploy/zeroclaw/entrypoint.sh — boots adi-zeroclaw-shane / adi-zeroclaw-meg.
#
# 1. Start tailscaled and join the tailnet (tag:fly-gw).
# 2. Seed /zeroclaw-data/.zeroclaw/config.toml on first boot.
# 3. Exec zeroclaw daemon.

set -euo pipefail

log() { echo "[zeroclaw-entrypoint] $*"; }

TAILSCALE_DIR="/zeroclaw-data/tailscale"
TAILSCALE_STATE="${TAILSCALE_DIR}/tailscaled.state"
TAILSCALE_SOCKET="${TAILSCALE_DIR}/tailscaled.sock"
TAILSCALE_LOG="${TAILSCALE_DIR}/tailscaled.log"

ZEROCLAW_CONFIG="/zeroclaw-data/.zeroclaw/config.toml"
ZEROCLAW_SEED="/opt/zeroclaw.config.seed.toml"

# Hostname on the tailnet, set per-persona via fly.toml's [env].
# Fly injects FLY_APP_NAME automatically (e.g. "adi-zeroclaw-shane").
TAILSCALE_HOSTNAME="${FLY_APP_NAME:-adi-zeroclaw-unknown}"

mkdir -p "$TAILSCALE_DIR" /zeroclaw-data/.zeroclaw /zeroclaw-data/workspace

# ── Tailscale ─────────────────────────────────────────────────────
if [[ -n "${TAILSCALE_AUTHKEY:-}" ]]; then
  log "starting tailscaled"
  /usr/local/bin/tailscaled \
    --state="$TAILSCALE_STATE" \
    --socket="$TAILSCALE_SOCKET" \
    --statedir="$TAILSCALE_DIR" \
    --tun=userspace-networking \
    >>"$TAILSCALE_LOG" 2>&1 &

  for _ in $(seq 1 30); do
    [[ -S "$TAILSCALE_SOCKET" ]] && break
    sleep 0.5
  done

  log "tailscale up (hostname=$TAILSCALE_HOSTNAME, tag:fly-gw)"
  /usr/local/bin/tailscale --socket="$TAILSCALE_SOCKET" up \
    --authkey="$TAILSCALE_AUTHKEY" \
    --hostname="$TAILSCALE_HOSTNAME" \
    --advertise-tags="tag:fly-gw" \
    --accept-dns=false \
    --reset
else
  log "TAILSCALE_AUTHKEY unset; skipping tailnet join. cliproxy unreachable."
fi

# ── Seed config ───────────────────────────────────────────────────
if [[ ! -f "$ZEROCLAW_CONFIG" ]]; then
  log "seeding $ZEROCLAW_CONFIG from $ZEROCLAW_SEED"
  cp "$ZEROCLAW_SEED" "$ZEROCLAW_CONFIG"

  # Per-persona table name (default "memories" if env unset).
  persona_table="${ZEROCLAW_MEMORY_POSTGRES_TABLE:-memories}"
  log "setting [memory.postgres] table = $persona_table"
  if grep -q '^\[memory.postgres\]' "$ZEROCLAW_CONFIG"; then
    # Add table line under the [memory.postgres] section if missing; else replace.
    if grep -qE '^\s*table\s*=' "$ZEROCLAW_CONFIG"; then
      sed -i -E "s|^(\s*)table\s*=.*|\1table = \"$persona_table\"|" "$ZEROCLAW_CONFIG"
    else
      sed -i "/^\[memory.postgres\]/a table = \"$persona_table\"" "$ZEROCLAW_CONFIG"
    fi
  fi
fi

# ── Launch zeroclaw ───────────────────────────────────────────────
log "starting zeroclaw daemon ($TAILSCALE_HOSTNAME)"
exec /usr/local/bin/zeroclaw daemon
