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
ZeroClaw installer bootstrap engine

Usage:
  ./zeroclaw_install.sh [options]
  ./bootstrap.sh [options]         # compatibility entrypoint

Modes:
  Default mode installs/builds ZeroClaw only (requires existing Rust toolchain).
  Guided mode asks setup questions and configures options interactively.
  Optional bootstrap mode can also install system dependencies and Rust.

Options:
  --guided                   Run interactive guided installer
  --no-guided                Disable guided installer
  --docker                   Run bootstrap in Docker-compatible mode and launch onboarding inside the container
  --docker-reset             Reset existing ZeroClaw Docker containers/networks/volumes and data dir before --docker bootstrap
  --docker-config <path>     Seed Docker config.toml from host path (skips default onboarding unless explicitly requested)
  --docker-secret-key <path> Seed Docker .secret_key from host path (used with --docker-config encrypted secrets)
  --docker-daemon            Start persistent Docker daemon container directly (requires --docker)
  --install-system-deps      Install build dependencies (Linux/macOS)
  --install-rust             Install Rust via rustup if missing
  --prefer-prebuilt          Try latest release binary first; fallback to source build on miss
  --prebuilt-only            Install only from latest release binary (no source build fallback)
  --force-source-build       Disable prebuilt flow and always build from source
  --onboard                  Run onboarding after install
  --interactive-onboard      Run interactive onboarding (implies --onboard)
  --api-key <key>            API key for non-interactive onboarding
  --provider <id>            Provider for non-interactive onboarding (default: openrouter)
  --model <id>               Model for non-interactive onboarding (optional)
  --build-first              Alias for explicitly enabling separate `cargo build --release --locked`
  --skip-build               Skip build step (`cargo build --release --locked` or Docker image build)
  --skip-install             Skip `cargo install --path . --force --locked`
  -h, --help                 Show help

Examples:
  ./zeroclaw_install.sh
  ./zeroclaw_install.sh --guided
  ./zeroclaw_install.sh --install-system-deps --install-rust
  ./zeroclaw_install.sh --prefer-prebuilt
  ./zeroclaw_install.sh --prebuilt-only
  ./zeroclaw_install.sh --onboard --api-key "sk-..." --provider openrouter [--model "openrouter/auto"]
  ./zeroclaw_install.sh --interactive-onboard
  ./zeroclaw_install.sh --docker --docker-config ./config.toml --docker-daemon

  # Compatibility entrypoint:
  ./bootstrap.sh --docker

  # Remote one-liner
  curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash

Environment:
  ZEROCLAW_CONTAINER_CLI     Container CLI command (default: docker; auto-fallback: podman)
  ZEROCLAW_DOCKER_DATA_DIR   Host path for Docker config/workspace persistence
  ZEROCLAW_DOCKER_IMAGE      Docker image tag to build/run (default: zeroclaw-bootstrap:local)
  ZEROCLAW_DOCKER_BROWSER_RUNTIME
                            Browser runtime provisioning mode for --docker: "auto" (prompt), "on", or "off"
  ZEROCLAW_DOCKER_BROWSER_SIDECAR_IMAGE
                            Browser WebDriver sidecar image (default: selenium/standalone-chromium:latest)
  ZEROCLAW_DOCKER_BROWSER_SIDECAR_NAME
                            Browser WebDriver sidecar container name (default: zeroclaw-browser-webdriver)
  ZEROCLAW_DOCKER_NETWORK    Docker network for ZeroClaw + sidecars (default: zeroclaw-bootstrap-net)
  ZEROCLAW_DOCKER_CARGO_FEATURES
                            Extra Cargo features for Docker builds (comma-separated)
  ZEROCLAW_DOCKER_DAEMON_NAME
                            Daemon container name for --docker-daemon (default: zeroclaw-daemon)
  ZEROCLAW_DOCKER_DAEMON_BIND_HOST
                            Host bind address for daemon port publish (default: 127.0.0.1)
  ZEROCLAW_DOCKER_DAEMON_HOST_PORT
                            Host port to publish daemon gateway (default: same as gateway.port)
  ZEROCLAW_DOCKER_SECRET_KEY_FILE
                            Host path to .secret_key used when seeding encrypted config.toml
  ZEROCLAW_API_KEY           Used when --api-key is not provided
  ZEROCLAW_PROVIDER          Used when --provider is not provided (default: openrouter)
  ZEROCLAW_MODEL             Used when --model is not provided
  ZEROCLAW_BOOTSTRAP_MIN_RAM_MB   Minimum RAM threshold for source build preflight (default: 2048)
  ZEROCLAW_BOOTSTRAP_MIN_DISK_MB  Minimum free disk threshold for source build preflight (default: 6144)
  ZEROCLAW_DISABLE_ALPINE_AUTO_DEPS
                            Set to 1 to disable Alpine auto-install of missing prerequisites
USAGE
}

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

get_total_memory_mb() {
  case "$(uname -s)" in
    Linux)
      if [[ -r /proc/meminfo ]]; then
        awk '/MemTotal:/ {printf "%d\n", $2 / 1024}' /proc/meminfo
      fi
      ;;
    Darwin)
      if have_cmd sysctl; then
        local bytes
        bytes="$(sysctl -n hw.memsize 2>/dev/null || true)"
        if [[ "$bytes" =~ ^[0-9]+$ ]]; then
          echo $((bytes / 1024 / 1024))
        fi
      fi
      ;;
  esac
}

get_available_disk_mb() {
  local path="${1:-.}"
  local free_kb
  free_kb="$(df -Pk "$path" 2>/dev/null | awk 'NR==2 {print $4}')"
  if [[ "$free_kb" =~ ^[0-9]+$ ]]; then
    echo $((free_kb / 1024))
  fi
}

detect_release_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os:$arch" in
    Linux:x86_64)
      echo "x86_64-unknown-linux-gnu"
      ;;
    Linux:aarch64|Linux:arm64)
      echo "aarch64-unknown-linux-gnu"
      ;;
    Linux:armv7l|Linux:armv6l)
      echo "armv7-unknown-linux-gnueabihf"
      ;;
    Darwin:x86_64)
      echo "x86_64-apple-darwin"
      ;;
    Darwin:arm64|Darwin:aarch64)
      echo "aarch64-apple-darwin"
      ;;
    *)
      return 1
      ;;
  esac
}

