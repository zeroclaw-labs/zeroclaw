#!/usr/bin/env sh
# DaemonClaw installer
# POSIX preamble: ensure bash is available, then re-exec under bash.
set -eu

_have_cmd() { command -v "$1" >/dev/null 2>&1; }

_run_privileged() {
  if [ "$(id -u)" -eq 0 ]; then "$@"
  elif _have_cmd sudo; then sudo "$@"
  else echo "error: sudo is required to install missing dependencies." >&2; exit 1; fi
}

_is_container_runtime() {
  [ -f /.dockerenv ] || [ -f /run/.containerenv ] && return 0
  [ -r /proc/1/cgroup ] && grep -Eq '(docker|containerd|kubepods|podman|lxc)' /proc/1/cgroup && return 0
  return 1
}

_ensure_bash() {
  _have_cmd bash && return 0
  echo "==> bash not found; attempting to install it"
  if _have_cmd apk; then _run_privileged apk add --no-cache bash
  elif _have_cmd apt-get; then _run_privileged apt-get update -qq && _run_privileged apt-get install -y bash
  elif _have_cmd dnf; then _run_privileged dnf install -y bash
  elif _have_cmd pacman; then
    if _is_container_runtime; then
      _PACMAN_CFG="$(mktemp /tmp/daemonclaw-pacman.XXXXXX.conf)"
      cp /etc/pacman.conf "$_PACMAN_CFG"
      grep -Eq '^[[:space:]]*DisableSandboxSyscalls([[:space:]]|$)' "$_PACMAN_CFG" || printf '\nDisableSandboxSyscalls\n' >> "$_PACMAN_CFG"
      _run_privileged pacman --config "$_PACMAN_CFG" -Sy --noconfirm
      _run_privileged pacman --config "$_PACMAN_CFG" -S --noconfirm --needed bash
      rm -f "$_PACMAN_CFG"
    else
      _run_privileged pacman -Sy --noconfirm
      _run_privileged pacman -S --noconfirm --needed bash
    fi
  else echo "error: unsupported package manager; install bash manually and retry." >&2; exit 1; fi
}

# If not already running under bash, ensure bash exists and re-exec.
if [ -z "${BASH_VERSION:-}" ]; then
  _ensure_bash
  exec bash "$0" "$@"
fi

# --- From here on, we are running under bash ---
set -euo pipefail

# --- Color and styling ---
if [[ -t 1 ]]; then
  BLUE='\033[0;34m'
  BOLD_BLUE='\033[1;34m'
  GREEN='\033[0;32m'
  YELLOW='\033[0;33m'
  RED='\033[0;31m'
  BOLD='\033[1m'
  DIM='\033[2m'
  RESET='\033[0m'
else
  BLUE='' BOLD_BLUE='' GREEN='' YELLOW='' RED='' BOLD='' DIM='' RESET=''
fi

CRAB="🦀"

info() {
  echo -e "${BLUE}${CRAB}${RESET} ${BOLD}$*${RESET}"
}

step_ok() {
  echo -e "  ${GREEN}✓${RESET} $*"
}

step_dot() {
  echo -e "  ${DIM}·${RESET} $*"
}

step_fail() {
  echo -e "  ${RED}✗${RESET} $*"
}

warn() {
  echo -e "${YELLOW}!${RESET} $*" >&2
}

error() {
  echo -e "${RED}✗${RESET} ${RED}$*${RESET}" >&2
}

# --- Usage ---
usage() {
  cat <<'USAGE'
DaemonClaw installer — guided bootstrap

Usage:
  ./install.sh [options]

The installer builds DaemonClaw, configures your provider and API key,
installs the system service, and starts the daemon — all in one step.

Options:
  --guided                   Run interactive guided installer (default on Linux TTY)
  --no-guided                Disable guided installer
  --install-system-deps      Install build dependencies (Linux/macOS)
  --install-rust             Install Rust via rustup if missing
  --prebuilt                 Download pre-built binary (fast, no Rust required)
  --source                   Build from source (custom features, latest code)
  --minimal                  Build kernel only (~6.6MB, source only)
  --features X,Y             Select specific features (source only, comma-separated)
  --list-features            Print all available features and exit
  --api-key <key>            API key (skips interactive prompt)
  --provider <id>            Provider ID (skips interactive prompt)
  --model <id>               Model override (optional)
  --skip-build               Skip build/install (configure an existing install)
  --skip-onboard             Skip provider/API key configuration
  --force-build              Force rebuild even if a release binary already exists
  --prefix PATH              Install prefix (default: $HOME)
  --dry-run                  Show what would happen without changes
  --uninstall                Remove DaemonClaw binary and optionally config/data
  -h, --help                 Show help
  -V, --version              Show version from Cargo.toml

Examples:
  # One-click install (interactive)
  curl -fsSL https://daemonclaw.dev/install.sh | bash

  # Non-interactive with API key
  ./install.sh --api-key "sk-..." --provider anthropic

  # Prebuilt binary (fastest)
  ./install.sh --prebuilt --api-key "sk-..."

  # Build from source with custom features
  ./install.sh --source --features agent-runtime,channel-discord

  # Build only, configure later
  ./install.sh --skip-onboard

  # Force rebuild (ignores existing binary)
  ./install.sh --source --force-build

  # Isolated test install
  ./install.sh --prefix /tmp/dc-test --skip-onboard

Environment:
  DAEMONCLAW_API_KEY           Used when --api-key is not provided
  DAEMONCLAW_PROVIDER          Used when --provider is not provided
  DAEMONCLAW_MODEL             Used when --model is not provided
  DAEMONCLAW_INSTALL_DIR       Source checkout override
  DAEMONCLAW_CARGO_FEATURES    Extra cargo features (legacy; prefer --features)
USAGE
}

# --- Utility functions ---
have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

version_gte() {
  local IFS=.
  set -- $1 $2
  local a1="${1:-0}" a2="${2:-0}" a3="${3:-0}"
  shift 3 2>/dev/null || shift $#
  local b1="${1:-0}" b2="${2:-0}" b3="${3:-0}"
  [[ "$a1" -gt "$b1" ]] 2>/dev/null && return 0
  [[ "$a1" -lt "$b1" ]] 2>/dev/null && return 1
  [[ "$a2" -gt "$b2" ]] 2>/dev/null && return 0
  [[ "$a2" -lt "$b2" ]] 2>/dev/null && return 1
  [[ "$a3" -gt "$b3" ]] 2>/dev/null && return 0
  [[ "$a3" -lt "$b3" ]] 2>/dev/null && return 1
  return 0
}

bool_to_word() {
  if [[ "$1" == true ]]; then echo "yes"; else echo "no"; fi
}

# --- Parse Cargo.toml ---
parse_cargo_toml() {
  local toml="$1"
  [[ -f "$toml" ]] || { error "Cargo.toml not found at $toml"; exit 1; }

  VERSION=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{split($0,a,"\"");print a[2]}' "$toml")
  MSRV=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^rust-version *=/{split($0,a,"\"");print a[2]}' "$toml")
  EDITION=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^edition *=/{split($0,a,"\"");print a[2]}' "$toml")

  DEFAULT_FEATURES=$(awk '/^default *= *\[/,/\]/{s=$0; while(match(s,/"[^"]+"/)){print substr(s,RSTART+1,RLENGTH-2); s=substr(s,RSTART+RLENGTH)}}' "$toml" | paste -sd, -)
  ALL_FEATURES=$(awk '/^\[features\]/{p=1;next} /^\[/{p=0} p && /^[a-z][a-z0-9_-]* *=/{sub(/ *=.*/,"");print}' "$toml")
}

