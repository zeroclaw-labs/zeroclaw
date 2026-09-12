#!/usr/bin/env bash
# Fresh-volume startup smoke test for the zerorelay container image.
#
# Regression for the finding "the documented non-root relay image cannot create
# its fresh TLS volume": the image runs as USER nonroot (UID 65532) and the
# baked config self-provisions outer TLS into /data/tls on first run. If /data
# is not owned by the runtime UID, `docker compose up` on a FRESH named volume
# fails with `mkdir: can't create directory '/data/tls': Permission denied`
# before the relay ever binds.
#
# This test reproduces exactly that first-run layout — a brand-new named volume
# mounted at /data, container running as the image's own nonroot user — and
# proves (a) the relay self-provisions its CA into /data/tls and (b) the
# shell-less healthcheck subcommand reports the listener reachable.
#
# Build the image first, then point this script at it:
#   docker build -f apps/zerorelay/Dockerfile -t zerorelay:smoke .
#   ZERORELAY_SMOKE_IMAGE=zerorelay:smoke scripts/ci/smoke_relay_fresh_volume.sh
set -euo pipefail

image="${ZERORELAY_SMOKE_IMAGE:?set ZERORELAY_SMOKE_IMAGE to a locally loaded zerorelay image}"
volume="zerorelay-smoke-vol-$$"
container="zerorelay-smoke-$$"

cleanup() {
  local status=$?
  trap - EXIT
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker volume rm -f "$volume" >/dev/null 2>&1 || true
  exit "$status"
}
trap cleanup EXIT

# A brand-new named volume: empty, so /data's ownership comes entirely from the
# image layer. This is the exact condition the finding failed under.
docker volume create "$volume" >/dev/null

# Run the relay exactly as the compose contract does: default entrypoint, the
# baked config, /data backed by the fresh volume, no ownership fix-up. If the
# image did not seed /data as 65532, self-provisioning aborts here.
docker run --detach --name "$container" \
  --volume "$volume:/data" \
  "$image" >/dev/null

echo "waiting for the relay to self-provision TLS and bind..."
provisioned=""
for _attempt in $(seq 1 30); do
  if ! docker ps --filter "name=$container" --filter "status=running" \
      --format '{{.Names}}' | grep -q "$container"; then
    echo "FAIL: the relay container exited during first-run startup" >&2
    docker logs "$container" >&2 || true
    exit 1
  fi
  # The self-provisioned CA is the artifact the finding could not create. The
  # runtime image is distroless (no shell/test binary), so read it back through
  # the container with `docker cp` — succeeds only if the file exists on the
  # fresh volume. No helper image required.
  if docker cp "$container:/data/tls/ca.crt" - >/dev/null 2>&1; then
    provisioned=1
    break
  fi
  sleep 1
done

if [[ -z "$provisioned" ]]; then
  echo "FAIL: /data/tls/ca.crt was not self-provisioned on the fresh volume" >&2
  echo "      (this is the permission-denied regression the fix addresses)" >&2
  docker logs "$container" >&2 || true
  exit 1
fi
echo "ok: relay self-provisioned /data/tls/ca.crt on a fresh volume as nonroot"

# The shell-less healthcheck subcommand is what compose's HEALTHCHECK invokes;
# a clean exit proves the outer TLS listener actually came up.
if ! docker exec "$container" /usr/local/bin/zerorelay healthcheck --addr 127.0.0.1:8443; then
  echo "FAIL: relay healthcheck did not report the listener reachable" >&2
  docker logs "$container" >&2 || true
  exit 1
fi
echo "ok: relay healthcheck reports 127.0.0.1:8443 reachable"
echo "PASS: fresh-volume non-root startup self-provisions TLS and binds"
