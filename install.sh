#!/usr/bin/env bash
set -euo pipefail

# ── ZeroClaw installer ───────────────────────────────────────────
# Builds and installs ZeroClaw from source.
# All feature lists and version info read from Cargo.toml — nothing hardcoded.

REPO_URL="https://github.com/zeroclaw-labs/zeroclaw.git"

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
  local a b
  IFS='.' read -ra a <<< "$1"
  IFS='.' read -ra b <<< "$2"
  local i len=${#b[@]}
  (( ${#a[@]} > len )) && len=${#a[@]}
  for ((i=0; i<len; i++)); do
    local av=${a[i]:-0} bv=${b[i]:-0}
    (( 10#$av > 10#$bv )) && return 0
    (( 10#$av < 10#$bv )) && return 1
  done
  return 0
}

# ── Detect user's shell ──────────────────────────────────────────

detect_shell_profile() {
  local shell_name
  shell_name=$(basename "${SHELL:-/bin/bash}")
  case "$shell_name" in
    zsh)  echo "$PREFIX/.zshrc" ;;
    fish) echo "$PREFIX/.config/fish/config.fish" ;;
    *)    echo "$PREFIX/.bashrc" ;;
  esac
}

shell_export_syntax() {
  local shell_name
  shell_name=$(basename "${SHELL:-/bin/bash}")
  case "$shell_name" in
    fish)
      echo "set -gx PATH \"$CARGO_HOME/bin\" \$PATH"
      ;;
    *)
      echo "export PATH=\"$CARGO_HOME/bin:\$PATH\""
      ;;
  esac
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
  --prefix PATH        Install everything under PATH (default: \$HOME)
                       Sets CARGO_HOME, RUSTUP_HOME, source checkout, config
  --dry-run            Show what would happen without building or installing
  --skip-onboard       Skip the setup wizard after install
  --uninstall          Remove ZeroClaw binary and optionally config/data
  -h, --help           Show this help

Examples:
  $0                                          # full install (interactive)
  $0 --minimal                                # smallest possible binary
  $0 --features agent-runtime,channel-discord  # custom feature set
  $0 --skip-onboard                           # build only, configure later
  $0 --prefix /tmp/zc-test --skip-onboard     # isolated test install
  $0 --dry-run --minimal                      # preview without building
  $0 --uninstall                              # remove ZeroClaw

Environment:
  ZEROCLAW_INSTALL_DIR   Source checkout override (default: PREFIX/.zeroclaw/src)
EOF
}

# ── Uninstall ─────────────────────────────────────────────────────

do_uninstall() {
  echo
  echo "$(bold "Uninstalling ZeroClaw")"
  echo

  local bin="$CARGO_HOME/bin/zeroclaw"

  # Stop/remove service BEFORE deleting the binary
  if [[ -f "$bin" ]]; then
    "$bin" service stop 2>/dev/null || true
    "$bin" service uninstall 2>/dev/null || true
    rm -f "$bin"
    info "Removed $bin"
  else
    warn "Binary not found at $bin"
  fi

  local config_dir="$PREFIX/.zeroclaw"
  if [[ -d "$config_dir" ]]; then
    if [[ -t 0 ]]; then
      echo
      read -rp "  Remove config and data ($config_dir)? [y/N] " confirm
      if [[ "$confirm" =~ ^[Yy] ]]; then
        rm -rf "$config_dir"
        info "Removed $config_dir"
      else
        info "Config preserved at $config_dir"
      fi
    else
      info "Config preserved at $config_dir (non-interactive — use rm -rf to remove)"
    fi
  fi

  # Check if another zeroclaw still lurks in PATH
  local other_bin
  other_bin=$(PATH="$ORIGINAL_PATH" command -v zeroclaw 2>/dev/null || true)
  if [[ -n "$other_bin" ]]; then
    local other_version
    other_version=$("$other_bin" --version 2>/dev/null | awk '{print $NF}' || echo "unknown")
    echo
    warn "Another zeroclaw found at $other_bin (v$other_version)"
    warn "Remove it manually if you want a full uninstall"
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
DRY_RUN=false
PREFIX="$HOME"

# Support legacy env var
if [[ -n "${ZEROCLAW_CARGO_FEATURES:-}" ]]; then
  USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$ZEROCLAW_CARGO_FEATURES"
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    --minimal)        MINIMAL=true ;;
    --features)       shift; USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$1" ;;
    --list-features)  LIST_FEATURES=true ;;
    --prefix)         shift; PREFIX="$(echo "$1" | sed 's|/*$||')" ;;
    --dry-run)        DRY_RUN=true ;;
    --skip-onboard)   SKIP_ONBOARD=true ;;
    --uninstall)      UNINSTALL=true ;;
    -h|--help)        usage; exit 0 ;;
    -V|--version)
      if [[ -f "Cargo.toml" ]]; then
        parse_cargo_toml "Cargo.toml"
        echo "install.sh for ZeroClaw v$VERSION"
      else
        echo "install.sh (version unknown — not in repo)"
      fi
      exit 0 ;;
    *) die "Unknown option: $1. Run: $0 --help" ;;
  esac
  shift