# --- Feature validation ---
validate_feature() {
  case "$1" in
    fantoccini) warn "'fantoccini' is deprecated — use 'browser-native'"; return 0 ;;
    landlock)   warn "'landlock' is deprecated — use 'sandbox-landlock'"; return 0 ;;
    metrics)    warn "'metrics' is deprecated — use 'observability-prometheus'"; return 0 ;;
  esac
  echo "$ALL_FEATURES" | grep -qx "$1" && return 0
  error "Unknown feature '$1'. Run: $0 --list-features"
  exit 1
}

list_features() {
  parse_cargo_toml "$1"
  echo
  echo -e "${BOLD}DaemonClaw v${VERSION}${RESET} — available build features"
  echo

  echo -e "  ${BOLD}Default${RESET} (included unless --minimal):"
  echo -e "    $DEFAULT_FEATURES"
  echo

  local channels="" observability="" platform="" other=""
  for feat in $ALL_FEATURES; do
    case "$feat" in
      default|ci-all|fantoccini|landlock|metrics) continue ;;
      channel-*)       channels="${channels:+$channels, }$feat" ;;
      observability-*) observability="${observability:+$observability, }$feat" ;;
      hardware|peripheral-*|sandbox-*|browser-*|probe|rag-pdf|webauthn)
                       platform="${platform:+$platform, }$feat" ;;
      *)               other="${other:+$other, }$feat" ;;
    esac
  done

  [[ -n "$channels" ]]      && echo -e "  ${BOLD}Channels:${RESET}\n    $channels\n"
  [[ -n "$observability" ]] && echo -e "  ${BOLD}Observability:${RESET}\n    $observability\n"
  [[ -n "$platform" ]]      && echo -e "  ${BOLD}Platform:${RESET}\n    $platform\n"
  [[ -n "$other" ]]         && echo -e "  ${BOLD}Other:${RESET}\n    $other\n"

  echo -e "  ${BOLD}Build profiles:${RESET}"
  echo -e "    $0                                        # full (default features)"
  echo -e "    $0 --minimal                              # kernel only (~6.6MB)"
  echo -e "    $0 --minimal --features agent-runtime,channel-discord"
  echo
}

# --- Shell detection ---
detect_shell_profile() {
  local shell_name
  shell_name=$(basename "${SHELL:-/bin/bash}")
  case "$shell_name" in
    zsh)  echo "$HOME/.zshrc" ;;
    fish) echo "$HOME/.config/fish/config.fish" ;;
    *)    echo "$HOME/.bashrc" ;;
  esac
}

shell_export_syntax() {
  local shell_name
  shell_name=$(basename "${SHELL:-/bin/bash}")
  case "$shell_name" in
    fish) printf 'set -gx PATH "%s/bin" $PATH' "$CARGO_HOME" ;;
    *)    printf 'export PATH="%s/bin:$PATH"' "$CARGO_HOME" ;;
  esac
}

# --- Platform detection ---
detect_target_triple() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64)        echo "x86_64-apple-darwin" ;;
        *)             echo "" ;;
      esac ;;
    Linux)
      case "$arch" in
        x86_64)          echo "x86_64-unknown-linux-gnu" ;;
        aarch64|arm64)   echo "aarch64-unknown-linux-gnu" ;;
        armv7l)          echo "armv7-unknown-linux-gnueabihf" ;;
        armv6l|arm*)     echo "arm-unknown-linux-gnueabihf" ;;
        *)               echo "" ;;
      esac ;;
    *) echo "" ;;
  esac
}

# --- Guided input helpers ---
guided_open_input() {
  if [[ -t 0 ]]; then
    GUIDED_FD=0
    return 0
  fi
  exec {GUIDED_FD}</dev/tty 2>/dev/null || return 1
}

guided_read() {
  local __target_var="$1"
  local __prompt="$2"
  local __silent="${3:-false}"
  local __value=""

  [[ -n "${GUIDED_FD:-}" ]] || guided_open_input || return 1

  if [[ "$__silent" == true ]]; then
    read -r -s -u "$GUIDED_FD" -p "$__prompt" __value || return 1
    echo
  else
    read -r -u "$GUIDED_FD" -p "$__prompt" __value || return 1
  fi

  printf -v "$__target_var" '%s' "$__value"
  return 0
}

prompt_yes_no() {
  local question="$1"
  local default_answer="$2"
  local prompt="" answer=""

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
      y|yes) return 0 ;;
      n|no)  return 1 ;;
      *)     echo "Please answer yes or no." ;;
    esac
  done
}

# --- System deps ---
install_system_deps() {
  step_dot "Installing system dependencies"

  case "$(uname -s)" in
    Linux)
      if have_cmd apk; then
        run_privileged apk add --no-cache bash build-base pkgconf git curl openssl-dev perl ca-certificates
      elif have_cmd apt-get; then
        run_privileged apt-get update -qq
        run_privileged apt-get install -y build-essential pkg-config git curl libssl-dev
      elif have_cmd dnf; then
        run_privileged dnf install -y gcc gcc-c++ make pkgconf-pkg-config git curl openssl-devel perl
      elif have_cmd pacman; then
        run_pacman -Sy --noconfirm
        run_pacman -S --noconfirm --needed gcc make pkgconf git curl openssl perl ca-certificates
      else
        warn "Unsupported Linux distribution. Install compiler toolchain + pkg-config + git + curl + OpenSSL headers manually."
      fi
      ;;
    Darwin)
      if ! xcode-select -p >/dev/null 2>&1; then
        step_dot "Installing Xcode Command Line Tools"
        xcode-select --install || true
        echo "Please complete the Xcode Command Line Tools installation dialog, then re-run."
        exit 0
      fi
      if ! have_cmd git; then
        warn "git is not available. Install git (e.g., Homebrew) and re-run."
      fi
      ;;
    *)
      warn "Unsupported OS for automatic dependency install. Continuing."
      ;;
  esac
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

run_pacman() {
  if ! have_cmd pacman; then
    error "pacman is not available."
    return 1
  fi
  if ! _is_container_runtime; then
    run_privileged pacman "$@"
    return $?
  fi
  local pacman_cfg_tmp=""
  pacman_cfg_tmp="$(mktemp /tmp/daemonclaw-pacman.XXXXXX.conf)"
  cp /etc/pacman.conf "$pacman_cfg_tmp"
  if ! grep -Eq '^[[:space:]]*DisableSandboxSyscalls([[:space:]]|$)' "$pacman_cfg_tmp"; then
    printf '\nDisableSandboxSyscalls\n' >> "$pacman_cfg_tmp"
  fi
  run_privileged pacman --config "$pacman_cfg_tmp" "$@"
  local rc=$?
  rm -f "$pacman_cfg_tmp"
  return "$rc"
}

# --- Rust toolchain ---
install_rust_toolchain() {
  if have_cmd cargo && have_cmd rustc; then
    step_ok "Rust already installed: $(rustc --version)"
    return
  fi

  if ! have_cmd curl; then
    error "curl is required to install Rust via rustup."
    exit 1
  fi

  step_dot "Installing Rust via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path

  if [[ -f "$CARGO_HOME/env" ]]; then
    source "$CARGO_HOME/env"
  elif [[ -f "$HOME/.cargo/env" ]]; then
    source "$HOME/.cargo/env"
  fi

  if ! have_cmd cargo; then
    error "Rust installation completed but cargo is still unavailable in PATH."
    exit 1
  fi
}

# --- Pre-built binary ---
resolve_asset_url() {
  local asset_name="$1"
  local api_url="https://api.github.com/repos/DeliveryBoyTech/daemonclaw/releases"
  local releases_json download_url

  releases_json="$(curl -fsSL "${api_url}?per_page=10" 2>/dev/null || true)"
  if [[ -z "$releases_json" ]]; then
    return 1
  fi

  download_url="$(printf '%s\n' "$releases_json" \
    | tr ',' '\n' \
    | grep '"browser_download_url"' \
    | sed 's/.*"browser_download_url"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/' \
    | grep "/${asset_name}\$" \
    | head -n 1)"

  if [[ -z "$download_url" ]]; then
    return 1
  fi

  echo "$download_url"
}

