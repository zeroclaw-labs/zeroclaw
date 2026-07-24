#!/usr/bin/env bash
set -euo pipefail

if [ -f "dev/docker-compose.ci.yml" ]; then
  COMPOSE_FILE="dev/docker-compose.ci.yml"
elif [ -f "docker-compose.ci.yml" ] && [ "$(basename "$(pwd)")" = "dev" ]; then
  COMPOSE_FILE="docker-compose.ci.yml"
else
  echo "❌ Run this script from repo root or dev/ directory."
  exit 1
fi

compose_cmd=(docker compose -f "$COMPOSE_FILE")
SMOKE_CACHE_DIR="${SMOKE_CACHE_DIR:-.cache/buildx-smoke}"

run_in_ci() {
  local cmd="$1"
  "${compose_cmd[@]}" run --rm local-ci bash -c "$cmd"
}

run_firmware_protocol_gate() {
  run_in_ci "./scripts/ci/firmware_protocol_gate.sh"
}

build_smoke_image() {
  if docker buildx version >/dev/null 2>&1; then
    mkdir -p "$SMOKE_CACHE_DIR"
    local build_args=(
      --load
      --target dev
      --cache-to "type=local,dest=$SMOKE_CACHE_DIR,mode=max"
      -t zeroclaw-local-smoke:latest
      .
    )
    if [ -f "$SMOKE_CACHE_DIR/index.json" ]; then
      build_args=(--cache-from "type=local,src=$SMOKE_CACHE_DIR" "${build_args[@]}")
    fi
    docker buildx build "${build_args[@]}"
  else
    DOCKER_BUILDKIT=1 docker build --target dev -t zeroclaw-local-smoke:latest .
  fi
}

print_help() {
  cat <<'EOF'
ZeroClaw Local CI in Docker

Usage: ./dev/ci.sh <command>

Commands:
  build-image   Build/update the local CI image
  shell         Open an interactive shell inside the CI container
  lint          Run rustfmt + clippy correctness gate (container only)
  lint-strict   Run rustfmt + full clippy warnings gate (container only)
  firmware-protocol Run standalone firmware protocol host gate (container only)
  test          Run cargo test (container only)
  test-component  Run component tests only
  test-integration Run integration tests only
  test-system     Run system tests only
  test-live       Run live tests (requires credentials)
  test-manual     Run manual test scripts (dockerignore, etc.)
  build         Run release build smoke check (container only)
  audit         Run cargo audit (container only)
  deny          Run cargo deny check (container only)
  security      Run cargo audit + cargo deny (container only)
  docker-smoke  Build and verify runtime image (host docker daemon)
  all           Run lint, firmware-protocol, test, build, security, docker-smoke
  clean         Remove local CI containers and volumes
EOF
}

if [ $# -lt 1 ]; then
  print_help
  exit 1
fi

case "$1" in
  build-image)
    "${compose_cmd[@]}" build local-ci
    ;;

  shell)
    "${compose_cmd[@]}" run --rm local-ci bash
    ;;

  lint)
    run_in_ci "./scripts/ci/rust_quality_gate.sh"
    ;;

  lint-strict)
    run_in_ci "./scripts/ci/rust_quality_gate.sh --strict"
    ;;

  firmware-protocol)
    run_firmware_protocol_gate
    ;;

  test)
    # Local Docker test path uses the stable `cargo test` runner. Required
    # CI uses `cargo nextest run --locked --workspace --exclude zeroclaw-desktop`
    # (see `.github/workflows/ci.yml`). Both select the same workspace
    # package boundary, but they differ in runner, scheduling, isolation,
    # and reporting behavior (nextest runs each test binary in its own
    # process and emits per-binary JUnit reports; cargo test uses the test
    # harness's default process model).
    run_in_ci "cargo test --locked --workspace --exclude zeroclaw-desktop --verbose"
    ;;

  test-component)
    run_in_ci "cargo test --test component --locked --verbose"
    ;;

  test-integration)
    run_in_ci "cargo test --test integration --locked --verbose"
    ;;

  test-system)
    run_in_ci "cargo test --test system --locked --verbose"
    ;;

  test-live)
    run_in_ci "cargo test --test live -- --ignored --verbose"
    ;;

  test-manual)
    run_in_ci "bash tests/manual/test_dockerignore.sh"
    ;;

  build)
    run_in_ci "cargo build --release --locked --verbose"
    ;;

  audit)
    run_in_ci "cargo audit"
    ;;

  deny)
    run_in_ci "cargo deny check licenses sources"
    ;;

  security)
    run_in_ci "cargo deny check licenses sources"
    run_in_ci "cargo audit"
    ;;

  docker-smoke)
    build_smoke_image
    docker run --rm zeroclaw-local-smoke:latest --version
    ;;

  all)
    # The `test` arm above and the `cargo test` invocation below both use
    # `cargo test` (not `nextest`) — see the comment on the `test` case
    # for why this differs from required CI. If you change the runner here,
    # update that comment in lockstep.
    run_in_ci "./scripts/ci/rust_quality_gate.sh"
    run_firmware_protocol_gate
    run_in_ci "cargo test --locked --workspace --exclude zeroclaw-desktop --verbose"
    run_in_ci "bash tests/manual/test_dockerignore.sh"
    run_in_ci "cargo build --release --locked --verbose"
    run_in_ci "cargo deny check licenses sources"
    run_in_ci "cargo audit"
    build_smoke_image
    docker run --rm zeroclaw-local-smoke:latest --version
    ;;

  clean)
    "${compose_cmd[@]}" down -v --remove-orphans
    ;;

  *)
    print_help
    exit 1
    ;;
esac
