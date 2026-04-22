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
  # Tailscale runs in userspace-networking mode. We DO NOT route
  # container traffic through it — that would break outbound calls to
  # Telegram/Slack/etc. Tailscale is here so YOU (the human operator)
  # can reach https://adi-zeroclaw-shane.<tailnet>.ts.net:42617/
  # from your phone/laptop. App-to-app traffic goes over Fly's 6PN
  # internal network (adi-cliproxy.internal:8317).
  log "starting tailscaled (userspace-networking, operator dashboard access)"
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

# ── Install Adi persona ───────────────────────────────────────────
# Persona files are baked into the image at /opt/adi-persona/ by the
# Dockerfile, scoped to the PERSONA build-arg (shane|meg) so no cross-
# contamination. Layout:
#   /opt/adi-persona/IDENTITY.md,SOUL.md,BRAND.md,FOXY.md,AGENTS.md,README.md — shared prompt files
#   /opt/adi-persona/USER.md                                                  — this persona's user context
#   /opt/adi-persona/memory/shared/*.md                                       — household/shared memories
#   /opt/adi-persona/memory/private/*.md                                      — this persona's private memories
#
# Zeroclaw injects a single MEMORY.md. We concatenate shared + private
# into /zeroclaw-data/workspace/MEMORY.md so zeroclaw sees the full
# context the way its README documents.
if [[ -d /opt/adi-persona ]]; then
  # Always overwrite from the image — persona files are source-controlled
  # in the private adi-persona repo and baked into the image at build
  # time. If you want to experiment with a file, edit it in the repo
  # and redeploy. This keeps all instances in sync with git.
  installed=0
  for f in IDENTITY.md SOUL.md BRAND.md FOXY.md AGENTS.md README.md USER.md; do
    if [[ -f "/opt/adi-persona/$f" ]]; then
      cp "/opt/adi-persona/$f" "/zeroclaw-data/workspace/$f"
      installed=$((installed + 1))
    fi
  done

  # Always regenerate MEMORY.md from the image's memory fragments so
  # persona memory updates propagate on redeploy.
  memory_parts=()
  [[ -d /opt/adi-persona/memory/shared ]]  && memory_parts+=(/opt/adi-persona/memory/shared/*.md)
  [[ -d /opt/adi-persona/memory/private ]] && memory_parts+=(/opt/adi-persona/memory/private/*.md)
  if [[ ${#memory_parts[@]} -gt 0 ]]; then
    {
      echo "# MEMORY.md — Long-term facts & lessons"
      echo ""
      echo "_Generated from persona memory fragments on every boot. Source: adi-persona repo._"
      echo ""
      for part in "${memory_parts[@]}"; do
        if [[ -f "$part" ]]; then
          echo "---"
          echo "<!-- source: $(basename "$(dirname "$part")")/$(basename "$part") -->"
          echo ""
          cat "$part"
          echo ""
        fi
      done
    } > /zeroclaw-data/workspace/MEMORY.md
    log "generated MEMORY.md from ${#memory_parts[@]} memory fragment(s)"
  fi

  (( installed > 0 )) && log "synced $installed persona prompt file(s) from image"
fi

# ── Seed config ───────────────────────────────────────────────────
# Seed once if missing, so runtime edits (e.g. `zeroclaw channel
# bind-telegram`, `zeroclaw config set`) survive restarts. To reset
# config to the latest seed, ssh in and `rm /zeroclaw-data/.zeroclaw/config.toml`
# before restarting the machine.
if [[ ! -f "$ZEROCLAW_CONFIG" ]]; then
  log "seeding $ZEROCLAW_CONFIG from $ZEROCLAW_SEED (first boot)"
  cp "$ZEROCLAW_SEED" "$ZEROCLAW_CONFIG"
  chmod 600 "$ZEROCLAW_CONFIG"

  # Per-persona table name injected into [memory.postgres] table field.
  persona_table="${ZEROCLAW_MEMORY_POSTGRES_TABLE:-memories}"
  log "setting [memory.postgres] table = $persona_table"
  if grep -qE '^\s*table\s*=' "$ZEROCLAW_CONFIG"; then
    sed -i -E "s|^(\s*)table\s*=.*|\1table = \"$persona_table\"|" "$ZEROCLAW_CONFIG"
  else
    sed -i "/^\[memory.postgres\]/a table = \"$persona_table\"" "$ZEROCLAW_CONFIG"
  fi

  # zeroclaw doesn't expand ${VAR} in TOML string values; do it here
  # from Fly secrets. If a secret is unset, leave the placeholder so
  # zeroclaw fails loudly at channel-init rather than silently.
  for var in TELEGRAM_BOT_TOKEN SLACK_BOT_TOKEN SLACK_APP_TOKEN COMPOSIO_API_KEY; do
    val="${!var:-}"
    if [[ -n "$val" ]]; then
      esc_val=$(printf '%s' "$val" | sed -e 's/[\/&]/\\&/g')
      sed -i "s|\${$var}|$esc_val|g" "$ZEROCLAW_CONFIG"
    fi
  done
  # PERSONA comes from the Dockerfile ENV (set from the PERSONA build-arg).
  if [[ -n "${PERSONA:-}" ]]; then
    sed -i "s|\${PERSONA}|${PERSONA}|g" "$ZEROCLAW_CONFIG"
  fi
else
  log "existing $ZEROCLAW_CONFIG preserved (seed-once mode)"
fi

# ── Bridge CLIPROXY_API_KEY → ZEROCLAW_API_KEY ─────────────────────
# The `custom:` provider auth key comes from ZEROCLAW_API_KEY / API_KEY
# (see crates/zeroclaw-providers/src/lib.rs:996). We don't set
# ZEROCLAW_API_KEY as a separate Fly secret — that would triple-source
# the same value. Instead, mirror CLIPROXY_API_KEY (the shared Fly
# secret all three apps already have) into ZEROCLAW_API_KEY at boot.
if [[ -n "${CLIPROXY_API_KEY:-}" ]]; then
  export ZEROCLAW_API_KEY="$CLIPROXY_API_KEY"
  log "bridged CLIPROXY_API_KEY → ZEROCLAW_API_KEY for cliproxy auth"
fi

# ── Launch zeroclaw ───────────────────────────────────────────────
log "starting zeroclaw daemon ($TAILSCALE_HOSTNAME)"
exec /usr/local/bin/zeroclaw daemon