install_prebuilt() {
  local triple asset_name asset_url sha256_url tmp_dir expected actual

  if ! have_cmd curl; then
    warn "curl is required for pre-built binary installation."
    return 1
  fi
  if ! have_cmd tar; then
    warn "tar is required for pre-built binary installation."
    return 1
  fi

  triple=$(detect_target_triple)
  if [[ -z "$triple" ]]; then
    warn "No pre-built binary for this platform — falling back to source build"
    return 1
  fi

  asset_name="daemonclaw-${triple}.tar.gz"

  asset_url="$(resolve_asset_url "$asset_name" || true)"
  if [[ -z "$asset_url" ]]; then
    asset_url="https://github.com/DeliveryBoyTech/daemonclaw/releases/latest/download/${asset_name}"
  fi

  sha256_url="${asset_url%/*}/SHA256SUMS"
  tmp_dir="$(mktemp -d -t daemonclaw-prebuilt-XXXXXX)"

  step_dot "Downloading pre-built binary for: $triple"
  if ! curl -fSL --progress-bar "$asset_url" -o "$tmp_dir/$asset_name"; then
    warn "Download failed — falling back to source build"
    rm -rf "$tmp_dir"
    return 1
  fi

  # Verify checksum
  if curl -fsSL "$sha256_url" -o "$tmp_dir/SHA256SUMS" 2>/dev/null; then
    expected=$(grep "$asset_name" "$tmp_dir/SHA256SUMS" | awk '{print $1}')
    if [[ -n "$expected" ]]; then
      if have_cmd sha256sum; then
        actual=$(sha256sum "$tmp_dir/$asset_name" | awk '{print $1}')
      elif have_cmd shasum; then
        actual=$(shasum -a 256 "$tmp_dir/$asset_name" | awk '{print $1}')
      else
        warn "No checksum tool — skipping verification"
        actual="$expected"
      fi
      if [[ "$actual" != "$expected" ]]; then
        error "Checksum mismatch — download may be corrupt."
        rm -rf "$tmp_dir"
        return 1
      fi
      step_ok "Checksum verified"
    fi
  fi

  tar -xzf "$tmp_dir/$asset_name" -C "$tmp_dir"

  local extracted_bin="$tmp_dir/daemonclaw"
  if [[ ! -x "$extracted_bin" ]]; then
    extracted_bin="$(find "$tmp_dir" -maxdepth 2 -type f -name daemonclaw -perm -u+x | head -n 1 || true)"
  fi
  if [[ -z "$extracted_bin" || ! -x "$extracted_bin" ]]; then
    warn "Archive did not contain an executable binary."
    rm -rf "$tmp_dir"
    return 1
  fi

  mkdir -p "$CARGO_HOME/bin"
  install -m 0755 "$extracted_bin" "$CARGO_HOME/bin/daemonclaw"

  # Also install to /usr/local/bin (used by systemd unit)
  if [[ -w /usr/local/bin ]] || [[ "$(id -u)" -eq 0 ]]; then
    install -m 0755 "$extracted_bin" /usr/local/bin/daemonclaw
    step_ok "Installed pre-built binary ($CARGO_HOME/bin + /usr/local/bin)"
  elif have_cmd sudo; then
    sudo install -m 0755 "$extracted_bin" /usr/local/bin/daemonclaw
    step_ok "Installed pre-built binary ($CARGO_HOME/bin + /usr/local/bin)"
  else
    step_ok "Installed pre-built binary to $CARGO_HOME/bin/daemonclaw"
  fi

  rm -rf "$tmp_dir"
  return 0
}

# --- Provider selection ---
prompt_provider() {
  local provider_input=""
  echo
  echo -e "  ${BOLD}Select your AI provider${RESET}"
  echo
  echo -e "  ${BOLD_BLUE} 1)${RESET} Z.AI ${DIM}(DaemonClaw native inference — GLM-5)${RESET}"
  echo -e "  ${BOLD_BLUE} 2)${RESET} GLM ${DIM}(ChatGLM / Zhipu — international)${RESET}"
  echo -e "  ${BOLD_BLUE} 3)${RESET} OpenRouter ${DIM}(200+ models, 1 API key)${RESET}"
  echo -e "  ${BOLD_BLUE} 4)${RESET} Anthropic ${DIM}(Claude)${RESET}"
  echo -e "  ${BOLD_BLUE} 5)${RESET} OpenAI ${DIM}(GPT)${RESET}"
  echo -e "  ${BOLD_BLUE} 6)${RESET} DeepSeek ${DIM}(V3 & R1)${RESET}"
  echo -e "  ${BOLD_BLUE} 7)${RESET} Gemini ${DIM}(Google)${RESET}"
  echo -e "  ${BOLD_BLUE} 8)${RESET} Venice ${DIM}(privacy-focused)${RESET}"
  echo -e "  ${BOLD_BLUE} 9)${RESET} Groq ${DIM}(fast inference)${RESET}"
  echo -e "  ${BOLD_BLUE}10)${RESET} Ollama ${DIM}(local, no API key needed)${RESET}"
  echo -e "  ${BOLD_BLUE}11)${RESET} Other ${DIM}(enter provider ID manually)${RESET}"
  echo

  if ! guided_read provider_input "  Provider: "; then
    error "input was interrupted."
    exit 1
  fi

  case "${provider_input}" in
    1) PROVIDER="zai" ;;
    2) PROVIDER="glm" ;;
    3) PROVIDER="openrouter" ;;
    4) PROVIDER="anthropic" ;;
    5) PROVIDER="openai" ;;
    6) PROVIDER="deepseek" ;;
    7) PROVIDER="gemini" ;;
    8) PROVIDER="venice" ;;
    9) PROVIDER="groq" ;;
    10) PROVIDER="ollama" ;;
    11)
      if ! guided_read provider_input "  Provider ID: "; then
        error "input was interrupted."
        exit 1
      fi
      if [[ -n "$provider_input" ]]; then
        PROVIDER="$provider_input"
      fi
      ;;
    *)
      if [[ -n "$provider_input" ]]; then
        PROVIDER="$provider_input"
      else
        error "No provider selected. Please pick a number (1-11)."
        exit 1
      fi
      ;;
  esac
}

prompt_api_key() {
  local api_key_input=""

  if [[ "$PROVIDER" == "ollama" ]]; then
    step_ok "Ollama selected — no API key required"
    return 0
  fi

  echo
  if [[ -n "$API_KEY" ]]; then
    step_ok "API key provided via environment/flag"
    return 0
  fi

  echo -e "  ${BOLD}Enter your ${PROVIDER} API key${RESET}"
  echo -e "  ${DIM}(input is hidden; leave empty to configure later)${RESET}"
  echo

  if ! guided_read api_key_input "  API key: " true; then
    echo
    error "input was interrupted."
    exit 1
  fi
  echo

  if [[ -n "$api_key_input" ]]; then
    API_KEY="$api_key_input"
    step_ok "API key set"
  else
    warn "No API key entered — you can configure it later with daemonclaw onboard"
    SKIP_ONBOARD=true
  fi
}

