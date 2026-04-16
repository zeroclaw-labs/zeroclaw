#!/bin/sh
set -eu

# ── ZeroClaw installer ───────────────────────────────────────────
# Builds and installs ZeroClaw from source.
# All feature lists and version info read from Cargo.toml — nothing hardcoded.
# POSIX sh — no bash required. Works on Alpine, Debian, macOS, everywhere.

REPO_URL="https://github.com/zeroclaw-labs/zeroclaw.git"

# ── Output helpers (terminal-aware) ──────────────────────────────

if [ -t 1 ]; then
  BOLD='\033[1m' GREEN='\033[32m' YELLOW='\033[33m' RED='\033[31m' RESET='\033[0m'
else
  BOLD='' GREEN='' YELLOW='' RED='' RESET=''
fi

info()  { printf "  ${GREEN}✓${RESET} %s\n" "$*"; }
warn()  { printf "  ${YELLOW}⚠${RESET} %s\n" "$*" >&2; }
die()   { printf "  ${RED}✗${RESET} %s\n" "$*" >&2; exit 1; }
bold()  { printf "${BOLD}%s${RESET}" "$*"; }

# ── Parse Cargo.toml (source of truth) ────────────────────────────

parse_cargo_toml() {
  local toml="$1"
  [ -f "$toml" ] || die "Cargo.toml not found at $toml"

  VERSION=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{split($0,a,"\"");print a[2]}' "$toml")
  MSRV=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^rust-version *=/{split($0,a,"\"");print a[2]}' "$toml")
  EDITION=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^edition *=/{split($0,a,"\"");print a[2]}' "$toml")

  DEFAULT_FEATURES=$(awk '/^default *= *\[/,/\]/{s=$0; while(match(s,/"[^"]+"/)){print substr(s,RSTART+1,RLENGTH-2); s=substr(s,RSTART+RLENGTH)}}' "$toml" | paste -sd, -)

  ALL_FEATURES=$(awk '/^\[features\]/{p=1;next} /^\[/{p=0} p && /^[a-z][a-z0-9_-]* *=/{sub(/ *=.*/,"");print}' "$toml")
}

# ── Feature validation ────────────────────────────────────────────

validate_feature() {
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
  printf "%s — available build features\n" "$(bold "ZeroClaw v${VERSION}")"
  echo

  printf "  %s\n" "$(bold "Default") (included unless --minimal):"
  printf "    %s\n" "$DEFAULT_FEATURES"
  echo

  channels="" observability="" platform="" other=""
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

  [ -n "$channels" ]      && printf "  %s\n    %s\n\n" "$(bold "Channels:")" "$channels"
  [ -n "$observability" ] && printf "  %s\n    %s\n\n" "$(bold "Observability:")" "$observability"
  [ -n "$platform" ]      && printf "  %s\n    %s\n\n" "$(bold "Platform:")" "$platform"
  [ -n "$other" ]         && printf "  %s\n    %s\n\n" "$(bold "Other:")" "$other"

  printf "  %s\n" "$(bold "Build profiles:")"
  printf "    %s                                        # full (default features)\n" "$0"
  printf "    %s --minimal                              # kernel only (~6.6MB)\n" "$0"
  printf "    %s --minimal --features agent-runtime,channel-discord\n" "$0"
  echo
}

# ── Version comparison ────────────────────────────────────────────

version_gte() {
  # Returns 0 if $1 >= $2 (dot-separated version strings)
  local IFS=.
  set -- $1 $2
  local a1="${1:-0}" a2="${2:-0}" a3="${3:-0}"
  shift 3 2>/dev/null || shift $#
  local b1="${1:-0}" b2="${2:-0}" b3="${3:-0}"

  [ "$a1" -gt "$b1" ] 2>/dev/null && return 0
  [ "$a1" -lt "$b1" ] 2>/dev/null && return 1
  [ "$a2" -gt "$b2" ] 2>/dev/null && return 0
  [ "$a2" -lt "$b2" ] 2>/dev/null && return 1
  [ "$a3" -gt "$b3" ] 2>/dev/null && return 0
  [ "$a3" -lt "$b3" ] 2>/dev/null && return 1
  return 0
}

