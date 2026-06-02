#!/bin/bash
# DaemonClaw hourly backup — SQLite-safe, covers all durable state.
# Installed by `daemonclaw service install` into /usr/local/lib/daemonclaw/.
set -euo pipefail

BACKUP_DIR="/var/backups/daemonclaw"
HOME_DIR="/var/lib/daemonclaw"
DC_DIR="${HOME_DIR}/.daemonclaw"
WORKSPACE="${DC_DIR}/workspace"
ETC_DIR="/etc/daemonclaw"

ts=$(date +%Y%m%d-%H%M%S)
staging=$(mktemp -d "${BACKUP_DIR}/.staging-${ts}-XXXX")
trap 'rm -rf "${staging}"' EXIT

# ── SQLite-safe DB snapshots ────────────────────────────────────
# Uses sqlite3 .backup API which handles WAL checkpointing correctly.
# If sqlite3 isn't available, falls back to copying DB + WAL + SHM together.
backup_sqlite() {
    local src="$1" dst="$2"
    mkdir -p "$(dirname "$dst")"
    if command -v sqlite3 >/dev/null 2>&1; then
        sqlite3 "$src" ".backup '$dst'" 2>/dev/null && return 0
    fi
    # Fallback: copy DB + WAL + SHM atomically (best-effort)
    cp -a "$src" "$dst" 2>/dev/null || true
    [ -f "${src}-wal" ] && cp -a "${src}-wal" "${dst}-wal" 2>/dev/null || true
    [ -f "${src}-shm" ] && cp -a "${src}-shm" "${dst}-shm" 2>/dev/null || true
}

# Memory (brain.db) — actively written every conversation turn
if [ -f "${WORKSPACE}/memory/brain.db" ]; then
    backup_sqlite "${WORKSPACE}/memory/brain.db" "${staging}/memory/brain.db"
fi

# Sessions (sessions.db) — written every message
if [ -f "${WORKSPACE}/sessions/sessions.db" ]; then
    backup_sqlite "${WORKSPACE}/sessions/sessions.db" "${staging}/sessions/sessions.db"
fi

# State (state.db) — costs, later tracks add more tables
if [ -f "${WORKSPACE}/state/state.db" ]; then
    backup_sqlite "${WORKSPACE}/state/state.db" "${staging}/state/state.db"
fi

# Cron (jobs.db) — job definitions + run history
if [ -f "${WORKSPACE}/cron/jobs.db" ]; then
    backup_sqlite "${WORKSPACE}/cron/jobs.db" "${staging}/cron/jobs.db"
fi

# Devices (devices.db) — paired device registry
if [ -f "${WORKSPACE}/devices.db" ]; then
    backup_sqlite "${WORKSPACE}/devices.db" "${staging}/devices.db"
fi

# Audit (audit.db) — Merkle-chained security audit trail
if [ -f "${WORKSPACE}/audit/audit.db" ]; then
    backup_sqlite "${WORKSPACE}/audit/audit.db" "${staging}/audit/audit.db"
fi

# ── Non-DB files ────────────────────────────────────────────────
# Config (follow symlink to /etc/daemonclaw/config.toml)
if [ -f "${ETC_DIR}/config.toml" ]; then
    mkdir -p "${staging}/config"
    cp -L "${ETC_DIR}/config.toml" "${staging}/config/config.toml"
fi

# Runtime state files (costs.jsonl.migrated, events.jsonl, traces, etc.)
if [ -d "${WORKSPACE}/state" ]; then
    mkdir -p "${staging}/state"
    find "${WORKSPACE}/state" -maxdepth 1 -type f \
        ! -name 'state.db' ! -name 'state.db-wal' ! -name 'state.db-shm' \
        -exec cp {} "${staging}/state/" \;
fi

# Daemon state (health snapshot)
if [ -f "${DC_DIR}/daemon_state.json" ]; then
    cp "${DC_DIR}/daemon_state.json" "${staging}/daemon_state.json"
fi

# ── .secret_key is deliberately EXCLUDED ────────────────────────
# The secret key decrypts all enc2: values in config.toml.
# Bundling it with config in the same archive means one stolen backup
# = full credential compromise. It should be backed up separately
# via a distinct mechanism (e.g. secrets manager, offline copy).

# ── Create archive ──────────────────────────────────────────────
tar czf "${BACKUP_DIR}/backup-${ts}.tar.gz" -C "${staging}" .

# ── Retention: keep last 168 (7 days hourly) ────────────────────
ls -1t "${BACKUP_DIR}"/backup-*.tar.gz 2>/dev/null | tail -n +169 | xargs -r rm -f
