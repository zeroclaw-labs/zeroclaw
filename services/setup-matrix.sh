#!/usr/bin/env bash
# setup-matrix.sh — Bootstrap a continuwuity Matrix homeserver for ZeroClaw.
#
# What this does:
#   1. Writes docker-compose.yml for continuwuity
#   2. Starts the container with registration briefly enabled
#   3. Creates the admin user, zeroclaw bot user, and room-bot user
#   4. Creates default rooms and invites the zeroclaw bot
#   5. Disables registration and restarts
#   6. Writes ~/.zeroclaw/matrix-setup-output.env with config values
#      ready to paste into ~/.zeroclaw/config.toml
#
# Usage:
#   ./services/setup-matrix.sh \
#       --server-name matrix.example.com \
#       --admin-user alice \
#       --admin-password s3cret \
#       --bot-password bot-s3cret \
#       --roombot-password roombot-s3cret \
#       --port 6167 \
#       --output-dir ~/my-matrix   # where docker-compose.yml is written
#
# All flags are optional; the script prompts for anything not supplied.

set -euo pipefail

###############################################################################
# Defaults
###############################################################################
SERVER_NAME=""
ADMIN_USER=""
ADMIN_PASSWORD=""
BOT_USER="zeroclaw"
BOT_PASSWORD=""
ROOMBOT_USER="room-bot"
ROOMBOT_PASSWORD=""
PORT=6167
OUTPUT_DIR=""
ZEROCLAW_CONFIG_DIR="${HOME}/.zeroclaw"

###############################################################################
# Argument parsing
###############################################################################
while [[ $# -gt 0 ]]; do
    case "$1" in
        --server-name)   SERVER_NAME="$2";   shift 2 ;;
        --admin-user)    ADMIN_USER="$2";    shift 2 ;;
        --admin-password) ADMIN_PASSWORD="$2"; shift 2 ;;
        --bot-password)  BOT_PASSWORD="$2";  shift 2 ;;
        --roombot-password) ROOMBOT_PASSWORD="$2"; shift 2 ;;
        --port)          PORT="$2";          shift 2 ;;
        --output-dir)    OUTPUT_DIR="$2";    shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

###############################################################################
# Interactive prompts for anything missing
###############################################################################
prompt() {
    local var="$1" prompt="$2" default="${3:-}"
    if [[ -z "${!var:-}" ]]; then
        if [[ -n "$default" ]]; then
            read -rp "$prompt [$default]: " val
            eval "$var=\"${val:-$default}\""
        else
            read -rp "$prompt: " val
            eval "$var=\"$val\""
        fi
    fi
}

prompt_secret() {
    local var="$1" prompt="$2"
    if [[ -z "${!var:-}" ]]; then
        read -rsp "$prompt: " val; echo
        eval "$var=\"$val\""
    fi
}

echo "=== ZeroClaw Matrix Setup ==="
echo ""
prompt SERVER_NAME "Matrix server name (domain only, e.g. matrix.example.com)"
prompt ADMIN_USER  "Admin username (local part, e.g. alice)"
prompt_secret ADMIN_PASSWORD "Admin password"
prompt_secret BOT_PASSWORD   "zeroclaw bot password"
prompt_secret ROOMBOT_PASSWORD "room-bot password"
prompt OUTPUT_DIR "Directory for docker-compose.yml" "${HOME}/matrix-${SERVER_NAME}"

HS="http://localhost:${PORT}"
MATRIX_DIR="$(realpath "${OUTPUT_DIR}")"

mkdir -p "${MATRIX_DIR}"

###############################################################################
# 1. Write docker-compose.yml (registration enabled for bootstrap)
###############################################################################
echo ""
echo "Writing ${MATRIX_DIR}/docker-compose.yml ..."

cat > "${MATRIX_DIR}/docker-compose.yml" <<EOF
services:
  homeserver:
    image: forgejo.ellis.link/continuwuation/continuwuity:latest
    restart: unless-stopped
    command: /sbin/conduwuit
    ports:
      - "${PORT}:${PORT}"
    volumes:
      - db:/var/lib/continuwuity
    environment:
      CONTINUWUITY_SERVER_NAME: ${SERVER_NAME}
      CONTINUWUITY_DATABASE_PATH: /var/lib/continuwuity
      CONTINUWUITY_PORT: ${PORT}
      CONTINUWUITY_ADDRESS: "0.0.0.0"
      CONTINUWUITY_MAX_REQUEST_SIZE: 20000000
      CONTINUWUITY_ALLOW_REGISTRATION: "true"
      CONTINUWUITY_ALLOW_FEDERATION: "false"
      CONTINUWUITY_ALLOW_GUEST_REGISTRATION: "false"
      CONTINUWUITY_ALLOW_ROOM_CREATION: "true"
      CONTINUWUITY_LOG: "warn,state_res=warn"
      CONTINUWUITY_TRUSTED_SERVERS: "[]"