# ── Detect user's shell ──────────────────────────────────────────

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
  -V, --version        Show version from Cargo.toml

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
  ZEROCLAW_CARGO_FEATURES  Extra cargo features (legacy; prefer --features)
EOF
}

# ── Uninstall ─────────────────────────────────────────────────────

do_uninstall() {
  echo
  printf "%s\n" "$(bold "Uninstalling ZeroClaw")"
  echo

  local bin="$CARGO_HOME/bin/zeroclaw"

  if [ -f "$bin" ]; then
    "$bin" service stop 2>/dev/null || true
    "$bin" service uninstall 2>/dev/null || true
    rm -f "$bin"
    info "Removed $bin"
  else
    warn "Binary not found at $bin"
  fi

  local config_dir="$PREFIX/.zeroclaw"
  if [ -d "$config_dir" ]; then
    if [ -t 0 ]; then
      printf "  Remove config and data (%s)? [y/N] " "$config_dir"
      read confirm
      case "$confirm" in
        [Yy]*) rm -rf "$config_dir"; info "Removed $config_dir" ;;
        *)     info "Config preserved at $config_dir" ;;
      esac
    else
      info "Config preserved at $config_dir (non-interactive — use rm -rf to remove)"
    fi
  fi

  # Check if another zeroclaw still lurks in PATH
  local other_bin
  other_bin=$(PATH="$ORIGINAL_PATH" command -v zeroclaw 2>/dev/null || true)
  if [ -n "$other_bin" ]; then
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
if [ -n "${ZEROCLAW_CARGO_FEATURES:-}" ]; then
  USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$ZEROCLAW_CARGO_FEATURES"
fi

while [ $# -gt 0 ]; do
  case "$1" in
    --minimal)        MINIMAL=true ;;
    --features)
      if [ $# -lt 2 ]; then
        die "Missing value for --features. Expected: --features X,Y"
      fi
      shift; USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$1" ;;
    --list-features)  LIST_FEATURES=true ;;
    --prefix)
      if [ $# -lt 2 ]; then
        die "Missing value for --prefix. Expected: --prefix /path"
      fi
      shift; PREFIX=$(echo "$1" | sed 's|/*$||') ;;
    --dry-run)        DRY_RUN=true ;;
    --skip-onboard)   SKIP_ONBOARD=true ;;
    --uninstall)      UNINSTALL=true ;;
    -h|--help)        usage; exit 0 ;;
    -V|--version)
      if [ -f "Cargo.toml" ]; then
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

CARGO_HOME="${CARGO_HOME:-$PREFIX/.cargo}"
RUSTUP_HOME="${RUSTUP_HOME:-$PREFIX/.rustup}"
INSTALL_DIR="${ZEROCLAW_INSTALL_DIR:-$PREFIX/.zeroclaw/src}"
ORIGINAL_PATH="$PATH"
PATH="$CARGO_HOME/bin:$PATH"
export CARGO_HOME RUSTUP_HOME PATH

[ "$UNINSTALL" = true ] && do_uninstall

# ── List features (can run without cloning if in repo) ────────────

if [ "$LIST_FEATURES" = true ]; then
  if [ -f "Cargo.toml" ]; then
    list_features "Cargo.toml"
  elif [ -f "$INSTALL_DIR/Cargo.toml" ]; then
    list_features "$INSTALL_DIR/Cargo.toml"
  else
    die "No Cargo.toml found. Clone the repo first or run from the repo root."
  fi
  exit 0
fi

# ── Locate source ─────────────────────────────────────────────────

echo
printf "%s\n" "$(bold "ZeroClaw — source install")"
if [ "$PREFIX" != "$HOME" ]; then
  printf "  prefix: %s\n" "$(bold "$PREFIX")"
fi
echo

if [ -f "Cargo.toml" ] && grep -q "zeroclaw" "Cargo.toml" 2>/dev/null; then
  INSTALL_DIR="$(pwd)"
  info "Building from $(pwd)"
