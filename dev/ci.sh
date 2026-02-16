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

run_in_ci() {
  local cmd="$1"
  "${compose_cmd[@]}" run --rm local-ci bash -c "$cmd"
}

print_help() {
  cat <<'EOF'
ZeroClaw Local CI in Docker

Usage: ./dev/ci.sh <command>

Commands:
  build-image   Build/update the local CI image
  shell         Open an interactive shell inside the CI container
  lint          Run rustfmt + clippy (container only)
  test          Run cargo test (container only)
  build         Run release build smoke check (container only)
  audit         Run cargo audit (container only)
  deny          Run cargo deny check (container only)
  security      Run cargo audit + cargo deny (container only)
  docker-smoke  Build and verify runtime image (host docker daemon)
  all           Run lint, test, build, security, docker-smoke
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
    run_in_ci "cargo fmt --all -- --check && cargo clippy --locked --all-targets -- -D clippy::correctness"
    ;;

  test)
    run_in_ci "cargo test --locked --verbose"
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
    docker build --target dev -t zeroclaw-local-smoke:latest .
    docker run --rm zeroclaw-local-smoke:latest --version
    ;;

  all)
    run_in_ci "cargo fmt --all -- --check && cargo clippy --locked --all-targets -- -D clippy::correctness"
    run_in_ci "cargo test --locked --verbose"
    run_in_ci "cargo build --release --locked --verbose"
    run_in_ci "cargo deny check licenses sources"
    run_in_ci "cargo audit"
    docker build --target dev -t zeroclaw-local-smoke:latest .
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
