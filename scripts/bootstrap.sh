#!/usr/bin/env bash
set -euo pipefail

info() {
  echo "==> $*"
}

warn() {
  echo "warning: $*" >&2
}

error() {
  echo "error: $*" >&2
}

usage() {
  cat <<'USAGE'
ZeroClaw one-click bootstrap

Usage:
  ./bootstrap.sh [options]

Modes:
  Default mode installs/builds ZeroClaw only (requires existing Rust toolchain).
  Optional bootstrap mode can also install system dependencies and Rust.

Options:
  --docker                   Run bootstrap in Docker and launch onboarding inside the container
  --install-system-deps      Install build dependencies (Linux/macOS)
  --install-rust             Install Rust via rustup if missing
  --onboard                  Run onboarding after install
  --interactive-onboard      Run interactive onboarding (implies --onboard)
  --api-key <key>            API key for non-interactive onboarding
  --provider <id>            Provider for non-interactive onboarding (default: openrouter)
  --model <id>               Model for non-interactive onboarding (optional)
  --skip-build               Skip `cargo build --release --locked`
  --skip-install             Skip `cargo install --path . --force --locked`
  -h, --help                 Show help

Examples:
  ./bootstrap.sh
  ./bootstrap.sh --docker
  ./bootstrap.sh --install-system-deps --install-rust
  ./bootstrap.sh --onboard --api-key "sk-..." --provider openrouter [--model "openrouter/auto"]
  ./bootstrap.sh --interactive-onboard

  # Remote one-liner
  curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash

Environment:
  ZEROCLAW_DOCKER_DATA_DIR   Host path for Docker config/workspace persistence
  ZEROCLAW_DOCKER_IMAGE      Docker image tag to build/run (default: zeroclaw-bootstrap:local)
  ZEROCLAW_API_KEY           Used when --api-key is not provided
  ZEROCLAW_PROVIDER          Used when --provider is not provided (default: openrouter)
  ZEROCLAW_MODEL             Used when --model is not provided
USAGE
}

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

run_privileged() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  elif have_cmd sudo; then
    sudo "$@"
  else
    error "sudo is required to install system dependencies."
    return 1
  fi
}

install_system_deps() {
  info "Installing system dependencies"

  case "$(uname -s)" in
    Linux)
      if have_cmd apt-get; then
        run_privileged apt-get update -qq
        run_privileged apt-get install -y build-essential pkg-config git curl
      elif have_cmd dnf; then
        run_privileged dnf group install -y development-tools
        run_privileged dnf install -y pkg-config git curl
      else
        warn "Unsupported Linux distribution. Install compiler toolchain + pkg-config + git + curl manually."
      fi
      ;;
    Darwin)
      if ! xcode-select -p >/dev/null 2>&1; then
        info "Installing Xcode Command Line Tools"
        xcode-select --install || true
        cat <<'MSG'
Please complete the Xcode Command Line Tools installation dialog,
then re-run bootstrap.
MSG
        exit 0
      fi
      if ! have_cmd git; then
        warn "git is not available. Install git (e.g., Homebrew) and re-run bootstrap."
      fi
      ;;
    *)
      warn "Unsupported OS for automatic dependency install. Continuing without changes."
      ;;
  esac
}

install_rust_toolchain() {
  if have_cmd cargo && have_cmd rustc; then
    info "Rust already installed: $(rustc --version)"
    return
  fi

  if ! have_cmd curl; then
    error "curl is required to install Rust via rustup."
    exit 1
  fi

  info "Installing Rust via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

  if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env"
  fi

  if ! have_cmd cargo; then
    error "Rust installation completed but cargo is still unavailable in PATH."
    error "Run: source \"$HOME/.cargo/env\""
    exit 1
  fi
}

ensure_docker_ready() {
  if ! have_cmd docker; then
    error "docker is not installed."
    cat <<'MSG' >&2
Install Docker first, then re-run with:
  ./bootstrap.sh --docker
MSG
    exit 1
  fi

  if ! docker info >/dev/null 2>&1; then
    error "Docker daemon is not reachable."
    error "Start Docker and re-run bootstrap."
    exit 1
  fi
}