should_attempt_prebuilt_for_resources() {
  local workspace="${1:-.}"
  local min_ram_mb min_disk_mb total_ram_mb free_disk_mb low_resource

  min_ram_mb="${ZEROCLAW_BOOTSTRAP_MIN_RAM_MB:-2048}"
  min_disk_mb="${ZEROCLAW_BOOTSTRAP_MIN_DISK_MB:-6144}"
  total_ram_mb="$(get_total_memory_mb || true)"
  free_disk_mb="$(get_available_disk_mb "$workspace" || true)"
  low_resource=false

  if [[ "$total_ram_mb" =~ ^[0-9]+$ && "$total_ram_mb" -lt "$min_ram_mb" ]]; then
    low_resource=true
  fi
  if [[ "$free_disk_mb" =~ ^[0-9]+$ && "$free_disk_mb" -lt "$min_disk_mb" ]]; then
    low_resource=true
  fi

  if [[ "$low_resource" == true ]]; then
    warn "Source build preflight indicates constrained resources."
    if [[ "$total_ram_mb" =~ ^[0-9]+$ ]]; then
      warn "Detected RAM: ${total_ram_mb}MB (recommended >= ${min_ram_mb}MB for local source builds)."
    else
      warn "Unable to detect total RAM automatically."
    fi
    if [[ "$free_disk_mb" =~ ^[0-9]+$ ]]; then
      warn "Detected free disk: ${free_disk_mb}MB (recommended >= ${min_disk_mb}MB)."
    else
      warn "Unable to detect free disk space automatically."
    fi
    return 0
  fi

  return 1
}

install_prebuilt_binary() {
  local target archive_url temp_dir archive_path extracted_bin install_dir

  if ! have_cmd curl; then
    warn "curl is required for pre-built binary installation."
    return 1
  fi
  if ! have_cmd tar; then
    warn "tar is required for pre-built binary installation."
    return 1
  fi

  target="$(detect_release_target || true)"
  if [[ -z "$target" ]]; then
    warn "No pre-built binary target mapping for $(uname -s)/$(uname -m)."
    return 1
  fi

  archive_url="https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-${target}.tar.gz"
  temp_dir="$(mktemp -d -t zeroclaw-prebuilt-XXXXXX)"
  archive_path="$temp_dir/zeroclaw-${target}.tar.gz"

  info "Attempting pre-built binary install for target: $target"
  if ! curl -fsSL "$archive_url" -o "$archive_path"; then
    warn "Could not download release asset: $archive_url"
    rm -rf "$temp_dir"
    return 1
  fi

  if ! tar -xzf "$archive_path" -C "$temp_dir"; then
    warn "Failed to extract pre-built archive."
    rm -rf "$temp_dir"
    return 1
  fi

  extracted_bin="$temp_dir/zeroclaw"
  if [[ ! -x "$extracted_bin" ]]; then
    extracted_bin="$(find "$temp_dir" -maxdepth 2 -type f -name zeroclaw -perm -u+x | head -n 1 || true)"
  fi
  if [[ -z "$extracted_bin" || ! -x "$extracted_bin" ]]; then
    warn "Archive did not contain an executable zeroclaw binary."
    rm -rf "$temp_dir"
    return 1
  fi

  install_dir="$HOME/.cargo/bin"
  mkdir -p "$install_dir"
  install -m 0755 "$extracted_bin" "$install_dir/zeroclaw"
  rm -rf "$temp_dir"

  info "Installed pre-built binary to $install_dir/zeroclaw"
  if [[ ":$PATH:" != *":$install_dir:"* ]]; then
    warn "$install_dir is not in PATH for this shell."
    warn "Run: export PATH=\"$install_dir:\$PATH\""
  fi

  return 0
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

is_container_runtime() {
  if [[ -f /.dockerenv || -f /run/.containerenv ]]; then
    return 0
  fi

  if [[ -r /proc/1/cgroup ]] && grep -Eq '(docker|containerd|kubepods|podman|lxc)' /proc/1/cgroup; then
    return 0
  fi

  return 1
}

run_pacman() {
  if ! have_cmd pacman; then
    error "pacman is not available."
    return 1
  fi

  if ! is_container_runtime; then
    run_privileged pacman "$@"
    return $?
  fi

  local pacman_cfg_tmp=""
  local pacman_rc=0
  pacman_cfg_tmp="$(mktemp /tmp/zeroclaw-pacman.XXXXXX.conf)"
  cp /etc/pacman.conf "$pacman_cfg_tmp"
  if ! grep -Eq '^[[:space:]]*DisableSandboxSyscalls([[:space:]]|$)' "$pacman_cfg_tmp"; then
    printf '\nDisableSandboxSyscalls\n' >> "$pacman_cfg_tmp"
  fi

  if run_privileged pacman --config "$pacman_cfg_tmp" "$@"; then
    pacman_rc=0
  else
    pacman_rc=$?
  fi

  rm -f "$pacman_cfg_tmp"
  return "$pacman_rc"
}

ALPINE_PREREQ_PACKAGES=(
  bash
  build-base
  pkgconf
  git
  curl
  openssl-dev
  perl
  ca-certificates
)
ALPINE_MISSING_PKGS=()

find_missing_alpine_prereqs() {
  ALPINE_MISSING_PKGS=()
  if ! have_cmd apk; then
    return 0
  fi

  local pkg=""
  for pkg in "${ALPINE_PREREQ_PACKAGES[@]}"; do
    if ! apk info -e "$pkg" >/dev/null 2>&1; then
      ALPINE_MISSING_PKGS+=("$pkg")
    fi
  done
}

bool_to_word() {
  if [[ "$1" == true ]]; then
    echo "yes"
  else
    echo "no"
  fi
}

string_to_bool() {
  local value
  value="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
  case "$value" in
    1|true|yes|on)
      echo "true"
      ;;
    0|false|no|off)
      echo "false"
      ;;
    *)
      echo "invalid"
      ;;
  esac
}

guided_input_stream() {
  if [[ -t 0 ]]; then
    echo "/dev/stdin"
    return 0
  fi

  if (: </dev/tty) 2>/dev/null; then
    echo "/dev/tty"
    return 0
  fi

  return 1
}