done

# ── Derive paths from prefix ─────────────────────────────────────

export CARGO_HOME="${CARGO_HOME:-$PREFIX/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$PREFIX/.rustup}"
INSTALL_DIR="${ZEROCLAW_INSTALL_DIR:-$PREFIX/.zeroclaw/src}"
ORIGINAL_PATH="$PATH"
export PATH="$CARGO_HOME/bin:$PATH"

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

# ── Locate source ─────────────────────────────────────────────────

echo
echo "$(bold "ZeroClaw — source install")"
if [[ "$PREFIX" != "$HOME" ]]; then
  echo "  prefix: $(bold "$PREFIX")"
fi
echo

if [[ -f "Cargo.toml" ]] && grep -q "zeroclaw" "Cargo.toml" 2>/dev/null; then
  # Already in the repo — build from here
  INSTALL_DIR="$(pwd)"
  info "Building from $(pwd)"
elif [[ -d "$INSTALL_DIR/.git" ]]; then
  info "Updating source in $INSTALL_DIR"
  git -C "$INSTALL_DIR" pull --ff-only --quiet 2>/dev/null || \
    git -C "$INSTALL_DIR" fetch origin master --quiet
  cd "$INSTALL_DIR"
else
  info "Cloning into $INSTALL_DIR"
  mkdir -p "$(dirname "$INSTALL_DIR")"
  git clone --depth 1 "$REPO_URL" "$INSTALL_DIR"
  cd "$INSTALL_DIR"
fi

# ── Parse Cargo.toml ──────────────────────────────────────────────

parse_cargo_toml "Cargo.toml"

echo "  Version: $(bold "$VERSION") (MSRV: $MSRV, edition: $EDITION)"

# ── Preflight: Rust ───────────────────────────────────────────────

NEED_RUST=false
if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
  NEED_RUST=true
elif [[ "$PREFIX" != "$HOME" && ! -d "$RUSTUP_HOME/toolchains" ]]; then
  # Custom prefix but no local toolchain — system Rust won't work with our RUSTUP_HOME
  NEED_RUST=true
fi

if [[ "$NEED_RUST" == true ]]; then
  if [[ "$DRY_RUN" == true ]]; then
    warn "[dry-run] Would install Rust via rustup into $RUSTUP_HOME"
  else
    warn "Installing Rust via rustup into $CARGO_HOME"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
      --no-modify-path --default-toolchain stable
    # shellcheck source=/dev/null
    source "$CARGO_HOME/env"
  fi
fi

if [[ "$DRY_RUN" != true ]]; then
  RUST_VERSION=$(rustc --version | awk '{print $2}')
  if ! version_gte "$RUST_VERSION" "$MSRV"; then
    die "Rust $RUST_VERSION is too old. ZeroClaw requires $MSRV+ (edition $EDITION). Run: rustup update stable"
  fi
  info "Rust $RUST_VERSION (>= $MSRV)"
fi

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
  # Normalize: treat commas, spaces, and tabs as delimiters; deduplicate; trim empty
  USER_FEATURES=$(echo "$USER_FEATURES" | tr ',[:space:]' '\n' | grep -v '^$' | sort -u | paste -sd, - || true)

  # Skip if normalization emptied the string
  if [[ -n "$USER_FEATURES" ]]; then
    # Validate each feature
    IFS=',' read -ra feats <<< "$USER_FEATURES"
    for feat in "${feats[@]}"; do
      [[ -n "$feat" ]] && validate_feature "$feat"
    done
    CARGO_ARGS+=(--features "$USER_FEATURES")
  fi
fi

# ── Detect existing installs ──────────────────────────────────────