run_docker_bootstrap() {
  local docker_image docker_data_dir default_data_dir
  docker_image="${ZEROCLAW_DOCKER_IMAGE:-zeroclaw-bootstrap:local}"
  if [[ "$TEMP_CLONE" == true ]]; then
    default_data_dir="$HOME/.zeroclaw-docker"
  else
    default_data_dir="$WORK_DIR/.zeroclaw-docker"
  fi
  docker_data_dir="${ZEROCLAW_DOCKER_DATA_DIR:-$default_data_dir}"
  DOCKER_DATA_DIR="$docker_data_dir"

  mkdir -p "$docker_data_dir/.zeroclaw" "$docker_data_dir/workspace"

  if [[ "$SKIP_INSTALL" == true ]]; then
    warn "--skip-install has no effect with --docker."
  fi

  if [[ "$SKIP_BUILD" == false ]]; then
    info "Building Docker image ($docker_image)"
    docker build --target release -t "$docker_image" "$WORK_DIR"
  else
    info "Skipping Docker image build"
  fi

  info "Docker data directory: $docker_data_dir"

  local onboard_cmd=()
  if [[ "$INTERACTIVE_ONBOARD" == true ]]; then
    info "Launching interactive onboarding in container"
    onboard_cmd=(onboard --interactive)
  else
    if [[ -z "$API_KEY" ]]; then
      cat <<'MSG'
==> Onboarding requested, but API key not provided.
Use either:
  --api-key "sk-..."
or:
  ZEROCLAW_API_KEY="sk-..." ./bootstrap.sh --docker
or run interactive:
  ./bootstrap.sh --docker --interactive-onboard
MSG
      exit 1
    fi
    if [[ -n "$MODEL" ]]; then
      info "Launching quick onboarding in container (provider: $PROVIDER, model: $MODEL)"
    else
      info "Launching quick onboarding in container (provider: $PROVIDER)"
    fi
    onboard_cmd=(onboard --api-key "$API_KEY" --provider "$PROVIDER")
    if [[ -n "$MODEL" ]]; then
      onboard_cmd+=(--model "$MODEL")
    fi
  fi

  docker run --rm -it \
    --user "$(id -u):$(id -g)" \
    -e HOME=/zeroclaw-data \
    -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
    -v "$docker_data_dir/.zeroclaw:/zeroclaw-data/.zeroclaw" \
    -v "$docker_data_dir/workspace:/zeroclaw-data/workspace" \
    "$docker_image" \
    "${onboard_cmd[@]}"
}

SCRIPT_PATH="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" >/dev/null 2>&1 && pwd || pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd || pwd)"
REPO_URL="https://github.com/zeroclaw-labs/zeroclaw.git"

DOCKER_MODE=false
INSTALL_SYSTEM_DEPS=false
INSTALL_RUST=false
RUN_ONBOARD=false
INTERACTIVE_ONBOARD=false
SKIP_BUILD=false
SKIP_INSTALL=false
API_KEY="${ZEROCLAW_API_KEY:-}"
PROVIDER="${ZEROCLAW_PROVIDER:-openrouter}"
MODEL="${ZEROCLAW_MODEL:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --docker)
      DOCKER_MODE=true
      shift
      ;;
    --install-system-deps)
      INSTALL_SYSTEM_DEPS=true
      shift
      ;;
    --install-rust)
      INSTALL_RUST=true
      shift
      ;;
    --onboard)
      RUN_ONBOARD=true
      shift
      ;;
    --interactive-onboard)
      RUN_ONBOARD=true
      INTERACTIVE_ONBOARD=true
      shift
      ;;
    --api-key)
      API_KEY="${2:-}"
      [[ -n "$API_KEY" ]] || {
        error "--api-key requires a value"
        exit 1
      }
      shift 2
      ;;
    --provider)
      PROVIDER="${2:-}"
      [[ -n "$PROVIDER" ]] || {
        error "--provider requires a value"
        exit 1
      }
      shift 2
      ;;
    --model)
      MODEL="${2:-}"
      [[ -n "$MODEL" ]] || {
        error "--model requires a value"
        exit 1
      }
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=true
      shift
      ;;
    --skip-install)
      SKIP_INSTALL=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      error "unknown option: $1"
      echo
      usage
      exit 1
      ;;
  esac
done

if [[ "$DOCKER_MODE" == true ]]; then
  if [[ "$INSTALL_SYSTEM_DEPS" == true ]]; then
    warn "--install-system-deps is ignored with --docker."
  fi
  if [[ "$INSTALL_RUST" == true ]]; then
    warn "--install-rust is ignored with --docker."
  fi
else
  if [[ "$INSTALL_SYSTEM_DEPS" == true ]]; then
    install_system_deps
  fi

  if [[ "$INSTALL_RUST" == true ]]; then
    install_rust_toolchain
  fi
fi

if [[ "$DOCKER_MODE" == false ]] && ! have_cmd cargo; then
  error "cargo is not installed."
  cat <<'MSG' >&2