elif [ -d "$INSTALL_DIR/.git" ]; then
  info "Updating source in $INSTALL_DIR"
  git -C "$INSTALL_DIR" pull --ff-only --quiet 2>/dev/null || {
    warn "Fast-forward pull failed — resetting to origin/master"
    git -C "$INSTALL_DIR" fetch origin master --quiet
    git -C "$INSTALL_DIR" reset --hard origin/master --quiet
  }
  cd "$INSTALL_DIR"
else
  info "Cloning into $INSTALL_DIR"
  mkdir -p "$(dirname "$INSTALL_DIR")"
  git clone --depth 1 "$REPO_URL" "$INSTALL_DIR"
  cd "$INSTALL_DIR"
fi

# ── Parse Cargo.toml ──────────────────────────────────────────────

parse_cargo_toml "Cargo.toml"

printf "  Version: %s (MSRV: %s, edition: %s)\n" "$(bold "$VERSION")" "$MSRV" "$EDITION"

# ── Preflight: Rust ───────────────────────────────────────────────

NEED_RUST=false
if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
  NEED_RUST=true
elif [ "$PREFIX" != "$HOME" ] && [ ! -d "$RUSTUP_HOME/toolchains" ]; then
  NEED_RUST=true
fi

if [ "$NEED_RUST" = true ]; then
  if [ "$DRY_RUN" = true ]; then
    warn "[dry-run] Would install Rust via rustup into $RUSTUP_HOME"
  else
    warn "Installing Rust via rustup into $CARGO_HOME"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
      --no-modify-path --default-toolchain stable
    . "$CARGO_HOME/env"
  fi
fi

if [ "$DRY_RUN" != true ]; then
  RUST_VERSION=$(rustc --version | awk '{print $2}')
  if ! version_gte "$RUST_VERSION" "$MSRV"; then
    die "Rust $RUST_VERSION is too old. ZeroClaw requires $MSRV+ (edition $EDITION). Run: rustup update stable"
  fi
  info "Rust $RUST_VERSION (>= $MSRV)"
fi

# ── Preflight: 32-bit ARM ────────────────────────────────────────

case "$(uname -m)" in
  armv7l|armv6l|armhf)
    die "32-bit ARM detected — the default feature 'observability-prometheus'
requires 64-bit atomics and will not compile on this architecture.

Example (full agent without prometheus):
  $0 --minimal --features agent-runtime,schema-export

See all available features:
  $0 --list-features"
    ;;
esac

# ── Build feature flags ──────────────────────────────────────────

CARGO_FLAGS=""

if [ "$MINIMAL" = true ]; then
  CARGO_FLAGS="--no-default-features"
fi

if [ -n "$USER_FEATURES" ]; then
  # Normalize: treat commas, spaces, tabs as delimiters; deduplicate; trim empty
  USER_FEATURES=$(printf '%s' "$USER_FEATURES" | tr ',[:space:]' '\n' | grep -v '^$' | sort -u | paste -sd, - || true)

  if [ -n "$USER_FEATURES" ]; then
    # Validate each feature
    OLD_IFS="$IFS"
    IFS=','
    for feat in $USER_FEATURES; do
      [ -n "$feat" ] && validate_feature "$feat"
    done
    IFS="$OLD_IFS"
    CARGO_FLAGS="$CARGO_FLAGS --features $USER_FEATURES"
  fi
fi

# ── Detect existing installs ──────────────────────────────────────

PATH_BIN=$(PATH="$ORIGINAL_PATH" command -v zeroclaw 2>/dev/null || true)
if [ -n "$PATH_BIN" ]; then
  PATH_VERSION=$("$PATH_BIN" --version 2>/dev/null | awk '{print $NF}' || echo "unknown")
  TARGET_BIN="$CARGO_HOME/bin/zeroclaw"
  if [ "$PATH_BIN" != "$TARGET_BIN" ]; then
    warn "zeroclaw found at $PATH_BIN (v$PATH_VERSION)"
    warn "This install targets $TARGET_BIN"
    warn "The old binary will shadow the new one unless removed or PATH is reordered"
  else
    warn "Existing install: $PATH_BIN (v$PATH_VERSION)"
  fi
  if [ "$MINIMAL" = true ] && [ "$DRY_RUN" != true ]; then
    if [ -t 0 ]; then
      printf "  --minimal will produce a reduced binary (no agent runtime by default). Continue? [Y/n] "
      read confirm
      case "$confirm" in
        [Nn]*) echo "Aborted."; exit 0 ;;
      esac
    fi
  fi