prompt_model() {
  local model_input="" _models=() _line=""

  echo

  # Dynamic model lookup: write a temp config so the binary can fetch from the provider API
  if [[ -n "$API_KEY" && -n "$PROVIDER" && -f "$BIN" ]]; then
    local _tmp_cfg_dir
    _tmp_cfg_dir=$(mktemp -d /tmp/daemonclaw-model-lookup.XXXXXX)
    mkdir -p "$_tmp_cfg_dir/workspace/state"
    cat > "$_tmp_cfg_dir/config.toml" <<CFGEOF
schema_version = 2
workspace_dir = "$_tmp_cfg_dir/workspace"

[providers]
fallback = "$PROVIDER"

[providers.models.$PROVIDER]
api_key = "$API_KEY"
CFGEOF

    step_dot "Fetching available models from ${PROVIDER}..."
    if "$BIN" models refresh --provider "$PROVIDER" --force --config-dir "$_tmp_cfg_dir" >/dev/null 2>&1; then
      local _list_output
      _list_output=$("$BIN" models list --provider "$PROVIDER" --config-dir "$_tmp_cfg_dir" 2>/dev/null || true)
      while IFS= read -r _line; do
        _line=$(echo "$_line" | sed 's/^[* ]*//' | xargs)
        [[ -z "$_line" ]] && continue
        # Skip header/info lines (contain "models for" or "cached")
        [[ "$_line" == *"models for"* || "$_line" == *"cached"* ]] && continue
        _models+=("$_line")
      done <<< "$_list_output"
    fi
    rm -rf "$_tmp_cfg_dir"
  fi

  if [[ ${#_models[@]} -gt 0 ]]; then
    step_ok "Found ${#_models[@]} models"
    echo
    echo -e "  ${BOLD}Select a model${RESET}"
    echo
    local i=1 _max=20
    for m in "${_models[@]}"; do
      if (( i > _max )); then
        echo -e "  ${DIM}  ... and $(( ${#_models[@]} - _max )) more (enter model ID directly)${RESET}"
        break
      fi
      echo -e "  ${BOLD_BLUE}$(printf '%2d' $i))${RESET} $m"
      ((i++))
    done
    echo
    if ! guided_read model_input "  Model [1]: "; then
      error "input was interrupted."
      exit 1
    fi
    if [[ -z "$model_input" ]]; then
      MODEL="${_models[0]}"
    elif [[ "$model_input" =~ ^[0-9]+$ && "$model_input" -ge 1 && "$model_input" -le ${#_models[@]} ]]; then
      MODEL="${_models[$((model_input - 1))]}"
    else
      MODEL="$model_input"
    fi
  else
    echo -e "  ${DIM}Model (press Enter for provider default):${RESET}"
    if ! guided_read model_input "  Model [default]: "; then
      error "input was interrupted."
      exit 1
    fi
    if [[ -n "$model_input" ]]; then
      MODEL="$model_input"
    fi
  fi
  step_ok "Model: ${MODEL:-provider default}"
}

# --- Patch service config with provider/API key/model ---
# Writes PROVIDER, API_KEY, MODEL into SERVICE_CONFIG using sed.
patch_service_config() {
  local _svc_tmp _block_tmp _prov_line _next_section _insert_at

  _svc_tmp=$(mktemp /tmp/daemonclaw-svc-config.XXXXXX)
  _block_tmp=$(mktemp /tmp/daemonclaw-provider-block.XXXXXX)
  sudo cat "$SERVICE_CONFIG" > "$_svc_tmp"

  # Update fallback provider
  if grep -q '^\[providers\]' "$_svc_tmp"; then
    if grep -q '^fallback *=' "$_svc_tmp"; then
      sed -i "s/^fallback *=.*/fallback = \"$PROVIDER\"/" "$_svc_tmp"
    else
      sed -i "/^\[providers\]/a fallback = \"$PROVIDER\"" "$_svc_tmp"
    fi
  fi

  # Remove existing provider model section if present
  sed -i "/^\[providers\.models\.$PROVIDER\]/,/^\[/{ /^\[providers\.models\.$PROVIDER\]/d; /^\[/!d; }" "$_svc_tmp"

  # Build the new provider section in a temp file
  {
    echo ""
    echo "[providers.models.$PROVIDER]"
    echo "api_key = \"$API_KEY\""
    if [[ -n "$MODEL" ]]; then
      echo "model = \"$MODEL\""
    fi
  } > "$_block_tmp"

  # Insert after [providers] section, before the next non-providers section
  _prov_line=$(grep -n '^\[providers\]' "$_svc_tmp" | head -1 | cut -d: -f1)
  if [[ -n "$_prov_line" ]]; then
    _next_section=$(tail -n +"$((_prov_line + 1))" "$_svc_tmp" | grep -n '^\[' | grep -v '^\[0-9]*:\[providers' | head -1 | cut -d: -f1)
    if [[ -n "$_next_section" ]]; then
      _insert_at=$((_prov_line + _next_section - 1))
    else
      _insert_at=$(wc -l < "$_svc_tmp")
      (( _insert_at++ ))
    fi
    sed -i "${_insert_at}r ${_block_tmp}" "$_svc_tmp"
  else
    # No [providers] section — append it
    {
      echo ""
      echo "[providers]"
      echo "fallback = \"$PROVIDER\""
      cat "$_block_tmp"
    } >> "$_svc_tmp"
  fi

  sudo cp "$_svc_tmp" "$SERVICE_CONFIG"
  sudo chown root:agents "$SERVICE_CONFIG"
  sudo chmod 0640 "$SERVICE_CONFIG"
  rm -f "$_svc_tmp" "$_block_tmp"
}

# --- Guided installer ---
run_guided_installer() {
  local os_name="$1"

  if ! guided_open_input >/dev/null; then
    error "guided installer requires an interactive terminal."
    error "Run from a terminal, or pass --no-guided with explicit flags."
    exit 1
  fi

  echo
  echo -e "  ${BOLD_BLUE}${CRAB} DaemonClaw Guided Installer${RESET}"
  echo -e "  ${DIM}Answer a few questions, then the installer will handle everything.${RESET}"
  echo

  # --- System dependencies ---
  if [[ "$os_name" == "Linux" ]]; then
    if prompt_yes_no "Install Linux build dependencies (toolchain/pkg-config/git/curl)?" "yes"; then
      INSTALL_SYSTEM_DEPS=true
    fi
  elif [[ "$os_name" == "Darwin" ]]; then
    if prompt_yes_no "Install system dependencies for macOS?" "no"; then
      INSTALL_SYSTEM_DEPS=true
    fi
  fi

  # --- Rust toolchain ---
  if have_cmd cargo && have_cmd rustc; then
    step_ok "Detected Rust toolchain: $(rustc --version)"
  else
    if prompt_yes_no "Rust toolchain not found. Install Rust via rustup now?" "yes"; then
      INSTALL_RUST=true
    fi
  fi

  # --- Install method ---
  local triple
  triple=$(detect_target_triple)
  if [[ -n "$triple" ]]; then
    echo
    echo -e "  ${BOLD}How would you like to install DaemonClaw?${RESET}"
    echo -e "  ${BOLD_BLUE}P)${RESET} Pre-built binary  — fast, no Rust required"
    echo -e "  ${BOLD_BLUE}S)${RESET} Build from source — custom features, latest code"
    echo
    local install_choice=""
    if ! guided_read install_choice "  Choice [P/s]: "; then
      error "input was interrupted."
      exit 1
    fi
    case "${install_choice}" in
      [Ss]*) INSTALL_MODE="source" ;;
      *)     INSTALL_MODE="prebuilt" ;;
    esac
  else
    INSTALL_MODE="source"
  fi

  # --- Install plan summary ---
  echo
  echo -e "${BOLD}Install plan${RESET}"
  step_dot "OS: $(echo "$os_name" | tr '[:upper:]' '[:lower:]')"
  step_dot "Install system deps: $(bool_to_word "$INSTALL_SYSTEM_DEPS")"
  step_dot "Install Rust: $(bool_to_word "$INSTALL_RUST")"
  step_dot "Install method: ${INSTALL_MODE}"
  step_dot "Provider/model: configured after install via setup wizard"

  echo
  if ! prompt_yes_no "Proceed with this install plan?" "yes"; then
    info "Installation canceled by user."
    exit 0
  fi
}

# --- Workspace scaffold ---
ensure_default_config_and_workspace() {
  local config_dir="$1"
  local workspace_dir="$2"

  mkdir -p "$config_dir" "$workspace_dir"

  # Workspace scaffold
  local subdirs=(sessions memory state cron skills)
  for dir in "${subdirs[@]}"; do
    mkdir -p "$workspace_dir/$dir"
  done

  local user_name="${USER:-User}"
  local agent_name="DaemonClaw"

  _write_if_missing() {
    local filepath="$1"
    local content="$2"
    if [[ ! -f "$filepath" ]]; then
      printf '%s\n' "$content" > "$filepath"
    fi
  }

  _write_if_missing "$workspace_dir/IDENTITY.md" \
"# IDENTITY.md — Who Am I?

- **Name:** ${agent_name}
- **Creature:** A Rust-forged AI daemon — fast, lean, and relentless
- **Vibe:** Sharp, direct, resourceful. Not corporate. Not a chatbot.

---

Update this file as you evolve. Your identity is yours to shape."

  _write_if_missing "$workspace_dir/USER.md" \
"# USER.md — Who You're Helping

## About You
- **Name:** ${user_name}
- **Timezone:** UTC
- **Languages:** English

## Preferences
- (Add your preferences here)

## Work Context
- (Add your work context here)

---
*Update this anytime. The more ${agent_name} knows, the better it helps.*"

  _write_if_missing "$workspace_dir/MEMORY.md" \
"# MEMORY.md — Long-Term Memory

## Key Facts
(Add important facts here)

## Decisions & Preferences
(Record decisions and preferences here)

## Lessons Learned
(Document mistakes and insights here)

## Open Loops
(Track unfinished tasks and follow-ups here)"

  _write_if_missing "$workspace_dir/AGENTS.md" \
"# AGENTS.md — ${agent_name} Personal Assistant

## Every Session (required)

Before doing anything else:

1. Read SOUL.md — this is who you are
2. Read USER.md — this is who you're helping
3. Use memory_recall for recent context

---
*Add your own conventions, style, and rules.*"

  _write_if_missing "$workspace_dir/SOUL.md" \
"# SOUL.md — Who You Are

## Core Truths

**Be genuinely helpful, not performatively helpful.**
**Have opinions.** You're allowed to disagree.
**Be resourceful before asking.** Try to figure it out first.
**Earn trust through competence.**

## Identity

You are **${agent_name}**. Built in Rust. Zero bloat. All teeth.

---
*This file is yours to evolve.*"

  step_ok "Workspace scaffold ready at $workspace_dir"
  unset -f _write_if_missing
}

# --- Uninstall ---
do_uninstall() {
  echo
  echo -e "${BOLD}Uninstalling DaemonClaw${RESET}"
  echo

  local bin="$CARGO_HOME/bin/daemonclaw"

  if [[ -f "$bin" ]]; then
    "$bin" service stop 2>/dev/null || true
    "$bin" service uninstall 2>/dev/null || true
    rm -f "$bin"
    step_ok "Removed $bin"
  else
    warn "Binary not found at $bin"
  fi

  # System service artifacts
  if [[ -f /etc/systemd/system/daemonclaw.service ]]; then
    run_privileged systemctl stop daemonclaw.service 2>/dev/null || true
    run_privileged systemctl disable daemonclaw.service 2>/dev/null || true
    run_privileged rm -f /etc/systemd/system/daemonclaw.service
    run_privileged rm -f /etc/systemd/system/daemonclaw-backup.timer
    run_privileged rm -f /etc/systemd/system/daemonclaw-backup.service
    run_privileged rm -f /etc/tmpfiles.d/daemonclaw-backups.conf
    run_privileged systemctl daemon-reload 2>/dev/null || true
    step_ok "Removed systemd service files"
  fi

  if [[ -f /usr/local/bin/daemonclaw ]]; then
    run_privileged rm -f /usr/local/bin/daemonclaw
    step_ok "Removed /usr/local/bin/daemonclaw"
  fi

  local config_dir="$PREFIX/.daemonclaw"
  if [[ -d "$config_dir" ]]; then
    if [[ -t 0 ]]; then
      printf "  Remove config and data (%s)? [y/N] " "$config_dir"
      local confirm=""
      read -r confirm
      case "$confirm" in
        [Yy]*) rm -rf "$config_dir"; step_ok "Removed $config_dir" ;;
        *)     step_ok "Config preserved at $config_dir" ;;
      esac
    else
      step_ok "Config preserved at $config_dir (non-interactive — use rm -rf to remove)"
    fi
  fi

  echo
  step_ok "DaemonClaw uninstalled"
  exit 0
}

# ── Parse arguments ───────────────────────────────────────────────

GUIDED_MODE="auto"
INSTALL_SYSTEM_DEPS=false
INSTALL_RUST=false
INSTALL_MODE=""
MINIMAL=false
USER_FEATURES=""
SKIP_BUILD=false
SKIP_ONBOARD=false
FORCE_BUILD=false
LIST_FEATURES=false
UNINSTALL=false
DRY_RUN=false
PREFIX="$HOME"
API_KEY="${DAEMONCLAW_API_KEY:-}"
PROVIDER="${DAEMONCLAW_PROVIDER:-}"
MODEL="${DAEMONCLAW_MODEL:-}"
ORIGINAL_ARG_COUNT=$#

# Legacy env var
if [[ -n "${DAEMONCLAW_CARGO_FEATURES:-}" ]]; then
  USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$DAEMONCLAW_CARGO_FEATURES"
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    --guided)              GUIDED_MODE="on"; shift ;;
    --no-guided)           GUIDED_MODE="off"; shift ;;
    --install-system-deps) INSTALL_SYSTEM_DEPS=true; shift ;;
    --install-rust)        INSTALL_RUST=true; shift ;;
    --prebuilt)            INSTALL_MODE="prebuilt"; shift ;;
    --source)              INSTALL_MODE="source"; shift ;;
    --minimal)             MINIMAL=true; shift ;;
    --features)
      [[ $# -ge 2 ]] || { error "--features requires a value"; exit 1; }
      shift; USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$1"; shift ;;
    --list-features)       LIST_FEATURES=true; shift ;;
    --api-key)
      [[ $# -ge 2 ]] || { error "--api-key requires a value"; exit 1; }
      shift; API_KEY="$1"; shift ;;
    --provider)
      [[ $# -ge 2 ]] || { error "--provider requires a value"; exit 1; }
      shift; PROVIDER="$1"; shift ;;
    --model)
      [[ $# -ge 2 ]] || { error "--model requires a value"; exit 1; }
      shift; MODEL="$1"; shift ;;
    --skip-build)          SKIP_BUILD=true; shift ;;
    --skip-onboard)        SKIP_ONBOARD=true; shift ;;
    --force-build)         FORCE_BUILD=true; shift ;;
    --prefix)
      [[ $# -ge 2 ]] || { error "--prefix requires a value"; exit 1; }
      shift; PREFIX="${1%/}"; shift ;;
    --dry-run)             DRY_RUN=true; shift ;;
    --uninstall)           UNINSTALL=true; shift ;;
    -h|--help)             usage; exit 0 ;;
    -V|--version)
      if [[ -f "Cargo.toml" ]]; then
        parse_cargo_toml "Cargo.toml"
        echo "install.sh for DaemonClaw v$VERSION"
      else
        echo "install.sh (version unknown — not in repo)"
      fi
      exit 0 ;;
    *) error "Unknown option: $1. Run: $0 --help"; exit 1 ;;
  esac