# Check what's in the user's actual PATH (not our modified one)
PATH_BIN=$(PATH="$ORIGINAL_PATH" command -v zeroclaw 2>/dev/null || true)
if [[ -n "$PATH_BIN" ]]; then
  PATH_VERSION=$("$PATH_BIN" --version 2>/dev/null | awk '{print $NF}' || echo "unknown")
  TARGET_BIN="$CARGO_HOME/bin/zeroclaw"
  if [[ "$PATH_BIN" != "$TARGET_BIN" ]]; then
    warn "zeroclaw found at $PATH_BIN (v$PATH_VERSION)"
    warn "This install targets $TARGET_BIN"
    warn "The old binary will shadow the new one unless removed or PATH is reordered"
  else
    warn "Existing install: $PATH_BIN (v$PATH_VERSION)"
  fi
  if [[ "$MINIMAL" == true && "$DRY_RUN" != true ]]; then
    if [[ -t 0 ]]; then
      echo
      read -rp "  --minimal will produce a reduced binary (no agent runtime by default). Continue? [Y/n] " confirm
      [[ "$confirm" =~ ^[Nn] ]] && { echo "Aborted."; exit 0; }
    fi
  fi
fi

# ── Dry run ───────────────────────────────────────────────────────

if [[ "$DRY_RUN" == true ]]; then
  echo
  echo "$(bold "Dry run — nothing will be built or installed")"
  echo
  info "Source:   $INSTALL_DIR"
  info "Binary:   $CARGO_HOME/bin/zeroclaw"
  info "Config:   $PREFIX/.zeroclaw/"
  info "Rust:     $CARGO_HOME (CARGO_HOME), $RUSTUP_HOME (RUSTUP_HOME)"
  echo
  if [[ ${#CARGO_ARGS[@]} -gt 0 ]]; then
    info "cargo install --path . --locked --force ${CARGO_ARGS[*]}"
  else
    info "cargo install --path . --locked --force"
  fi

  EXPORT_LINE=$(shell_export_syntax)
  PROFILE=$(detect_shell_profile)
  echo
  echo "  $(bold "Shell profile") ($PROFILE):"
  echo "    $EXPORT_LINE"
  echo
  exit 0
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

BIN="$CARGO_HOME/bin/zeroclaw"
if [[ -f "$BIN" ]]; then
  SIZE=$(du -h "$BIN" | awk '{print $1}')
  NEW_VERSION=$("$BIN" --version 2>/dev/null | awk '{print $NF}' || echo "$VERSION")
  echo
  info "Installed: $BIN (v$NEW_VERSION, $SIZE)"

  # Check if something else shadows it in the user's actual PATH
  ACTIVE_BIN=$(PATH="$ORIGINAL_PATH" command -v zeroclaw 2>/dev/null || true)
  if [[ -n "$ACTIVE_BIN" && "$ACTIVE_BIN" != "$BIN" ]]; then
    ACTIVE_VERSION=$("$ACTIVE_BIN" --version 2>/dev/null | awk '{print $NF}' || echo "unknown")
    echo
    warn "$(bold "WARNING:") zeroclaw in your PATH is $ACTIVE_BIN (v$ACTIVE_VERSION)"
    warn "It will shadow the v$NEW_VERSION binary you just installed at $BIN"
    warn "Fix: remove the old binary or put $CARGO_HOME/bin earlier in your PATH"
  fi
else
  warn "Binary not found at expected path: $BIN"
fi

# ── PATH guidance ─────────────────────────────────────────────────

PROFILE=$(detect_shell_profile)
EXPORT_LINE=$(shell_export_syntax)

# Always show for custom prefix; for default prefix, check if profile has it
SHOW_PATH_HELP=false
if [[ "$PREFIX" != "$HOME" ]]; then
  SHOW_PATH_HELP=true
elif [[ -f "$PROFILE" ]] && ! grep -q "$CARGO_HOME/bin" "$PROFILE" 2>/dev/null; then
  SHOW_PATH_HELP=true
elif [[ ! -f "$PROFILE" ]]; then
  SHOW_PATH_HELP=true
fi

if [[ "$SHOW_PATH_HELP" == true ]]; then
  echo
  echo "  $(bold "Add to your shell profile") ($PROFILE):"
  echo
  echo "    $EXPORT_LINE"
  echo
  echo "  Then reload:"
  echo
  echo "    source $PROFILE"
  echo
fi

# ── Onboard ───────────────────────────────────────────────────────

if [[ "$SKIP_ONBOARD" == false && -f "$BIN" ]]; then
  echo
  echo "$(bold "Running setup wizard...")"
  echo
  "$BIN" onboard || warn "Onboard wizard exited with an error — run 'zeroclaw onboard' manually"
fi

echo
info "Done. Run $(bold "zeroclaw agent") to start chatting."
echo