volumes:
  db:
EOF

###############################################################################
# 2. Start container
###############################################################################
echo "Starting continuwuity..."
cd "${MATRIX_DIR}"
docker compose up -d

echo "Waiting for homeserver to be ready..."
for i in $(seq 1 30); do
    if curl -sf "${HS}/_matrix/client/versions" >/dev/null 2>&1; then
        echo "  Ready."
        break
    fi
    sleep 2
done

###############################################################################
# Helper: register a user
###############################################################################
register_user() {
    local username="$1" password="$2"
    local result
    result=$(curl -sf -X POST "${HS}/_matrix/client/v3/register" \
        -H "Content-Type: application/json" \
        -d "{\"username\":\"${username}\",\"password\":\"${password}\",\"kind\":\"user\",\"auth\":{\"type\":\"m.login.dummy\"}}" \
        2>&1) || true
    echo "${result}"
}

###############################################################################
# Helper: login and return access token
###############################################################################
get_token() {
    local username="$1" password="$2"
    curl -sf -X POST "${HS}/_matrix/client/v3/login" \
        -H "Content-Type: application/json" \
        -d "{\"type\":\"m.login.password\",\"user\":\"${username}\",\"password\":\"${password}\"}" \
        | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])"
}

###############################################################################
# Helper: create a room and return room_id
###############################################################################
create_room() {
    local token="$1" name="$2" alias="$3"
    curl -sf -X POST "${HS}/_matrix/client/v3/createRoom" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${token}" \
        -d "{\"name\":\"${name}\",\"room_alias_name\":\"${alias}\",\"preset\":\"private_chat\",\"visibility\":\"private\"}" \
        | python3 -c "import sys,json; print(json.load(sys.stdin)['room_id'])"
}

###############################################################################
# Helper: invite a user to a room
###############################################################################
invite_user() {
    local token="$1" room_id="$2" user_id="$3"
    curl -sf -X POST "${HS}/_matrix/client/v3/rooms/$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1],safe=''))" "${room_id}")/invite" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${token}" \
        -d "{\"user_id\":\"${user_id}\"}" >/dev/null
}

###############################################################################
# Helper: join a room
###############################################################################
join_room() {
    local token="$1" room_id="$2"
    curl -sf -X POST "${HS}/_matrix/client/v3/join/$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1],safe=''))" "${room_id}")" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${token}" \
        -d '{}' >/dev/null
}

###############################################################################
# 3. Create users
###############################################################################
echo "Creating admin user @${ADMIN_USER}:${SERVER_NAME} ..."
register_user "${ADMIN_USER}" "${ADMIN_PASSWORD}" >/dev/null

echo "Creating bot user @${BOT_USER}:${SERVER_NAME} ..."
register_user "${BOT_USER}" "${BOT_PASSWORD}" >/dev/null

echo "Creating room-bot user @${ROOMBOT_USER}:${SERVER_NAME} ..."
register_user "${ROOMBOT_USER}" "${ROOMBOT_PASSWORD}" >/dev/null

###############################################################################
# 4. Get tokens
###############################################################################
echo "Logging in to get access tokens..."
ADMIN_TOKEN=$(get_token "${ADMIN_USER}" "${ADMIN_PASSWORD}")
BOT_TOKEN=$(get_token "${BOT_USER}" "${BOT_PASSWORD}")
ROOMBOT_TOKEN=$(get_token "${ROOMBOT_USER}" "${ROOMBOT_PASSWORD}")

###############################################################################
# 5. Create default rooms
###############################################################################
echo "Creating rooms..."

ROOM_GENERAL=$(create_room "${ADMIN_TOKEN}" "General" "general")
echo "  General: ${ROOM_GENERAL}"

ROOM_ZEROCLAW=$(create_room "${ADMIN_TOKEN}" "ZeroClaw" "zeroclaw")
echo "  ZeroClaw: ${ROOM_ZEROCLAW}"