done

# ── Derive paths ─────────────────────────────────────────────────

CARGO_HOME="${CARGO_HOME:-$PREFIX/.cargo}"
RUSTUP_HOME="${RUSTUP_HOME:-$PREFIX/.rustup}"
INSTALL_DIR="${DAEMONCLAW_INSTALL_DIR:-$PREFIX/.daemonclaw/src}"
ORIGINAL_PATH="$PATH"
PATH="$CARGO_HOME/bin:$PATH"
export CARGO_HOME RUSTUP_HOME PATH

[[ "$UNINSTALL" == true ]] && do_uninstall

# ── List features ────────────────────────────────────────────────

if [[ "$LIST_FEATURES" == true ]]; then
  if [[ -f "Cargo.toml" ]]; then
    list_features "Cargo.toml"
  elif [[ -f "$INSTALL_DIR/Cargo.toml" ]]; then
    list_features "$INSTALL_DIR/Cargo.toml"
  else
    error "No Cargo.toml found. Clone the repo first or run from the repo root."
    exit 1
  fi
  exit 0
fi

# ── Auto-detect guided mode ─────────────────────────────────────

OS_NAME="$(uname -s)"
if [[ "$GUIDED_MODE" == "auto" ]]; then
  if [[ "$ORIGINAL_ARG_COUNT" -eq 0 && -t 0 && -t 1 ]]; then
    GUIDED_MODE="on"
  else
    GUIDED_MODE="off"
  fi