Install Rust first: https://rustup.rs/
or re-run with:
  ./bootstrap.sh --install-rust
MSG
  exit 1
fi

WORK_DIR="$ROOT_DIR"
TEMP_CLONE=false
TEMP_DIR=""

cleanup() {
  if [[ "$TEMP_CLONE" == true && -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
    rm -rf "$TEMP_DIR"
  fi
}
trap cleanup EXIT

# Support three launch modes:
# 1) ./bootstrap.sh from repo root
# 2) scripts/bootstrap.sh from repo
# 3) curl | bash (no local repo => temporary clone)
if [[ ! -f "$WORK_DIR/Cargo.toml" ]]; then
  if [[ -f "$(pwd)/Cargo.toml" ]]; then
    WORK_DIR="$(pwd)"
  else
    if ! have_cmd git; then
      error "git is required when running bootstrap outside a local repository checkout."
      if [[ "$INSTALL_SYSTEM_DEPS" == false ]]; then
        error "Re-run with --install-system-deps or install git manually."
      fi
      exit 1
    fi

    TEMP_DIR="$(mktemp -d -t zeroclaw-bootstrap-XXXXXX)"
    info "No local repository detected; cloning latest main branch"
    git clone --depth 1 "$REPO_URL" "$TEMP_DIR"
    WORK_DIR="$TEMP_DIR"
    TEMP_CLONE=true
  fi
fi

info "ZeroClaw bootstrap"
echo "    workspace: $WORK_DIR"

cd "$WORK_DIR"

if [[ "$DOCKER_MODE" == true ]]; then
  ensure_docker_ready
  if [[ "$RUN_ONBOARD" == false ]]; then
    RUN_ONBOARD=true
    if [[ -z "$API_KEY" ]]; then
      INTERACTIVE_ONBOARD=true
    fi
  fi
  run_docker_bootstrap
  cat <<'DONE'

✅ Docker bootstrap complete.

Your containerized ZeroClaw data is persisted under:
DONE
  echo "  $DOCKER_DATA_DIR"
  cat <<'DONE'

Next steps:
  ./bootstrap.sh --docker --interactive-onboard
  ./bootstrap.sh --docker --api-key "sk-..." --provider openrouter
DONE
  exit 0
fi

if [[ "$SKIP_BUILD" == false ]]; then
  info "Building release binary"
  cargo build --release --locked
else
  info "Skipping build"
fi

if [[ "$SKIP_INSTALL" == false ]]; then
  info "Installing zeroclaw to cargo bin"
  cargo install --path "$WORK_DIR" --force --locked
else
  info "Skipping install"
fi

ZEROCLAW_BIN=""
if have_cmd zeroclaw; then
  ZEROCLAW_BIN="zeroclaw"
elif [[ -x "$WORK_DIR/target/release/zeroclaw" ]]; then
  ZEROCLAW_BIN="$WORK_DIR/target/release/zeroclaw"
fi

if [[ "$RUN_ONBOARD" == true ]]; then
  if [[ -z "$ZEROCLAW_BIN" ]]; then
    error "onboarding requested but zeroclaw binary is not available."
    error "Run without --skip-install, or ensure zeroclaw is in PATH."
    exit 1
  fi

  if [[ "$INTERACTIVE_ONBOARD" == true ]]; then
    info "Running interactive onboarding"
    "$ZEROCLAW_BIN" onboard --interactive
  else
    if [[ -z "$API_KEY" ]]; then
      cat <<'MSG'
==> Onboarding requested, but API key not provided.
Use either:
  --api-key "sk-..."
or:
  ZEROCLAW_API_KEY="sk-..." ./bootstrap.sh --onboard
or run interactive:
  ./bootstrap.sh --interactive-onboard
MSG
      exit 1
    fi
    if [[ -n "$MODEL" ]]; then
      info "Running quick onboarding (provider: $PROVIDER, model: $MODEL)"
    else
      info "Running quick onboarding (provider: $PROVIDER)"
    fi
    ONBOARD_CMD=("$ZEROCLAW_BIN" onboard --api-key "$API_KEY" --provider "$PROVIDER")
    if [[ -n "$MODEL" ]]; then
      ONBOARD_CMD+=(--model "$MODEL")
    fi
    "${ONBOARD_CMD[@]}"
  fi
fi

cat <<'DONE'

✅ Bootstrap complete.

Next steps:
  zeroclaw status
  zeroclaw agent -m "Hello, ZeroClaw!"
  zeroclaw gateway
DONE