###############################################################################
# 6. Invite and join the bot user to its rooms
###############################################################################
echo "Inviting @${BOT_USER}:${SERVER_NAME} to rooms..."
invite_user "${ADMIN_TOKEN}" "${ROOM_GENERAL}" "@${BOT_USER}:${SERVER_NAME}"
join_room "${BOT_TOKEN}" "${ROOM_GENERAL}"

invite_user "${ADMIN_TOKEN}" "${ROOM_ZEROCLAW}" "@${BOT_USER}:${SERVER_NAME}"
join_room "${BOT_TOKEN}" "${ROOM_ZEROCLAW}"

###############################################################################
# 7. Disable registration
###############################################################################
echo "Disabling registration..."
sed -i.bak 's/CONTINUWUITY_ALLOW_REGISTRATION: "true"/CONTINUWUITY_ALLOW_REGISTRATION: "false"/' \
    "${MATRIX_DIR}/docker-compose.yml"
docker compose up -d

###############################################################################
# 8. Encrypt the bot access token using zeroclaw (if available)
###############################################################################
ZEROCLAW_BIN="${HOME}/.cargo/bin/zeroclaw"
if [[ -f "${ZEROCLAW_BIN}" ]]; then
    echo "Encrypting bot access token with zeroclaw..."
    BOT_TOKEN_ENC=$(echo -n "${BOT_TOKEN}" | "${ZEROCLAW_BIN}" encrypt-secret - 2>/dev/null || echo "${BOT_TOKEN}")
else
    BOT_TOKEN_ENC="${BOT_TOKEN}"
fi

###############################################################################
# 9. Write output config
###############################################################################
OUTPUT_ENV="${ZEROCLAW_CONFIG_DIR}/matrix-setup-output.env"
mkdir -p "${ZEROCLAW_CONFIG_DIR}"

cat > "${OUTPUT_ENV}" <<EOF
# Generated by setup-matrix.sh on $(date)
# Paste the [channels_config.matrix] block into ~/.zeroclaw/config.toml
# and the [channel_workspaces] / [tmux_targets] entries as needed.

SERVER_NAME=${SERVER_NAME}
ADMIN_USER=@${ADMIN_USER}:${SERVER_NAME}
BOT_USER=@${BOT_USER}:${SERVER_NAME}
ROOMBOT_USER=@${ROOMBOT_USER}:${SERVER_NAME}
BOT_TOKEN=${BOT_TOKEN}
ROOM_GENERAL=${ROOM_GENERAL}
ROOM_ZEROCLAW=${ROOM_ZEROCLAW}

# ---- Paste into ~/.zeroclaw/config.toml ----

[channels_config.matrix]
homeserver = "http://localhost:${PORT}"
access_token = "${BOT_TOKEN_ENC}"
user_id = "@${BOT_USER}:${SERVER_NAME}"
room_ids = []
room_id = "${ROOM_GENERAL}"
allowed_users = ["@${ADMIN_USER}:${SERVER_NAME}"]

[channel_workspaces]
"${ROOM_ZEROCLAW}" = "/path/to/your/project"

[tmux_targets]
"${ROOM_ZEROCLAW}" = "main:zeroclaw"

# ---- Paste into ~/.zeroclaw/room-bot.json ----
# {
#   "homeserver": "http://localhost:${PORT}",
#   "user_id": "@${ROOMBOT_USER}:${SERVER_NAME}",
#   "password": "${ROOMBOT_PASSWORD}",
#   "poll_interval_secs": 30,
#   "context_lines": 15,
#   "pane_room_map": {
#     "main:zeroclaw": "${ROOM_ZEROCLAW}"
#   }
# }
EOF

###############################################################################
# Done
###############################################################################
echo ""
echo "=== Setup complete ==="
echo ""
echo "Config written to: ${OUTPUT_ENV}"
echo ""
echo "Next steps:"
echo "  1. Review ${OUTPUT_ENV}"
echo "  2. Paste the [channels_config.matrix] block into ~/.zeroclaw/config.toml"
echo "  3. Update [channel_workspaces] and [tmux_targets] with your project paths"
echo "  4. Create ~/.zeroclaw/room-bot.json using the template in ${OUTPUT_ENV}"
echo "  5. Restart ZeroClaw: zeroclaw daemon"
echo ""
echo "Admin user:   @${ADMIN_USER}:${SERVER_NAME}"
echo "Bot user:     @${BOT_USER}:${SERVER_NAME}"
echo "Room-bot:     @${ROOMBOT_USER}:${SERVER_NAME}"
echo "General room: ${ROOM_GENERAL}"
echo "ZeroClaw room: ${ROOM_ZEROCLAW}"