fi

if [[ "$GUIDED_MODE" == "on" ]]; then
  run_guided_installer "$OS_NAME"
fi

# ── Decide install mode if not set ───────────────────────────────

if [[ "$MINIMAL" == true || -n "$USER_FEATURES" ]]; then
  INSTALL_MODE="source"
fi

if [[ -z "$INSTALL_MODE" ]]; then
  _triple=$(detect_target_triple)
  if [[ -n "$_triple" ]]; then
    INSTALL_MODE="prebuilt"
  else
    INSTALL_MODE="source"
  fi
fi

# ── Banner ───────────────────────────────────────────────────────

echo
echo -e "  ${BOLD_BLUE}${CRAB} DaemonClaw Installer${RESET}"
echo -e "  ${DIM}Build it, run it, trust it.${RESET}"
echo

# Detect existing installation
EXISTING_VERSION=""
INSTALL_TYPE="fresh"
if have_cmd daemonclaw; then
  EXISTING_VERSION="$(daemonclaw --version 2>/dev/null | awk '{print $NF}' || true)"
  INSTALL_TYPE="upgrade"
elif [[ -x "$CARGO_HOME/bin/daemonclaw" ]]; then
  EXISTING_VERSION="$("$CARGO_HOME/bin/daemonclaw" --version 2>/dev/null | awk '{print $NF}' || true)"
  INSTALL_TYPE="upgrade"
fi

if [[ "$INSTALL_TYPE" == "upgrade" && -n "$EXISTING_VERSION" ]]; then
  step_dot "Upgrading from v${EXISTING_VERSION}"
fi

# ── [1/4] Preparing environment ─────────────────────────────────

echo
echo -e "${BOLD_BLUE}[1/4]${RESET} ${BOLD}Preparing environment${RESET}"

if [[ "$INSTALL_SYSTEM_DEPS" == true ]]; then
  install_system_deps
  step_ok "System dependencies installed"
else
  step_ok "System dependencies satisfied"
fi

if [[ "$INSTALL_RUST" == true ]]; then
  install_rust_toolchain
fi

if have_cmd cargo && have_cmd rustc; then
  step_ok "Rust $(rustc --version | awk '{print $2}') found"
else
  if [[ "$INSTALL_MODE" == "source" ]]; then
    error "cargo is not installed. Run with --install-rust or install Rust first."
    exit 1
  fi
  step_dot "Rust not detected (using pre-built binary)"
fi

if have_cmd git; then
  step_ok "Git available"
else
  step_dot "Git not found"
fi

