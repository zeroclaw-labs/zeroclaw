#!/bin/sh
# ZeroClaw Railway Entrypoint
#
# Fixes volume mount permissions before dropping to non-root user.
# Railway mounts volumes as root:root, but ZeroClaw runs as non-root
# user 'zeroclaw'. This script runs as root to fix ownership, then
# exec's the main binary as the zeroclaw user via gosu.

set -e

ZEROCLAW_HOME="/app"
ZEROCLAW_DIR="${ZEROCLAW_HOME}/.zeroclaw"
WORKSPACE_DIR="${ZEROCLAW_DIR}/workspace"

# Fix ownership on volume-mounted directories if running as root.
if [ "$(id -u)" = "0" ]; then
    # Ensure directories exist inside the volume mount.
    mkdir -p "${ZEROCLAW_DIR}" "${WORKSPACE_DIR}"

    # Fix ownership so the zeroclaw user can write.
    chown -R zeroclaw:zeroclaw "${ZEROCLAW_DIR}"

    # Seed default config if none exists (first deploy with fresh volume).
    if [ ! -f "${ZEROCLAW_DIR}/config.toml" ]; then
        cat > "${ZEROCLAW_DIR}/config.toml" <<'TOML'
default_temperature = 0.7

[gateway]
allow_public_bind = true
require_pairing = false

[auth]
enabled = true
allow_registration = true
TOML
        chown zeroclaw:zeroclaw "${ZEROCLAW_DIR}/config.toml"
        chmod 600 "${ZEROCLAW_DIR}/config.toml"
    fi

    # Ensure [gateway] section has allow_public_bind = true (upgrade path).
    if ! grep -q 'allow_public_bind' "${ZEROCLAW_DIR}/config.toml" 2>/dev/null; then
        if grep -q '^\[gateway\]' "${ZEROCLAW_DIR}/config.toml" 2>/dev/null; then
            sed -i '/^\[gateway\]/a allow_public_bind = true' "${ZEROCLAW_DIR}/config.toml"
        else
            printf '\n[gateway]\nallow_public_bind = true\nrequire_pairing = false\n' \
                >> "${ZEROCLAW_DIR}/config.toml"
        fi
    fi

    # Ensure require_pairing = false for Railway (upgrade path).
    if grep -q 'require_pairing = true' "${ZEROCLAW_DIR}/config.toml" 2>/dev/null; then
        sed -i 's/require_pairing = true/require_pairing = false/' "${ZEROCLAW_DIR}/config.toml"
    fi

    # Ensure [auth] section exists in config (upgrade path for existing deploys).
    if ! grep -q '^\[auth\]' "${ZEROCLAW_DIR}/config.toml" 2>/dev/null; then
        printf '\n[auth]\nenabled = true\nallow_registration = true\n' \
            >> "${ZEROCLAW_DIR}/config.toml"
    fi

    # Drop to zeroclaw user and exec the command.
    exec gosu zeroclaw "$@"
fi

# If already running as non-root (shouldn't happen with this Dockerfile),
# just exec the command directly.
exec "$@"
