#!/bin/sh
set -e

CONFIG="/etc/tinyproxy/tinyproxy.conf"
RUNTIME_CONFIG="/tmp/tinyproxy.conf"

# Copy base config
cp "$CONFIG" "$RUNTIME_CONFIG"

# Add allowed CONNECT domains from env
if [ -n "$ALLOWED_CONNECT" ]; then
    echo "$ALLOWED_CONNECT" | tr ',' '\n' | while read -r line; do
        port=$(echo "$line" | cut -d: -f2)
        echo "ConnectPort $port" >> "$RUNTIME_CONFIG"
    done
fi

exec tinyproxy -d -c "$RUNTIME_CONFIG"
