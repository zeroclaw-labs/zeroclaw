#!/usr/bin/env bash
set -euo pipefail

# ── ZeroClaw installer ───────────────────────────────────────────
# Builds and installs ZeroClaw from source.
# All feature lists and version info read from Cargo.toml — nothing hardcoded.

REPO_URL="https://github.com/zeroclaw-labs/zeroclaw.git"
INSTALL_DIR="${ZEROCLAW_INSTALL_DIR:-$HOME/.zeroclaw/src}"

# ── Output helpers ────────────────────────────────────────────────

bold() { printf '\033[1m%s\033[0m' "$*"; }
green() { printf '\033[32m%s\033[0m' "$*"; }
yellow() { printf '\033[33m%s\033[0m' "$*"; }
red() { printf '\033[31m%s\033[0m' "$*"; }

info()  { echo "  $(green "✓") $*"; }
warn()  { echo "  $(yellow "⚠") $*" >&2; }
die()   { echo "  $(red "✗") $*" >&2; exit 1; }

# ── Parse Cargo.toml (source of truth) ────────────────────────────

parse_cargo_toml() {
  local toml="$1"
  [[ -f "$toml" ]] || die "Cargo.toml not found at $toml"

  VERSION=$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\([^"]*\)"/\1/p}' "$toml")
  MSRV=$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^rust-version *= *"\([^"]*\)"/\1/p}' "$toml")
  EDITION=$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^edition *= *"\([^"]*\)"/\1/p}' "$toml")

  # Default features (may span multiple lines)
  DEFAULT_FEATURES=$(sed -n '/^default *= *\[/,/\]/{s/.*"\([^"]*\)".*/\1/p}' "$toml" | paste -sd, -)

  # All feature names from [features] section
  ALL_FEATURES=$(sed -n '/^\[features\]/,/^\[/{/^[a-z][a-z0-9_-]* *=/s/ *=.*//p}' "$toml")
}

# ── Feature validation ────────────────────────────────────────────

validate_feature() {
  # Check deprecated aliases first (they exist in Cargo.toml but should warn)
  case "$1" in
    fantoccini) warn "'fantoccini' is deprecated — use 'browser-native'" ; return 0 ;;
    landlock)   warn "'landlock' is deprecated — use 'sandbox-landlock'" ; return 0 ;;
    metrics)    warn "'metrics' is deprecated — use 'observability-prometheus'" ; return 0 ;;
  esac
  echo "$ALL_FEATURES" | grep -qx "$1" && return 0
  die "Unknown feature '$1'. Run: $0 --list-features"
}

# ── List features ─────────────────────────────────────────────────

list_features() {
  parse_cargo_toml "$1"
  echo
  echo "$(bold "ZeroClaw v${VERSION}") — available build features"
  echo

  echo "  $(bold "Default") (included unless --minimal):"
  echo "    $DEFAULT_FEATURES"
  echo

  local channels="" observability="" platform="" other=""
  while IFS= read -r feat; do
    case "$feat" in
      default|ci-all) continue ;;
      fantoccini|landlock|metrics) continue ;; # deprecated aliases — hidden
      channel-*)       channels="${channels:+$channels, }$feat" ;;
      observability-*) observability="${observability:+$observability, }$feat" ;;
      hardware|peripheral-*|sandbox-*|browser-*|probe|rag-pdf|webauthn)
                       platform="${platform:+$platform, }$feat" ;;
      *)               other="${other:+$other, }$feat" ;;
    esac
  done <<< "$ALL_FEATURES"

  [[ -n "$channels" ]]      && echo "  $(bold "Channels:")" && echo "    $channels" && echo
  [[ -n "$observability" ]] && echo "  $(bold "Observability:")" && echo "    $observability" && echo
  [[ -n "$platform" ]]      && echo "  $(bold "Platform:")" && echo "    $platform" && echo
  [[ -n "$other" ]]         && echo "  $(bold "Other:")" && echo "    $other" && echo

  echo "  $(bold "Build profiles:")"
  echo "    $0                                        # full (default features)"
  echo "    $0 --minimal                              # kernel only (~6.6MB)"
  echo "    $0 --minimal --features agent-runtime,channel-discord"
  echo
}