guided_read() {
  local __target_var="$1"
  local __prompt="$2"
  local __silent="${3:-false}"
  local __input_source=""
  local __value=""

  if ! __input_source="$(guided_input_stream)"; then
    return 1
  fi

  if [[ "$__silent" == true ]]; then
    if ! read -r -s -p "$__prompt" __value <"$__input_source"; then
      return 1
    fi
  else
    if ! read -r -p "$__prompt" __value <"$__input_source"; then
      return 1
    fi
  fi

  printf -v "$__target_var" '%s' "$__value"
  return 0
}

prompt_yes_no() {
  local question="$1"
  local default_answer="$2"
  local prompt=""
  local answer=""

  if [[ "$default_answer" == "yes" ]]; then
    prompt="[Y/n]"
  else
    prompt="[y/N]"
  fi

  while true; do
    if ! guided_read answer "$question $prompt "; then
      error "guided installer input was interrupted."
      exit 1
    fi
    answer="${answer:-$default_answer}"
    case "$(printf '%s' "$answer" | tr '[:upper:]' '[:lower:]')" in
      y|yes)
        return 0
        ;;
      n|no)
        return 1
        ;;
      *)
        echo "Please answer yes or no."
        ;;
    esac
  done
}

install_system_deps() {
  info "Installing system dependencies"

  case "$(uname -s)" in
    Linux)
      if have_cmd apk; then
        find_missing_alpine_prereqs
        if [[ ${#ALPINE_MISSING_PKGS[@]} -eq 0 ]]; then
          info "Alpine prerequisites already installed"
        else
          info "Installing Alpine prerequisites: ${ALPINE_MISSING_PKGS[*]}"
          run_privileged apk add --no-cache "${ALPINE_MISSING_PKGS[@]}"
        fi
      elif have_cmd apt-get; then
        run_privileged apt-get update -qq
        run_privileged apt-get install -y build-essential pkg-config git curl
      elif have_cmd dnf; then
        run_privileged dnf install -y \
          gcc \
          gcc-c++ \
          make \
          pkgconf-pkg-config \
          git \
          curl \
          openssl-devel \
          perl
      elif have_cmd pacman; then
        run_pacman -Sy --noconfirm
        run_pacman -S --noconfirm --needed \
          gcc \
          make \
          pkgconf \
          git \
          curl \
          openssl \
          perl \
          ca-certificates
      else
        warn "Unsupported Linux distribution. Install compiler toolchain + pkg-config + git + curl + OpenSSL headers + perl manually."
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

run_guided_installer() {
  local os_name="$1"
  local provider_input=""
  local model_input=""
  local api_key_input=""

  if ! guided_input_stream >/dev/null; then
    error "guided installer requires an interactive terminal."
    error "Run from a terminal, or pass --no-guided with explicit flags."
    exit 1
  fi

  echo
  echo "ZeroClaw guided installer"
  echo "Answer a few questions, then the installer will run automatically."
  echo

  if [[ "$os_name" == "Linux" ]]; then
    if prompt_yes_no "Install Linux build dependencies (toolchain/pkg-config/git/curl)?" "yes"; then
      INSTALL_SYSTEM_DEPS=true
    fi
  else
    if prompt_yes_no "Install system dependencies for $os_name?" "no"; then
      INSTALL_SYSTEM_DEPS=true
    fi
  fi

  if have_cmd cargo && have_cmd rustc; then
    info "Detected Rust toolchain: $(rustc --version)"
  else
    if prompt_yes_no "Rust toolchain not found. Install Rust via rustup now?" "yes"; then
      INSTALL_RUST=true
    fi
  fi

  if prompt_yes_no "Run a separate prebuild before install?" "yes"; then
    SKIP_BUILD=false
  else
    SKIP_BUILD=true
  fi

  if prompt_yes_no "Install zeroclaw into cargo bin now?" "yes"; then
    SKIP_INSTALL=false
  else
    SKIP_INSTALL=true
  fi

  if prompt_yes_no "Run onboarding after install?" "no"; then
    RUN_ONBOARD=true
    if prompt_yes_no "Use interactive onboarding?" "yes"; then
      INTERACTIVE_ONBOARD=true
    else
      INTERACTIVE_ONBOARD=false
      if ! guided_read provider_input "Provider [$PROVIDER]: "; then
        error "guided installer input was interrupted."
        exit 1
      fi
      if [[ -n "$provider_input" ]]; then
        PROVIDER="$provider_input"
      fi

      if ! guided_read model_input "Model [${MODEL:-leave empty}]: "; then
        error "guided installer input was interrupted."
        exit 1
      fi
      if [[ -n "$model_input" ]]; then
        MODEL="$model_input"
      fi

      if [[ -z "$API_KEY" ]]; then
        if ! guided_read api_key_input "API key (hidden, leave empty to switch to interactive onboarding): " true; then
          echo
          error "guided installer input was interrupted."
          exit 1
        fi
        echo
        if [[ -n "$api_key_input" ]]; then
          API_KEY="$api_key_input"
        else
          warn "No API key entered. Using interactive onboarding instead."
          INTERACTIVE_ONBOARD=true
        fi
      fi
    fi
  fi

  echo
  info "Installer plan"
  local install_binary=true
  local build_first=false
  if [[ "$SKIP_INSTALL" == true ]]; then
    install_binary=false
  fi
  if [[ "$SKIP_BUILD" == false ]]; then
    build_first=true
  fi
  echo "    docker-mode: $(bool_to_word "$DOCKER_MODE")"
  echo "    install-system-deps: $(bool_to_word "$INSTALL_SYSTEM_DEPS")"
  echo "    install-rust: $(bool_to_word "$INSTALL_RUST")"
  echo "    build-first: $(bool_to_word "$build_first")"
  echo "    install-binary: $(bool_to_word "$install_binary")"
  echo "    onboard: $(bool_to_word "$RUN_ONBOARD")"
  if [[ "$RUN_ONBOARD" == true ]]; then
    echo "    interactive-onboard: $(bool_to_word "$INTERACTIVE_ONBOARD")"
    if [[ "$INTERACTIVE_ONBOARD" == false ]]; then
      echo "    provider: $PROVIDER"
      if [[ -n "$MODEL" ]]; then
        echo "    model: $MODEL"
      fi
    fi
  fi

  echo
  if ! prompt_yes_no "Proceed with this install plan?" "yes"; then
    info "Installation canceled by user."
    exit 0
  fi
}

resolve_container_cli() {
  local requested_cli
  requested_cli="${ZEROCLAW_CONTAINER_CLI:-docker}"

  if have_cmd "$requested_cli"; then
    CONTAINER_CLI="$requested_cli"
    return 0
  fi

  if [[ "$requested_cli" == "docker" ]] && have_cmd podman; then
    warn "docker CLI not found; falling back to podman."
    CONTAINER_CLI="podman"
    return 0
  fi

  error "Container CLI '$requested_cli' is not installed."
  if [[ "$requested_cli" != "docker" ]]; then
    error "Set ZEROCLAW_CONTAINER_CLI to an installed Docker-compatible CLI (e.g., docker or podman)."
  else
    error "Install Docker, install podman, or set ZEROCLAW_CONTAINER_CLI to an available Docker-compatible CLI."
  fi
  exit 1
}

ensure_docker_ready() {
  resolve_container_cli

  if ! "$CONTAINER_CLI" info >/dev/null 2>&1; then
    error "Container runtime is not reachable via '$CONTAINER_CLI'."
    error "Start the container runtime and re-run bootstrap."
    exit 1
  fi
}

is_zeroclaw_container() {
  local name="$1"
  local image="$2"
  local command="$3"
  local name_lc image_lc command_lc

  name_lc="$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')"
  image_lc="$(printf '%s' "$image" | tr '[:upper:]' '[:lower:]')"
  command_lc="$(printf '%s' "$command" | tr '[:upper:]' '[:lower:]')"

  [[ "$name_lc" == *"zeroclaw"* || "$image_lc" == *"zeroclaw"* || "$command_lc" == *"zeroclaw"* ]]
}

is_zeroclaw_resource_name() {
  local name="$1"
  local name_lc
  name_lc="$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')"
  [[ "$name_lc" == *"zeroclaw"* ]]
}

maybe_stop_running_zeroclaw_containers() {
  local -a running_ids running_rows
  local id name image command row

  while IFS=$'\t' read -r id name image command; do
    if [[ -z "$id" ]]; then
      continue
    fi
    if is_zeroclaw_container "$name" "$image" "$command"; then
      running_ids+=("$id")
      running_rows+=("$id"$'\t'"$name"$'\t'"$image"$'\t'"$command")
    fi
  done < <("$CONTAINER_CLI" ps --format '{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Command}}')

  if [[ ${#running_ids[@]} -eq 0 ]]; then
    return 0
  fi

  warn "Detected running ZeroClaw container(s):"
  for row in "${running_rows[@]}"; do
    IFS=$'\t' read -r id name image command <<<"$row"
    echo "  - $name ($id) image=$image cmd=$command"
  done

  if ! guided_input_stream >/dev/null 2>&1; then
    warn "Non-interactive mode detected; leaving existing ZeroClaw containers running."
    return 0
  fi

  if prompt_yes_no "Stop these running ZeroClaw containers before continuing?" "yes"; then
    info "Stopping ${#running_ids[@]} ZeroClaw container(s)"
    "$CONTAINER_CLI" stop "${running_ids[@]}" >/dev/null
  else
    warn "Continuing with existing ZeroClaw containers still running."
  fi
}

reset_docker_zeroclaw_resources() {
  local docker_data_dir="$1"
  local -a container_ids container_rows network_names volume_names
  local id name image command row resource_name

  container_ids=()
  container_rows=()
  network_names=()
  volume_names=()

  info "Resetting ZeroClaw Docker resources"

  while IFS=$'\t' read -r id name image command; do
    if [[ -z "$id" ]]; then
      continue
    fi
    if is_zeroclaw_container "$name" "$image" "$command"; then
      container_ids+=("$id")
      container_rows+=("$id"$'\t'"$name"$'\t'"$image"$'\t'"$command")
    fi
  done < <("$CONTAINER_CLI" ps -a --format '{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Command}}')

  if [[ ${#container_ids[@]} -gt 0 ]]; then
    info "Removing ${#container_ids[@]} ZeroClaw container(s)"
    for row in "${container_rows[@]}"; do
      IFS=$'\t' read -r id name image command <<<"$row"
      echo "  - $name ($id) image=$image cmd=$command"
    done
    "$CONTAINER_CLI" rm -f "${container_ids[@]}" >/dev/null
  else
    info "No existing ZeroClaw containers found"
  fi

  while IFS= read -r resource_name; do
    if [[ -z "$resource_name" ]]; then
      continue
    fi
    if is_zeroclaw_resource_name "$resource_name"; then
      network_names+=("$resource_name")
    fi
  done < <("$CONTAINER_CLI" network ls --format '{{.Name}}')

  if [[ ${#network_names[@]} -gt 0 ]]; then
    info "Removing ${#network_names[@]} ZeroClaw network(s)"
    for resource_name in "${network_names[@]}"; do
      echo "  - $resource_name"
      if ! "$CONTAINER_CLI" network rm "$resource_name" >/dev/null 2>&1; then
        warn "Could not remove network '$resource_name' (it may still be in use)."
      fi
    done
  else
    info "No existing ZeroClaw networks found"
  fi

  while IFS= read -r resource_name; do
    if [[ -z "$resource_name" ]]; then
      continue
    fi
    if is_zeroclaw_resource_name "$resource_name"; then
      volume_names+=("$resource_name")
    fi
  done < <("$CONTAINER_CLI" volume ls --format '{{.Name}}')

  if [[ ${#volume_names[@]} -gt 0 ]]; then
    info "Removing ${#volume_names[@]} ZeroClaw volume(s)"
    for resource_name in "${volume_names[@]}"; do
      echo "  - $resource_name"
      if ! "$CONTAINER_CLI" volume rm "$resource_name" >/dev/null 2>&1; then
        warn "Could not remove volume '$resource_name' (it may still be in use)."
      fi
    done
  else
    info "No existing ZeroClaw volumes found"
  fi

  if [[ -d "$docker_data_dir" ]]; then
    info "Removing Docker data directory ($docker_data_dir)"
    rm -rf "$docker_data_dir"
  else
    info "No Docker data directory to remove ($docker_data_dir)"
  fi
}

ensure_docker_network() {
  local network_name="$1"
  if "$CONTAINER_CLI" network inspect "$network_name" >/dev/null 2>&1; then
    return 0
  fi
  info "Creating Docker network ($network_name)"
  "$CONTAINER_CLI" network create "$network_name" >/dev/null
}

toml_section_value() {
  local file_path="$1"
  local section_name="$2"
  local key_name="$3"
  awk -v target_section="[$section_name]" -v target_key="$key_name" '
    function trim(s) {
      sub(/^[[:space:]]+/, "", s);
      sub(/[[:space:]]+$/, "", s);
      return s;
    }
    {
      line = $0;
      sub(/[[:space:]]*#.*/, "", line);
      line = trim(line);
      if (line == "") {
        next;
      }
      if (line ~ /^\[[^]]+\]$/) {
        section = line;
        next;
      }
      if (section != target_section) {
        next;
      }

      split_pos = index(line, "=");
      if (split_pos == 0) {
        next;
      }
      key = trim(substr(line, 1, split_pos - 1));
      if (key != target_key) {
        next;
      }
      value = trim(substr(line, split_pos + 1));
      print value;
      exit;
    }
  ' "$file_path"
}

strip_toml_quotes() {
  local value="$1"
  value="$(printf '%s' "$value" | tr -d '\r')"
  if [[ "$value" == \"*\" ]]; then
    value="${value#\"}"
    value="${value%\"}"
  fi
  printf '%s' "$value"
}

config_requests_webdriver_sidecar() {
  local config_path="$1"
  local enabled_raw backend_raw enabled backend

  enabled_raw="$(toml_section_value "$config_path" "browser" "enabled" || true)"
  backend_raw="$(toml_section_value "$config_path" "browser" "backend" || true)"

  enabled="$(printf '%s' "$enabled_raw" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')"
  backend="$(printf '%s' "$backend_raw" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')"
  backend="$(strip_toml_quotes "$backend")"

  if [[ "$enabled" != "true" ]]; then
    return 1
  fi

  case "$backend" in
    rust_native|auto)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

config_gateway_port() {
  local config_path="$1"
  local raw port
  raw="$(toml_section_value "$config_path" "gateway" "port" || true)"
  port="$(printf '%s' "$raw" | tr -cd '0-9')"
  if [[ "$port" =~ ^[0-9]+$ ]] && ((port >= 1 && port <= 65535)); then
    printf '%s' "$port"
  fi
}

config_has_encrypted_secrets() {
  local config_path="$1"
  grep -Eq "enc2:|enc:" "$config_path"
}

seed_docker_secret_key_for_config() {
  local source_config_path="$1"
  local target_config_dir="$2"
  local source_secret_key_override="${3:-}"
  local source_config_dir source_secret_key target_secret_key

  source_config_dir="$(dirname "$source_config_path")"
  if [[ -n "$source_secret_key_override" ]]; then
    source_secret_key="$source_secret_key_override"
  else
    source_secret_key="$source_config_dir/.secret_key"
  fi
  target_secret_key="$target_config_dir/.secret_key"

  if [[ -f "$source_secret_key" ]]; then
    info "Importing secret key from $source_secret_key"
    install -m 600 "$source_secret_key" "$target_secret_key"
    return 0
  fi

  if config_has_encrypted_secrets "$source_config_path"; then
    error "Encrypted secrets detected in $source_config_path, but key file was not found at:"
    error "  $source_secret_key"
    error "Provide the matching .secret_key next to config.toml (or via --docker-secret-key), or decrypt/remove encrypted values before bootstrap."
    exit 1
  fi
}

ensure_browser_webdriver_sidecar() {
  local sidecar_name="$1"
  local sidecar_image="$2"
  local network_name="$3"

  if "$CONTAINER_CLI" ps --format '{{.Names}}' | grep -Fxq "$sidecar_name"; then
    info "Browser WebDriver sidecar already running ($sidecar_name)"
    return 0
  fi

  if "$CONTAINER_CLI" ps -a --format '{{.Names}}' | grep -Fxq "$sidecar_name"; then
    info "Starting existing browser WebDriver sidecar ($sidecar_name)"
    "$CONTAINER_CLI" start "$sidecar_name" >/dev/null
    return 0
  fi

  info "Starting browser WebDriver sidecar ($sidecar_name)"
  "$CONTAINER_CLI" run -d \
    --name "$sidecar_name" \
    --network "$network_name" \
    --shm-size=2g \
    "$sidecar_image" >/dev/null
}

run_docker_bootstrap() {
  local docker_image docker_data_dir default_data_dir fallback_image
  local seed_config_path
  local seed_secret_key_path
  local config_mount workspace_mount
  local docker_build_features docker_browser_runtime_mode docker_browser_runtime_bool
  local docker_browser_sidecar_name docker_browser_sidecar_image docker_network
  local container_network_name docker_browser_webdriver_url
  local docker_daemon_name docker_daemon_bind_host docker_daemon_host_port docker_daemon_port
  local config_gateway_port_value
  local -a container_run_user_args container_run_namespace_args
  local -a container_extra_run_args container_extra_env_args docker_build_args daemon_cmd
  docker_image="${ZEROCLAW_DOCKER_IMAGE:-zeroclaw-bootstrap:local}"
  fallback_image="ghcr.io/zeroclaw-labs/zeroclaw:latest"
  docker_build_features="${ZEROCLAW_DOCKER_CARGO_FEATURES:-}"
  docker_browser_runtime_mode="${ZEROCLAW_DOCKER_BROWSER_RUNTIME:-auto}"
  docker_browser_sidecar_name="${ZEROCLAW_DOCKER_BROWSER_SIDECAR_NAME:-zeroclaw-browser-webdriver}"
  docker_browser_sidecar_image="${ZEROCLAW_DOCKER_BROWSER_SIDECAR_IMAGE:-selenium/standalone-chromium:latest}"
  docker_network="${ZEROCLAW_DOCKER_NETWORK:-zeroclaw-bootstrap-net}"
  docker_daemon_name="${ZEROCLAW_DOCKER_DAEMON_NAME:-zeroclaw-daemon}"
  docker_daemon_bind_host="${ZEROCLAW_DOCKER_DAEMON_BIND_HOST:-127.0.0.1}"
  docker_daemon_host_port="${ZEROCLAW_DOCKER_DAEMON_HOST_PORT:-}"
  seed_config_path="${DOCKER_CONFIG_FILE:-}"
  seed_secret_key_path="${DOCKER_SECRET_KEY_FILE:-${ZEROCLAW_DOCKER_SECRET_KEY_FILE:-}}"
  container_network_name=""
  docker_browser_webdriver_url=""
  if [[ "$TEMP_CLONE" == true ]]; then
    default_data_dir="$HOME/.zeroclaw-docker"
  else
    default_data_dir="$WORK_DIR/.zeroclaw-docker"
  fi
  docker_data_dir="${ZEROCLAW_DOCKER_DATA_DIR:-$default_data_dir}"
  DOCKER_DATA_DIR="$docker_data_dir"

  if [[ "$DOCKER_RESET" == true ]]; then
    reset_docker_zeroclaw_resources "$docker_data_dir"
  fi

  mkdir -p "$docker_data_dir/.zeroclaw" "$docker_data_dir/workspace"

  if [[ -n "$seed_config_path" ]]; then
    if [[ ! -f "$seed_config_path" ]]; then
      error "--docker-config file was not found: $seed_config_path"
      exit 1
    fi
    info "Seeding Docker config from $seed_config_path"
    install -m 600 "$seed_config_path" "$docker_data_dir/.zeroclaw/config.toml"
    seed_docker_secret_key_for_config "$seed_config_path" "$docker_data_dir/.zeroclaw" "$seed_secret_key_path"
  fi

  if [[ "$SKIP_INSTALL" == true ]]; then
    warn "--skip-install has no effect with --docker."
  fi

  maybe_stop_running_zeroclaw_containers

  docker_browser_runtime_bool="false"
  case "$(printf '%s' "$docker_browser_runtime_mode" | tr '[:upper:]' '[:lower:]')" in
    ""|auto)
      if [[ -n "$seed_config_path" ]]; then
        if config_requests_webdriver_sidecar "$seed_config_path"; then
          docker_browser_runtime_bool="true"
          info "Browser WebDriver sidecar enabled from seeded config ([browser] backend=rust_native/auto)."
        else
          docker_browser_runtime_bool="false"
          info "Browser WebDriver sidecar disabled from seeded config."
        fi
      elif guided_input_stream >/dev/null 2>&1; then
        echo
        if prompt_yes_no "Provision browser WebDriver sidecar for Docker bootstrap?" "yes"; then
          docker_browser_runtime_bool="true"
        fi
      fi
      ;;
    *)
      docker_browser_runtime_bool="$(string_to_bool "$docker_browser_runtime_mode")"
      if [[ "$docker_browser_runtime_bool" == "invalid" ]]; then
        warn "Invalid ZEROCLAW_DOCKER_BROWSER_RUNTIME='$docker_browser_runtime_mode' (expected auto/on/off). Defaulting to off."
        docker_browser_runtime_bool="false"
      fi
      ;;
  esac

  if [[ "$docker_browser_runtime_bool" == "true" ]]; then
    if [[ ",${docker_build_features// /,}," != *,browser-native,* ]]; then
      if [[ -n "$docker_build_features" ]]; then
        docker_build_features+=",browser-native"
      else
        docker_build_features="browser-native"
      fi
    fi
    ensure_docker_network "$docker_network"
    ensure_browser_webdriver_sidecar \
      "$docker_browser_sidecar_name" \
      "$docker_browser_sidecar_image" \
      "$docker_network"
    container_network_name="$docker_network"
    docker_browser_webdriver_url="http://${docker_browser_sidecar_name}:4444"
    info "Browser runtime sidecar: $docker_browser_sidecar_name ($docker_browser_webdriver_url)"
    if [[ "$SKIP_BUILD" == true ]]; then
      warn "--skip-build enabled: existing image must already include browser-native feature for rust_native backend."
    fi
  fi

  if [[ "$SKIP_BUILD" == false ]]; then
    info "Building Docker image ($docker_image)"
    docker_build_args=(build --target release -t "$docker_image")
    if [[ -n "$docker_build_features" ]]; then
      info "Docker build features: $docker_build_features"
      docker_build_args+=(--build-arg "ZEROCLAW_CARGO_FEATURES=$docker_build_features")
    fi
    docker_build_args+=("$WORK_DIR")
    "$CONTAINER_CLI" "${docker_build_args[@]}"
  else
    info "Skipping Docker image build"
    if ! "$CONTAINER_CLI" image inspect "$docker_image" >/dev/null 2>&1; then
      warn "Local Docker image ($docker_image) was not found."
      info "Pulling official ZeroClaw image ($fallback_image)"
      if ! "$CONTAINER_CLI" pull "$fallback_image"; then
        error "Failed to pull fallback Docker image: $fallback_image"
        error "Run without --skip-build to build locally, or verify access to GHCR."
        exit 1
      fi
      if [[ "$docker_image" != "$fallback_image" ]]; then
        info "Tagging fallback image as $docker_image"
        "$CONTAINER_CLI" tag "$fallback_image" "$docker_image"
      fi
    fi
  fi

  config_mount="$docker_data_dir/.zeroclaw:/zeroclaw-data/.zeroclaw"
  workspace_mount="$docker_data_dir/workspace:/zeroclaw-data/workspace"
  if [[ "$CONTAINER_CLI" == "podman" ]]; then
    config_mount+=":Z"
    workspace_mount+=":Z"
    container_run_namespace_args=(--userns keep-id)
    container_run_user_args=(--user "$(id -u):$(id -g)")
  else
    container_run_namespace_args=()
    container_run_user_args=(--user "$(id -u):$(id -g)")
  fi

  container_extra_run_args=()
  container_extra_env_args=()
  if [[ -n "$container_network_name" ]]; then
    container_extra_run_args+=(--network "$container_network_name")
  fi
  if [[ -n "$docker_browser_webdriver_url" ]]; then
    container_extra_env_args+=(-e "ZEROCLAW_DOCKER_WEBDRIVER_URL=$docker_browser_webdriver_url")
  fi

  info "Docker data directory: $docker_data_dir"
  info "Container CLI: $CONTAINER_CLI"

  if [[ "$DOCKER_DAEMON_MODE" == true ]]; then
    if "$CONTAINER_CLI" ps -a --format '{{.Names}}' | grep -Fxq "$docker_daemon_name"; then
      error "Daemon container '$docker_daemon_name' already exists."
      error "Use --docker-reset, or remove it manually: $CONTAINER_CLI rm -f $docker_daemon_name"
      exit 1
    fi

    config_gateway_port_value=""
    if [[ -f "$docker_data_dir/.zeroclaw/config.toml" ]]; then
      config_gateway_port_value="$(config_gateway_port "$docker_data_dir/.zeroclaw/config.toml" || true)"
    fi
    docker_daemon_port="${config_gateway_port_value:-42617}"
    if [[ -z "$docker_daemon_host_port" ]]; then
      docker_daemon_host_port="$docker_daemon_port"
    fi

    daemon_cmd=(run -d --name "$docker_daemon_name" --restart unless-stopped)
    if [[ "$CONTAINER_CLI" == "podman" ]]; then
      daemon_cmd+=("${container_run_namespace_args[@]}")
    fi
    daemon_cmd+=("${container_run_user_args[@]}")
    if [[ ${#container_extra_run_args[@]} -gt 0 ]]; then
      daemon_cmd+=("${container_extra_run_args[@]}")
    fi
    daemon_cmd+=(
      -p "${docker_daemon_bind_host}:${docker_daemon_host_port}:${docker_daemon_port}"
      -e HOME=/zeroclaw-data
      -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
      -e ZEROCLAW_DOCKER_BOOTSTRAP=1
    )
    if [[ ${#container_extra_env_args[@]} -gt 0 ]]; then
      daemon_cmd+=("${container_extra_env_args[@]}")
    fi
    daemon_cmd+=(
      -v "$config_mount"
      -v "$workspace_mount"
      "$docker_image"
      daemon
      --port "$docker_daemon_port"
    )

    info "Starting daemon container ($docker_daemon_name)"
    "$CONTAINER_CLI" "${daemon_cmd[@]}" >/dev/null
    info "Daemon running: $docker_daemon_name (gateway: http://${docker_daemon_bind_host}:${docker_daemon_host_port})"
    info "Follow logs: $CONTAINER_CLI logs -f $docker_daemon_name"
    return 0
  fi

  if [[ "$RUN_ONBOARD" == true ]]; then
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
  ZEROCLAW_API_KEY="sk-..." ./zeroclaw_install.sh --docker
or run interactive:
  ./zeroclaw_install.sh --docker --interactive-onboard
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

    if [[ "$CONTAINER_CLI" == "podman" ]]; then
      "$CONTAINER_CLI" run --rm -it \
        "${container_run_namespace_args[@]}" \
        "${container_run_user_args[@]}" \
        "${container_extra_run_args[@]+${container_extra_run_args[@]}}" \
        -e HOME=/zeroclaw-data \
        -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
        -e ZEROCLAW_DOCKER_BOOTSTRAP=1 \
        "${container_extra_env_args[@]+${container_extra_env_args[@]}}" \
        -v "$config_mount" \
        -v "$workspace_mount" \
        "$docker_image" \
        "${onboard_cmd[@]}"
    else
      "$CONTAINER_CLI" run --rm -it \
        "${container_run_user_args[@]}" \
        "${container_extra_run_args[@]+${container_extra_run_args[@]}}" \
        -e HOME=/zeroclaw-data \
        -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
        -e ZEROCLAW_DOCKER_BOOTSTRAP=1 \
        "${container_extra_env_args[@]+${container_extra_env_args[@]}}" \
        -v "$config_mount" \
        -v "$workspace_mount" \
        "$docker_image" \
        "${onboard_cmd[@]}"
    fi
  else
    info "Skipping onboarding container run (--onboard not requested)."
  fi
}

SCRIPT_PATH="${BASH_SOURCE[0]:-$0}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" >/dev/null 2>&1 && pwd || pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd || pwd)"
REPO_URL="https://github.com/zeroclaw-labs/zeroclaw.git"
ORIGINAL_ARG_COUNT=$#
GUIDED_MODE="auto"

DOCKER_MODE=false
DOCKER_RESET=false
DOCKER_DAEMON_MODE=false
DOCKER_CONFIG_FILE=""
DOCKER_SECRET_KEY_FILE=""
INSTALL_SYSTEM_DEPS=false
INSTALL_RUST=false
PREFER_PREBUILT=false
PREBUILT_ONLY=false
FORCE_SOURCE_BUILD=false
RUN_ONBOARD=false
INTERACTIVE_ONBOARD=false
SKIP_BUILD=false
SKIP_INSTALL=false
PREBUILT_INSTALLED=false
CONTAINER_CLI="${ZEROCLAW_CONTAINER_CLI:-docker}"
API_KEY="${ZEROCLAW_API_KEY:-}"
PROVIDER="${ZEROCLAW_PROVIDER:-openrouter}"
MODEL="${ZEROCLAW_MODEL:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --guided)
      GUIDED_MODE="on"
      shift
      ;;
    --no-guided)
      GUIDED_MODE="off"
      shift
      ;;
    --docker)
      DOCKER_MODE=true
      shift
      ;;
    --docker-reset)
      DOCKER_RESET=true
      shift
      ;;
    --docker-config)
      DOCKER_CONFIG_FILE="${2:-}"
      [[ -n "$DOCKER_CONFIG_FILE" ]] || {
        error "--docker-config requires a value"
        exit 1
      }
      shift 2
      ;;
    --docker-secret-key)
      DOCKER_SECRET_KEY_FILE="${2:-}"
      [[ -n "$DOCKER_SECRET_KEY_FILE" ]] || {
        error "--docker-secret-key requires a value"
        exit 1
      }
      shift 2
      ;;
    --docker-daemon)
      DOCKER_DAEMON_MODE=true
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
    --prefer-prebuilt)
      PREFER_PREBUILT=true
      shift
      ;;
    --prebuilt-only)
      PREBUILT_ONLY=true
      shift
      ;;
    --force-source-build)
      FORCE_SOURCE_BUILD=true
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
    --build-first)
      SKIP_BUILD=false
      shift
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

OS_NAME="$(uname -s)"
if [[ "$GUIDED_MODE" == "auto" ]]; then
  if [[ "$OS_NAME" == "Linux" && "$ORIGINAL_ARG_COUNT" -eq 0 && -t 0 && -t 1 ]]; then
    GUIDED_MODE="on"
  else
    GUIDED_MODE="off"
  fi
fi

if [[ "$DOCKER_MODE" == true && "$GUIDED_MODE" == "on" ]]; then
  warn "--guided is ignored with --docker."
  GUIDED_MODE="off"
fi

if [[ "$DOCKER_RESET" == true && "$DOCKER_MODE" == false ]]; then
  error "--docker-reset requires --docker."
  exit 1
fi

if [[ -n "$DOCKER_CONFIG_FILE" && "$DOCKER_MODE" == false ]]; then
  error "--docker-config requires --docker."
  exit 1
fi

if [[ -n "$DOCKER_SECRET_KEY_FILE" && "$DOCKER_MODE" == false ]]; then
  error "--docker-secret-key requires --docker."
  exit 1
fi

if [[ -n "$DOCKER_SECRET_KEY_FILE" && -z "$DOCKER_CONFIG_FILE" ]]; then
  error "--docker-secret-key requires --docker-config."
  exit 1
fi

if [[ "$DOCKER_DAEMON_MODE" == true && "$DOCKER_MODE" == false ]]; then
  error "--docker-daemon requires --docker."
  exit 1
fi

if [[ "$DOCKER_DAEMON_MODE" == true && "$RUN_ONBOARD" == true ]]; then
  error "--docker-daemon cannot be combined with --onboard/--interactive-onboard."
  exit 1
fi

if [[ "$GUIDED_MODE" == "on" ]]; then
  run_guided_installer "$OS_NAME"
fi

if [[ "$DOCKER_MODE" == true ]]; then
  if [[ "$INSTALL_SYSTEM_DEPS" == true ]]; then
    warn "--install-system-deps is ignored with --docker."
  fi
  if [[ "$INSTALL_RUST" == true ]]; then
      warn "--install-rust is ignored with --docker."
  fi
else
  if [[ "$OS_NAME" == "Linux" && -z "${ZEROCLAW_DISABLE_ALPINE_AUTO_DEPS:-}" ]] && have_cmd apk; then
    find_missing_alpine_prereqs
    if [[ ${#ALPINE_MISSING_PKGS[@]} -gt 0 && "$INSTALL_SYSTEM_DEPS" == false ]]; then
      info "Detected Alpine with missing prerequisites: ${ALPINE_MISSING_PKGS[*]}"
      info "Auto-enabling system dependency installation (set ZEROCLAW_DISABLE_ALPINE_AUTO_DEPS=1 to disable)."
      INSTALL_SYSTEM_DEPS=true
    fi
  fi

  if [[ "$INSTALL_SYSTEM_DEPS" == true ]]; then
    install_system_deps
  fi

  if [[ "$INSTALL_RUST" == true ]]; then
    install_rust_toolchain
  fi
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

if [[ "$FORCE_SOURCE_BUILD" == true ]]; then
  PREFER_PREBUILT=false
  PREBUILT_ONLY=false
fi

if [[ "$PREBUILT_ONLY" == true ]]; then
  PREFER_PREBUILT=true
fi

if [[ "$DOCKER_MODE" == true ]]; then
  ensure_docker_ready
  if [[ "$RUN_ONBOARD" == false ]]; then
    if [[ -n "$DOCKER_CONFIG_FILE" || "$DOCKER_DAEMON_MODE" == true ]]; then
      RUN_ONBOARD=false
    else
      RUN_ONBOARD=true
      if [[ -z "$API_KEY" ]]; then
        INTERACTIVE_ONBOARD=true
      fi
    fi
  fi
  run_docker_bootstrap
  echo
  echo "✅ Docker bootstrap complete."
  echo
  echo "Your containerized ZeroClaw data is persisted under:"
  echo "  $DOCKER_DATA_DIR"
  echo

  if [[ "$DOCKER_DAEMON_MODE" == true ]]; then
    daemon_name="${ZEROCLAW_DOCKER_DAEMON_NAME:-zeroclaw-daemon}"
    echo "Daemon mode is active; onboarding was intentionally skipped."
    echo "  container: $daemon_name"
    echo "  logs:      $CONTAINER_CLI logs -f $daemon_name"
    echo "  stop:      $CONTAINER_CLI rm -f $daemon_name"
    echo
    echo "Optional next steps:"
    echo "  ./zeroclaw_install.sh --docker --interactive-onboard"
  elif [[ "$RUN_ONBOARD" == false ]]; then
    echo "Onboarding was intentionally skipped (pre-seeded config mode)."
    echo
    echo "Next steps:"
    echo "  ./zeroclaw_install.sh --docker --docker-config ./config.toml --docker-daemon"
    echo "  ./zeroclaw_install.sh --docker --interactive-onboard"
  else
    cat <<'DONE'
Next steps:
  ./zeroclaw_install.sh --docker --interactive-onboard
  ./zeroclaw_install.sh --docker --api-key "sk-..." --provider openrouter
  ./zeroclaw_install.sh --docker --docker-config ./config.toml --docker-daemon
DONE
  fi
  exit 0
fi

if [[ "$FORCE_SOURCE_BUILD" == false ]]; then
  if [[ "$PREFER_PREBUILT" == false && "$PREBUILT_ONLY" == false ]]; then
    if should_attempt_prebuilt_for_resources "$WORK_DIR"; then
      info "Attempting pre-built binary first due to resource preflight."
      PREFER_PREBUILT=true
    fi
  fi

  if [[ "$PREFER_PREBUILT" == true ]]; then
    if install_prebuilt_binary; then
      PREBUILT_INSTALLED=true
      SKIP_BUILD=true
      SKIP_INSTALL=true
    elif [[ "$PREBUILT_ONLY" == true ]]; then
      error "Pre-built-only mode requested, but no compatible release asset is available."
      error "Try again later, or run with --force-source-build on a machine with enough RAM/disk."
      exit 1
    else
      warn "Pre-built install unavailable; falling back to source build."
    fi
  fi
fi

if [[ "$PREBUILT_INSTALLED" == false && ( "$SKIP_BUILD" == false || "$SKIP_INSTALL" == false ) ]] && ! have_cmd cargo; then
  error "cargo is not installed."
  cat <<'MSG' >&2
Install Rust first: https://rustup.rs/
or re-run with:
  ./zeroclaw_install.sh --install-rust
MSG
  exit 1
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
elif [[ -x "$HOME/.cargo/bin/zeroclaw" ]]; then
  ZEROCLAW_BIN="$HOME/.cargo/bin/zeroclaw"
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
  ZEROCLAW_API_KEY="sk-..." ./zeroclaw_install.sh --onboard
or run interactive:
  ./zeroclaw_install.sh --interactive-onboard
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
