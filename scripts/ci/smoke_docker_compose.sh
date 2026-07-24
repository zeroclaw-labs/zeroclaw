#!/usr/bin/env bash
# Verify the repository Compose contract against a persisted loopback config.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image="${ZEROCLAW_COMPOSE_SMOKE_IMAGE:?set ZEROCLAW_COMPOSE_SMOKE_IMAGE to a locally loaded image}"
requested_host_port="${ZEROCLAW_COMPOSE_SMOKE_HOST_PORT:-42618}"
smoke_root="$(mktemp -d)"
override_file="$smoke_root/compose.override.yml"
config_file="$smoke_root/config.toml"
project_name="zeroclaw-compose-smoke-$$"
container_name="zeroclaw-compose-smoke-$$"

compose() {
  HOST_PORT="127.0.0.1:${requested_host_port}" \
    ZEROCLAW_GATEWAY_PORT=42617 \
    ZEROCLAW_COMPOSE_SMOKE_CONFIG="$config_file" \
    ZEROCLAW_COMPOSE_SMOKE_IMAGE="$image" \
    ZEROCLAW_COMPOSE_SMOKE_CONTAINER="$container_name" \
    docker compose \
      --project-name "$project_name" \
      --file "$repo_root/docker-compose.yml" \
      --file "$override_file" \
      "$@"
}

cleanup() {
  local status=$?
  trap - EXIT

  if ! compose down --volumes --remove-orphans; then
    echo "failed to tear down Compose smoke-test resources" >&2
    status=1
  fi
  rm -rf "$smoke_root"
  exit "$status"
}
trap cleanup EXIT

cat > "$config_file" <<'CONFIG'
[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false
require_pairing = false
CONFIG

cat > "$override_file" <<'COMPOSE'
services:
  zeroclaw:
    image: "${ZEROCLAW_COMPOSE_SMOKE_IMAGE}"
    pull_policy: never
    container_name: "${ZEROCLAW_COMPOSE_SMOKE_CONTAINER}"
    restart: "no"
    volumes:
      - "${ZEROCLAW_COMPOSE_SMOKE_CONFIG}:/zeroclaw-data/.zeroclaw/config.toml:ro"
COMPOSE

compose config --quiet
compose up --detach

published="$(compose port zeroclaw 42617 | sed -n '1p')"
published_port="${published##*:}"
case "$published_port" in
  ''|*[!0-9]*)
    echo "could not resolve published gateway port from: $published" >&2
    exit 1
    ;;
esac
if [[ "$published_port" != "$requested_host_port" ]]; then
  echo "expected host port $requested_host_port, Compose published $published" >&2
  exit 1
fi

for _attempt in $(seq 1 30); do
  if health_response="$(curl --fail --silent --show-error --max-time 2 \
    "http://127.0.0.1:${published_port}/health" 2>/dev/null)"; then
    printf '%s\n' "$health_response"
    exit 0
  fi
  sleep 1
done

compose ps >&2
compose logs zeroclaw >&2
echo "gateway did not become reachable through published port $published_port" >&2
exit 1