# ── Version comparison ────────────────────────────────────────────

version_gte() {
  # Returns 0 if $1 >= $2 (dot-separated version strings)
  printf '%s\n%s' "$2" "$1" | sort -V -C
}

# ── Usage ─────────────────────────────────────────────────────────

usage() {
  cat <<EOF
$(bold "ZeroClaw installer") — build and install from source

Usage: $0 [options]

Options:
  --minimal            Build kernel only (config + providers + memory, ~6.6MB)
  --features X,Y       Select specific features (comma-separated)
  --list-features      Print all available features and exit
  --skip-onboard       Skip the setup wizard after install
  --uninstall          Remove ZeroClaw binary and optionally config/data
  -h, --help           Show this help

Examples:
  $0                                          # full install (interactive)
  $0 --minimal                                # smallest possible binary
  $0 --features agent-runtime,channel-discord  # custom feature set
  $0 --skip-onboard                           # build only, configure later
  $0 --uninstall                              # remove ZeroClaw

Environment:
  ZEROCLAW_INSTALL_DIR   Source checkout location (default: ~/.zeroclaw/src)
EOF
}

# ── Uninstall ─────────────────────────────────────────────────────

do_uninstall() {
  echo
  echo "$(bold "Uninstalling ZeroClaw")"
  echo

  local bin="${CARGO_HOME:-$HOME/.cargo}/bin/zeroclaw"
  if [[ -f "$bin" ]]; then
    rm -f "$bin"
    info "Removed $bin"
  else
    warn "Binary not found at $bin"
  fi

  # Try to stop/remove service if zeroclaw is still callable
  if command -v zeroclaw >/dev/null 2>&1; then
    zeroclaw service stop 2>/dev/null || true
    zeroclaw service uninstall 2>/dev/null || true
  fi

  local config_dir="$HOME/.zeroclaw"
  if [[ -d "$config_dir" ]]; then
    echo
    read -rp "  Remove config and data ($config_dir)? [y/N] " confirm
    if [[ "$confirm" =~ ^[Yy] ]]; then
      rm -rf "$config_dir"
      info "Removed $config_dir"
    else
      info "Config preserved at $config_dir"
    fi
  fi

  echo
  info "ZeroClaw uninstalled"
  exit 0
}

# ── Parse arguments ───────────────────────────────────────────────

MINIMAL=false
USER_FEATURES=""
SKIP_ONBOARD=false
LIST_FEATURES=false
UNINSTALL=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --minimal)        MINIMAL=true ;;
    --features)       shift; USER_FEATURES="$1" ;;
    --list-features)  LIST_FEATURES=true ;;
    --skip-onboard)   SKIP_ONBOARD=true ;;
    --uninstall)      UNINSTALL=true ;;
    -h|--help)        usage; exit 0 ;;
    *) die "Unknown option: $1. Run: $0 --help" ;;
  esac
  shift
done

[[ "$UNINSTALL" == true ]] && do_uninstall

# ── List features (can run without cloning if in repo) ────────────

if [[ "$LIST_FEATURES" == true ]]; then
  if [[ -f "Cargo.toml" ]]; then
    list_features "Cargo.toml"
  elif [[ -f "$INSTALL_DIR/Cargo.toml" ]]; then
    list_features "$INSTALL_DIR/Cargo.toml"
  else
    die "No Cargo.toml found. Clone the repo first or run from the repo root."
  fi
  exit 0
fi

# ── Clone or update source ────────────────────────────────────────

echo
echo "$(bold "ZeroClaw — source install")"
echo