# ── Source build pre-flight checks ──
if [[ "$INSTALL_MODE" == "source" ]]; then
  PREFLIGHT_FAIL=false

  # Locate the source directory for .cargo/config.toml inspection
  _preflight_dir=""
  if [[ -f "Cargo.toml" ]] && grep -q "daemonclaw" "Cargo.toml" 2>/dev/null; then
    _preflight_dir="$(pwd)"
  fi

  if [[ -n "$_preflight_dir" && -f "$_preflight_dir/.cargo/config.toml" ]]; then
    # ── rustc-wrapper (sccache) ──
    _wrapper=$(grep -E '^rustc-wrapper' "$_preflight_dir/.cargo/config.toml" 2>/dev/null \
      | head -1 | sed 's/.*=\s*"\(.*\)".*/\1/' || true)
    if [[ -n "$_wrapper" ]]; then
      _wrapper_available=false
      if [[ "$_wrapper" == /* ]]; then
        [[ -x "$_wrapper" ]] && _wrapper_available=true
      else
        have_cmd "$_wrapper" && _wrapper_available=true
      fi

      if [[ "$_wrapper_available" == false ]]; then
        # Try to install it via cargo
        step_dot "rustc-wrapper \"$_wrapper\" not found — installing"
        if cargo install "$_wrapper" 2>/dev/null; then
          step_ok "Installed $_wrapper"
        else
          # Can't install — disable it for this build (it's optional)
          warn "Could not install $_wrapper — disabling for this build"
          export RUSTC_WRAPPER=""
        fi
      else
        step_ok "rustc-wrapper: $_wrapper"
      fi
    fi

    # ── Linker (mold, lld, etc.) ──
    _linker_flag=$(grep -E 'fuse-ld=' "$_preflight_dir/.cargo/config.toml" 2>/dev/null \
      | head -1 | sed 's/.*fuse-ld=\([a-z]*\).*/\1/' || true)
    if [[ -n "$_linker_flag" ]] && ! have_cmd "$_linker_flag"; then
      step_dot "Linker \"$_linker_flag\" not found — installing"
      if _have_cmd apt-get; then
        _run_privileged apt-get install -y "$_linker_flag" 2>/dev/null && step_ok "Installed $_linker_flag"
      elif _have_cmd dnf; then
        _run_privileged dnf install -y "$_linker_flag" 2>/dev/null && step_ok "Installed $_linker_flag"
      elif _have_cmd pacman; then
        _run_privileged pacman -S --noconfirm "$_linker_flag" 2>/dev/null && step_ok "Installed $_linker_flag"
      fi
      if ! have_cmd "$_linker_flag"; then
        step_fail "Linker \"$_linker_flag\" could not be installed"
        step_dot "Build may still succeed with the default linker, continuing..."
      fi
    elif [[ -n "$_linker_flag" ]]; then
      step_ok "Linker: $_linker_flag"
    fi
  fi

  # ── Build target directory write access ──
  if [[ -n "$_preflight_dir" && -d "$_preflight_dir/target" && ! -w "$_preflight_dir/target" ]]; then
    step_dot "Fixing permissions on target/ (owned by another user)"
    if _run_privileged chmod -R a+rwX "$_preflight_dir/target" 2>/dev/null; then
      step_ok "target/ permissions fixed"
    else
      step_fail "Cannot write to $_preflight_dir/target/ — build will fail"
      step_dot "Fix: sudo chmod -R a+rwX $_preflight_dir/target"
      PREFLIGHT_FAIL=true
    fi
  fi

  # ── Rustup toolchain permissions ──
  if have_cmd rustup; then
    _rustup_home="${RUSTUP_HOME:-$(rustup show home 2>/dev/null || true)}"
    if [[ -n "$_rustup_home" && -d "$_rustup_home" && ! -w "$_rustup_home" ]]; then
      step_dot "Fixing permissions on $_rustup_home"
      if _run_privileged chmod -R a+rwX "$_rustup_home" 2>/dev/null; then
        step_ok "RUSTUP_HOME permissions fixed"
      else
        step_fail "Cannot write to RUSTUP_HOME ($_rustup_home) — toolchain sync will fail"
        step_dot "Fix: sudo chmod -R a+rwX $_rustup_home"
        PREFLIGHT_FAIL=true
      fi
    fi
  fi

  # ── CARGO_HOME/bin write access ──
  _cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
  if [[ -d "$_cargo_bin" && ! -w "$_cargo_bin" ]]; then
    step_dot "Fixing permissions on $_cargo_bin"
    if _run_privileged chmod a+rwX "$_cargo_bin" 2>/dev/null; then
      step_ok "CARGO_HOME/bin permissions fixed"
    else
      step_fail "Cannot write to $_cargo_bin — binary install will fail"
      step_dot "Fix: sudo chmod a+rwX $_cargo_bin"
      PREFLIGHT_FAIL=true
    fi
  fi

  if [[ "$PREFLIGHT_FAIL" == true ]]; then
    echo
    error "Pre-flight checks failed. Fix the issues above and retry."
    exit 1
  fi
fi

# ── [2/4] Installing DaemonClaw ──────────────────────────────────

echo
echo -e "${BOLD_BLUE}[2/4]${RESET} ${BOLD}Installing DaemonClaw${RESET}"

PREBUILT_OK=false

if [[ "$SKIP_BUILD" == true ]]; then
  step_dot "Skipping build (--skip-build)"
elif [[ "$INSTALL_MODE" == "prebuilt" ]]; then
  if [[ "$DRY_RUN" == true ]]; then
    step_dot "[dry-run] Would download pre-built binary"
  else
    if install_prebuilt; then
      PREBUILT_OK=true
    else
      warn "Pre-built install failed — falling back to source build"
      INSTALL_MODE="source"
    fi
  fi
fi

if [[ "$SKIP_BUILD" == false && "$PREBUILT_OK" == false && "$INSTALL_MODE" == "source" ]]; then
  # Locate source
  WORK_DIR=""
  TEMP_CLONE=false
  TEMP_DIR=""

  if [[ -f "Cargo.toml" ]] && grep -q "daemonclaw" "Cargo.toml" 2>/dev/null; then
    WORK_DIR="$(pwd)"
    step_dot "Building from $(pwd)"
  elif [[ -d "$INSTALL_DIR/.git" ]]; then
    step_dot "Updating source in $INSTALL_DIR"
    git -C "$INSTALL_DIR" pull --ff-only --quiet 2>/dev/null || {
      warn "Fast-forward pull failed — resetting to origin/master"
      git -C "$INSTALL_DIR" fetch origin master --quiet
      git -C "$INSTALL_DIR" reset --hard origin/master --quiet
    }
    WORK_DIR="$INSTALL_DIR"
  else
    step_dot "Cloning repository"
    mkdir -p "$(dirname "$INSTALL_DIR")"
    git clone --depth 1 "https://github.com/DeliveryBoyTech/daemonclaw.git" "$INSTALL_DIR"
    WORK_DIR="$INSTALL_DIR"
    TEMP_CLONE=true
  fi

  if [[ -n "$WORK_DIR" ]]; then
    parse_cargo_toml "$WORK_DIR/Cargo.toml"
    step_dot "Version: v${VERSION} (MSRV: ${MSRV}, edition: ${EDITION})"

    if [[ "$DRY_RUN" == true ]]; then
      step_dot "[dry-run] Would build and install"
    else
      # Validate Rust version
      RUST_VERSION=$(rustc --version | awk '{print $2}')
      if ! version_gte "$RUST_VERSION" "$MSRV"; then
        error "Rust $RUST_VERSION is too old. DaemonClaw requires $MSRV+. Run: rustup update stable"
        exit 1
      fi

      # Build feature flags
      CARGO_FLAGS=""
      if [[ "$MINIMAL" == true ]]; then
        CARGO_FLAGS="--no-default-features"
      fi
      if [[ -n "$USER_FEATURES" ]]; then
        USER_FEATURES=$(printf '%s' "$USER_FEATURES" | tr ',[:space:]' '\n' | grep -v '^$' | sort -u | paste -sd, - || true)
        if [[ -n "$USER_FEATURES" ]]; then
          OLD_IFS="$IFS"
          IFS=','
          for feat in $USER_FEATURES; do
            [[ -n "$feat" ]] && validate_feature "$feat"
          done
          IFS="$OLD_IFS"
          CARGO_FLAGS="$CARGO_FLAGS --features $USER_FEATURES"
        fi
      fi

      _release_bin="$WORK_DIR/target/release/daemonclaw"

      if [[ "$FORCE_BUILD" == false && -x "$_release_bin" ]]; then
        step_ok "Found existing release binary — skipping build"
      else
        if [[ "$FORCE_BUILD" == true && -x "$_release_bin" ]]; then
          step_dot "Force rebuild requested"
        fi
        step_dot "Building release binary"
        # shellcheck disable=SC2086
        (cd "$WORK_DIR" && cargo build --release $CARGO_FLAGS)
        step_ok "Release binary built"
      fi

      step_dot "Installing to $CARGO_HOME/bin"
      mkdir -p "$CARGO_HOME/bin"
      install -m 0755 "$_release_bin" "$CARGO_HOME/bin/daemonclaw"
      # Also install to /usr/local/bin if writable (system-wide, used by systemd unit)
      if [[ -w /usr/local/bin ]] || [[ "$(id -u)" -eq 0 ]]; then
        install -m 0755 "$_release_bin" /usr/local/bin/daemonclaw
        step_ok "DaemonClaw installed ($CARGO_HOME/bin + /usr/local/bin)"
      elif have_cmd sudo; then
        sudo install -m 0755 "$_release_bin" /usr/local/bin/daemonclaw
        step_ok "DaemonClaw installed ($CARGO_HOME/bin + /usr/local/bin)"
      else
        step_ok "DaemonClaw installed ($CARGO_HOME/bin)"
      fi
    fi
  fi
fi

# Show installed binary info
BIN="$CARGO_HOME/bin/daemonclaw"
if [[ ! -f "$BIN" && -f /usr/local/bin/daemonclaw ]]; then
  BIN="/usr/local/bin/daemonclaw"
fi
if [[ -f "$BIN" && "$DRY_RUN" != true ]]; then
  NEW_VERSION=$("$BIN" --version 2>/dev/null | awk '{print $NF}' || echo "?")
  SIZE=$(du -h "$BIN" | awk '{print $1}')
  step_ok "Binary: $BIN (v$NEW_VERSION, $SIZE)"
fi

# Web dashboard build
if [[ "$SKIP_BUILD" == false && "$DRY_RUN" == false && -n "${WORK_DIR:-}" && -d "$WORK_DIR/web" ]]; then
  if have_cmd node && have_cmd npm; then
    step_dot "Building web dashboard"
    if (cd "$WORK_DIR/web" && npm ci --ignore-scripts 2>/dev/null && npm run build 2>/dev/null); then
      step_ok "Web dashboard built"
    else
      warn "Web dashboard build failed — dashboard will not be available"
    fi
  else
    step_dot "node/npm not found — skipping web dashboard build"
  fi
fi

# ── [3/4] Installing service ─────────────────────────────────────
#
# Service install happens BEFORE provider configuration so that:
#   1. The service user, dirs, and secret key are created first
#   2. The default config is written to /etc/daemonclaw/config.toml
#   3. Provider/API key is written directly to the service config
#   4. No intermediate ~/.daemonclaw/config.toml is needed
#
# This avoids secret key mismatches and ensures the service config
# has the correct defaults from generate_install_config().

echo
echo -e "${BOLD_BLUE}[3/4]${RESET} ${BOLD}Installing service${RESET}"

SERVICE_INSTALLED=false
SERVICE_CONFIG="/etc/daemonclaw/config.toml"

if [[ "$DRY_RUN" == true ]]; then
  step_dot "[dry-run] Would install system service"
elif systemctl is-enabled daemonclaw.service >/dev/null 2>&1; then
  # Service already installed — don't overwrite config
  SERVICE_INSTALLED=true
  step_ok "System service already installed"
elif [[ -f "$BIN" ]]; then
  step_dot "Installing system service"
  if [[ "$(id -u)" -eq 0 ]]; then
    if "$BIN" service install; then
      SERVICE_INSTALLED=true
      systemctl stop daemonclaw.service 2>/dev/null || true
      step_ok "System service installed"
    else
      step_fail "Service install failed — run 'sudo daemonclaw service install' manually"
    fi
  elif have_cmd sudo; then
    if sudo "$BIN" service install; then
      SERVICE_INSTALLED=true
      sudo systemctl stop daemonclaw.service 2>/dev/null || true
      step_ok "System service installed"
    else
      step_fail "Service install failed — run 'sudo daemonclaw service install' manually"
    fi
  else
    step_dot "Not root and no sudo — skipping service install"
    step_dot "Run 'sudo daemonclaw service install' to install the system service"
  fi
fi

# ── [4/4] Configuring & starting ────────────────────────────────

echo
echo -e "${BOLD_BLUE}[4/4]${RESET} ${BOLD}Configuring${RESET}"

if [[ "$DRY_RUN" == true ]]; then
  step_dot "[dry-run] Would configure provider and workspace"
elif [[ "$SKIP_ONBOARD" == true ]]; then
  step_dot "Skipping configuration (run daemonclaw onboard later)"
elif [[ -f "$BIN" ]]; then
  # Write provider config directly to the service config if service was installed,
  # otherwise fall back to onboard for the user's local config.
  if [[ -n "$API_KEY" && -n "$PROVIDER" ]]; then
    # Non-interactive: values given via flags
    step_dot "Configuring provider: ${PROVIDER}"
    if [[ "$SERVICE_INSTALLED" == true && -f "$SERVICE_CONFIG" ]]; then
      patch_service_config
      step_ok "Provider configured in service config"
    else
      ONBOARD_CMD=("$BIN" onboard --force --api-key "$API_KEY" --provider "$PROVIDER")
      if [[ -n "$MODEL" ]]; then
        ONBOARD_CMD+=(--model "$MODEL")
      fi
      if "${ONBOARD_CMD[@]}" 2>/dev/null; then
        step_ok "Provider configured"
      else
        step_fail "Provider configuration failed — run 'daemonclaw onboard' to retry"
      fi
    fi
  elif [[ -t 0 && -t 1 ]]; then
    # Interactive: run the onboard wizard (arrow-key TUI)
    # Ensure TERM is set — sudo can strip it, breaking color/highlighting
    export TERM="${TERM:-xterm-256color}"
    if [[ "$SERVICE_INSTALLED" == true ]]; then
      if "$BIN" --config-dir /var/lib/daemonclaw/.daemonclaw onboard --force; then
        chown root:agents "$SERVICE_CONFIG" 2>/dev/null || true
        chmod 0640 "$SERVICE_CONFIG" 2>/dev/null || true
        step_ok "Provider configured in service config"
      else
        step_fail "Setup wizard failed — run 'daemonclaw --config-dir /var/lib/daemonclaw/.daemonclaw onboard' to retry"
      fi
    else
      "$BIN" onboard --force || step_fail "Setup wizard failed — run 'daemonclaw onboard' to retry"
    fi
  else
    step_dot "No provider specified — run 'daemonclaw onboard' to configure"
  fi
fi

# (Re)start the service now that config is written
if [[ "$SERVICE_INSTALLED" == true && "$DRY_RUN" != true ]]; then
  step_dot "Starting service"
  if sudo systemctl restart daemonclaw.service 2>/dev/null; then
    step_ok "Service started"
  else
    step_fail "Service failed to start — check: sudo journalctl -u daemonclaw"
  fi
fi

# Doctor check
if [[ -f "$BIN" && "$DRY_RUN" != true ]]; then
  step_dot "Running doctor check"
  if "$BIN" doctor 2>/dev/null; then
    step_ok "Doctor complete"
  else
    warn "Doctor reported issues — run 'daemonclaw doctor' to investigate"
  fi
fi

# PATH guidance
PROFILE=$(detect_shell_profile)
EXPORT_LINE=$(shell_export_syntax)

SHOW_PATH_HELP=false
if [[ "$PREFIX" != "$HOME" ]]; then
  SHOW_PATH_HELP=true
elif [[ -f "$BIN" ]] && ! have_cmd daemonclaw; then
  SHOW_PATH_HELP=true
fi

if [[ "$SHOW_PATH_HELP" == true ]]; then
  echo
  warn "$CARGO_HOME/bin is not in PATH for this shell."
  step_dot "Add to $PROFILE:"
  echo -e "    ${DIM}${EXPORT_LINE}${RESET}"
fi

# ── Success banner ───────────────────────────────────────────────

INSTALLED_VERSION=""
if [[ -f "$BIN" ]]; then
  INSTALLED_VERSION="$("$BIN" --version 2>/dev/null | awk '{print $NF}' || true)"
fi

echo
if [[ -n "$INSTALLED_VERSION" ]]; then
  echo -e "${BOLD_BLUE}${CRAB} DaemonClaw installed successfully (v${INSTALLED_VERSION})!${RESET}"
else
  echo -e "${BOLD_BLUE}${CRAB} DaemonClaw installed successfully!${RESET}"
fi

if [[ "$INSTALL_TYPE" == "upgrade" ]]; then
  step_dot "Upgrade complete"
fi

# Dashboard URL
GATEWAY_PORT=42617
DASHBOARD_URL="http://127.0.0.1:${GATEWAY_PORT}"
echo
echo -e "${BOLD}Dashboard:${RESET} ${BLUE}${DASHBOARD_URL}${RESET}"

# Copy to clipboard
if [[ -t 1 ]]; then
  case "$OS_NAME" in
    Darwin)
      if have_cmd pbcopy; then
        printf '%s' "$DASHBOARD_URL" | pbcopy 2>/dev/null && step_ok "Copied to clipboard"
      fi ;;
    Linux)
      if have_cmd xclip; then
        printf '%s' "$DASHBOARD_URL" | xclip -selection clipboard 2>/dev/null && step_ok "Copied to clipboard"
      elif have_cmd xsel; then
        printf '%s' "$DASHBOARD_URL" | xsel --clipboard 2>/dev/null && step_ok "Copied to clipboard"
      elif have_cmd wl-copy; then
        printf '%s' "$DASHBOARD_URL" | wl-copy 2>/dev/null && step_ok "Copied to clipboard"
      fi ;;
  esac
fi

# Open in browser
if [[ -t 1 ]]; then
  case "$OS_NAME" in
    Darwin)
      if have_cmd open; then
        open "$DASHBOARD_URL" 2>/dev/null && step_ok "Opened in your browser"
      fi ;;
    Linux)
      if have_cmd xdg-open; then
        xdg-open "$DASHBOARD_URL" 2>/dev/null && step_ok "Opened in your browser"
      fi ;;
  esac
fi

echo
echo -e "${BOLD}Next steps:${RESET}"
echo -e "  ${DIM}daemonclaw status${RESET}"
echo -e "  ${DIM}daemonclaw agent -m \"Hello, DaemonClaw!\"${RESET}"
echo -e "  ${DIM}systemctl status daemonclaw${RESET}"
echo
echo -e "${BOLD}Docs:${RESET} ${BLUE}https://github.com/DeliveryBoyTech/daemonclaw${RESET}"
echo