fi

# ── Dry run ───────────────────────────────────────────────────────

if [ "$DRY_RUN" = true ]; then
  echo
  printf "%s\n" "$(bold "Dry run — nothing will be built or installed")"
  echo
  info "Source:   $INSTALL_DIR"
  info "Binary:   $CARGO_HOME/bin/zeroclaw"
  info "Config:   $PREFIX/.zeroclaw/"
  info "Rust:     $CARGO_HOME (CARGO_HOME), $RUSTUP_HOME (RUSTUP_HOME)"
  echo
  if [ -n "$CARGO_FLAGS" ]; then
    info "cargo install --path . --locked --force $CARGO_FLAGS"
  else
    info "cargo install --path . --locked --force"
  fi

  EXPORT_LINE=$(shell_export_syntax)
  PROFILE=$(detect_shell_profile)
  echo
  printf "  %s (%s):\n" "$(bold "Shell profile")" "$PROFILE"
  printf "    %s\n" "$EXPORT_LINE"
  echo
  exit 0
fi

# ── Build and install ─────────────────────────────────────────────

echo
printf "%s\n" "$(bold "Building ZeroClaw v$VERSION")"
if [ -n "$CARGO_FLAGS" ]; then
  info "Feature flags: $CARGO_FLAGS"
else
  info "Feature flags: (defaults)"
fi
echo

# shellcheck disable=SC2086
cargo install --path . --locked --force $CARGO_FLAGS

# ── Summary ───────────────────────────────────────────────────────

BIN="$CARGO_HOME/bin/zeroclaw"
if [ -f "$BIN" ]; then
  SIZE=$(du -h "$BIN" | awk '{print $1}')
  NEW_VERSION=$("$BIN" --version 2>/dev/null | awk '{print $NF}' || echo "$VERSION")
  echo
  info "Installed: $BIN (v$NEW_VERSION, $SIZE)"

  ACTIVE_BIN=$(PATH="$ORIGINAL_PATH" command -v zeroclaw 2>/dev/null || true)
  if [ -n "$ACTIVE_BIN" ] && [ "$ACTIVE_BIN" != "$BIN" ]; then
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

SHOW_PATH_HELP=false
if [ "$PREFIX" != "$HOME" ]; then
  SHOW_PATH_HELP=true
elif [ -f "$PROFILE" ] && ! grep -q "$CARGO_HOME/bin" "$PROFILE" 2>/dev/null; then
  SHOW_PATH_HELP=true
elif [ ! -f "$PROFILE" ]; then
  SHOW_PATH_HELP=true
fi

if [ "$SHOW_PATH_HELP" = true ]; then
  echo
  printf "  %s (%s):\n" "$(bold "Add to your shell profile")" "$PROFILE"
  echo
  printf "    %s\n" "$EXPORT_LINE"
  echo
  printf "  Then reload:\n"
  echo
  printf "    source %s\n" "$PROFILE"
  echo
fi

# ── Onboard ───────────────────────────────────────────────────────

if [ "$SKIP_ONBOARD" = false ] && [ -f "$BIN" ]; then
  if [ -t 0 ]; then
    echo
    printf "%s\n" "$(bold "Running setup wizard...")"
    echo
    "$BIN" onboard || warn "Onboard wizard exited with an error — run 'zeroclaw onboard' manually"
  else
    info "Non-interactive — skipping onboard wizard. Run 'zeroclaw onboard' to configure."
  fi
fi

echo
info "Done. Run $(bold "zeroclaw agent") to start chatting."
echo