if [[ -d "$INSTALL_DIR/.git" ]]; then
  info "Updating source in $INSTALL_DIR"
  git -C "$INSTALL_DIR" fetch origin master --quiet
  git -C "$INSTALL_DIR" reset --hard origin/master --quiet
else
  info "Cloning into $INSTALL_DIR"
  git clone --depth 1 "$REPO_URL" "$INSTALL_DIR"
fi

cd "$INSTALL_DIR"

# ── Parse Cargo.toml ──────────────────────────────────────────────

parse_cargo_toml "Cargo.toml"

echo "  Version: $(bold "$VERSION") (MSRV: $MSRV)"

# ── Preflight: Rust ───────────────────────────────────────────────

if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
  warn "Rust not found — installing via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck source=/dev/null
  source "${CARGO_HOME:-$HOME/.cargo}/env"
fi

RUST_VERSION=$(rustc --version | awk '{print $2}')
if ! version_gte "$RUST_VERSION" "$MSRV"; then
  die "Rust $RUST_VERSION is too old. ZeroClaw requires $MSRV+ (edition $EDITION). Run: rustup update stable"
fi
info "Rust $RUST_VERSION (>= $MSRV)"

# ── Preflight: 32-bit ARM ────────────────────────────────────────

case "$(uname -m)" in
  armv7l|armv6l|armhf)
    warn "32-bit ARM — prometheus requires 64-bit atomics, using --minimal + agent-runtime"
    MINIMAL=true
    USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}agent-runtime"
    ;;
esac

# ── Build feature flags ──────────────────────────────────────────

CARGO_ARGS=()

if [[ "$MINIMAL" == true ]]; then
  CARGO_ARGS+=(--no-default-features)
fi

if [[ -n "$USER_FEATURES" ]]; then
  # Validate each feature
  IFS=',' read -ra feats <<< "$USER_FEATURES"
  for feat in "${feats[@]}"; do
    feat=$(echo "$feat" | xargs) # trim whitespace
    [[ -n "$feat" ]] && validate_feature "$feat"
  done
  CARGO_ARGS+=(--features "$USER_FEATURES")
fi

# ── Detect reinstall ─────────────────────────────────────────────

if command -v zeroclaw >/dev/null 2>&1; then
  EXISTING=$(zeroclaw --version 2>/dev/null | awk '{print $NF}' || echo "unknown")
  warn "Existing install detected: v$EXISTING"
  if [[ "$MINIMAL" == true ]]; then
    echo
    read -rp "  --minimal will produce a reduced binary (no agent runtime by default). Continue? [Y/n] " confirm
    [[ "$confirm" =~ ^[Nn] ]] && { echo "Aborted."; exit 0; }
  fi
fi

# ── Build and install ─────────────────────────────────────────────

echo
echo "$(bold "Building ZeroClaw v$VERSION")"
if [[ ${#CARGO_ARGS[@]} -gt 0 ]]; then
  info "Feature flags: ${CARGO_ARGS[*]}"
else
  info "Feature flags: (defaults)"
fi
echo

cargo install --path . --locked --force "${CARGO_ARGS[@]}"

# ── Summary ───────────────────────────────────────────────────────

BIN="${CARGO_HOME:-$HOME/.cargo}/bin/zeroclaw"
if [[ -f "$BIN" ]]; then
  SIZE=$(du -h "$BIN" | awk '{print $1}')
  echo
  info "Installed: $BIN ($SIZE)"
else
  warn "Binary not found at expected path: $BIN"
fi

# ── Onboard ───────────────────────────────────────────────────────

if [[ "$SKIP_ONBOARD" == false ]] && command -v zeroclaw >/dev/null 2>&1; then
  echo
  echo "$(bold "Running setup wizard...")"
  echo
  zeroclaw onboard || warn "Onboard wizard exited with an error — run 'zeroclaw onboard' manually"
fi

echo
info "Done. Run $(bold "zeroclaw agent") to start chatting."
echo
