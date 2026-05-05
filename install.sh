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

# ── Platform / target triple detection ───────────────────────────

detect_target_triple() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)

  case "$os" in
    Darwin) echo "aarch64-apple-darwin" ;;   # presume M-series
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

# ── Pre-built binary install ──────────────────────────────────────

install_prebuilt() {
  local triple version asset_name asset_url sha256_url tmp_dir web_data_dir
  triple=$(detect_target_triple)

  if [ -z "$triple" ]; then
    warn "No pre-built binary for this platform — falling back to source build"
    return 1
  fi

  # Resolve latest release version via GitHub API
  version=$(curl -fsSL "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\(.*\)".*/\1/')

  if [ -z "$version" ]; then
    warn "Could not resolve latest release — falling back to source build"
    return 1
  fi

  asset_name="zeroclaw-${triple}.tar.gz"
  asset_url="https://github.com/zeroclaw-labs/zeroclaw/releases/download/${version}/${asset_name}"
  sha256_url="https://github.com/zeroclaw-labs/zeroclaw/releases/download/${version}/SHA256SUMS"

  echo
  printf "%s\n" "$(bold "Installing ZeroClaw ${version} (pre-built)")"
  info "Platform: $triple"
  info "Source:   $asset_url"
  echo

  # Resolve platform-correct web data directory to match gateway auto-detect
  case "$(uname -s)" in
    Darwin)
      web_data_dir="${HOME}/Library/Application Support/zeroclaw/web/dist"
      ;;
    MINGW*|CYGWIN*|MSYS*)
      web_data_dir="${LOCALAPPDATA}/zeroclaw/web/dist"
      ;;
    *)
      web_data_dir="${XDG_DATA_HOME:-${PREFIX}/.local/share}/zeroclaw/web/dist"
      ;;
  esac

  if [ "$DRY_RUN" = true ]; then
    info "[dry-run] Would download $asset_url"
    info "[dry-run] Would install to $CARGO_HOME/bin/zeroclaw"
    info "[dry-run] Would install web dashboard to $web_data_dir"
    return 0
  fi

  tmp_dir=$(mktemp -d)
  trap 'rm -rf "$tmp_dir"' EXIT

  curl -fSL --progress-bar "$asset_url" -o "$tmp_dir/$asset_name" \
    || { warn "Download failed — falling back to source build"; rm -rf "$tmp_dir"; return 1; }

  # Verify checksum — all failure modes fall back to source rather than install unverified
  if ! curl -fsSL "$sha256_url" -o "$tmp_dir/SHA256SUMS" 2>/dev/null; then
    warn "Could not fetch SHA256SUMS — falling back to source build"
    rm -rf "$tmp_dir"; return 1
  fi

  expected=$(grep "$asset_name" "$tmp_dir/SHA256SUMS" | awk '{print $1}')
  if [ -z "$expected" ]; then
    warn "Asset not found in SHA256SUMS — falling back to source build"
    rm -rf "$tmp_dir"; return 1
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp_dir/$asset_name" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp_dir/$asset_name" | awk '{print $1}')
  else
    warn "No checksum tool available (sha256sum/shasum) — falling back to source build"
    rm -rf "$tmp_dir"; return 1
  fi

  if [ "$actual" != "$expected" ]; then
    die "Checksum mismatch — download may be corrupt. Expected: $expected  Got: $actual"
  fi
  info "Checksum verified"

  tar -xzf "$tmp_dir/$asset_name" -C "$tmp_dir"
  mkdir -p "$CARGO_HOME/bin"
  install -m 755 "$tmp_dir/zeroclaw" "$CARGO_HOME/bin/zeroclaw"

  # Install web dashboard assets bundled in the release tarball
  if [ -d "$tmp_dir/web/dist" ]; then
    mkdir -p "$web_data_dir"
    cp -r "$tmp_dir/web/dist/." "$web_data_dir/"
    info "Web dashboard installed to $web_data_dir"
  fi

  rm -rf "$tmp_dir"
  trap - EXIT
  return 0
}

# ── Usage ─────────────────────────────────────────────────────────

usage() {
  cat <<EOF
$(bold "ZeroClaw installer")

Usage: $0 [options]

Options:
  --prebuilt           Download and install a pre-built binary (default when asked)
  --source             Build from source (skips the pre-built prompt)
  --preset NAME        Named feature preset: 'minimal' (kernel only, ~6.6MB) or
                       'full' (default features). Source builds only.
  --minimal            Alias for --preset minimal
  --features X,Y       Select specific features — source only (comma-separated)
  --with-gateway       Force the gateway feature on (overrides preset/feature default)
  --without-gateway    Force the gateway feature off (overrides preset/feature default)
  --list-features      Print all available features and exit
  --prefix PATH        Install everything under PATH (default: \$HOME)
                       Sets CARGO_HOME, RUSTUP_HOME, source checkout, config
  --dry-run            Show what would happen without building or installing
  --skip-onboard       Skip the post-install onboarding prompt
  --uninstall          Remove ZeroClaw binary and optionally config/data
  -h, --help           Show this help
  -V, --version        Show version from Cargo.toml

Examples:
  $0                                           # interactive: asks prebuilt or source
  $0 --prebuilt                                # download pre-built binary (fast)
  $0 --source                                  # always build from source
  $0 --source --minimal                        # smallest possible binary
  $0 --source --features agent-runtime,channel-discord  # custom feature set
  $0 --skip-onboard                            # install only, configure later
  $0 --prefix /tmp/zc-test --skip-onboard      # isolated test install
  $0 --dry-run --prebuilt                      # preview without installing
  $0 --uninstall                               # remove ZeroClaw

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
INSTALL_MODE=""   # ""=ask, "prebuilt"=force prebuilt, "source"=force source
PRESET=""         # ""=unset, "minimal"=alias for --minimal, "full"=default-features
WITH_GATEWAY=""   # ""=unset (preset/feature default applies), "true"/"false"=explicit toggle

# Support legacy env var
if [ -n "${ZEROCLAW_CARGO_FEATURES:-}" ]; then
  USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$ZEROCLAW_CARGO_FEATURES"
fi

while [ $# -gt 0 ]; do
  case "$1" in
    --minimal)        MINIMAL=true ;;
    --preset)
      if [ $# -lt 2 ]; then
        die "Missing value for --preset. Expected: --preset minimal|full"
      fi
      shift
      case "$1" in
        minimal) PRESET="minimal"; MINIMAL=true ;;
        full)    PRESET="full" ;;
        *)       die "Unknown preset '$1'. Expected: minimal or full" ;;
      esac ;;
    --features)
      if [ $# -lt 2 ]; then
        die "Missing value for --features. Expected: --features X,Y"
      fi
      shift; USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$1" ;;
    --with-gateway)    WITH_GATEWAY="true" ;;
    --without-gateway) WITH_GATEWAY="false" ;;
    --list-features)  LIST_FEATURES=true ;;
    --prefix)
      if [ $# -lt 2 ]; then
        die "Missing value for --prefix. Expected: --prefix /path"
      fi
      shift; PREFIX=$(echo "$1" | sed 's|/*$||') ;;
    --dry-run)        DRY_RUN=true ;;
    --skip-onboard)   SKIP_ONBOARD=true ;;
    --prebuilt)       INSTALL_MODE="prebuilt" ;;
    --source)         INSTALL_MODE="source" ;;
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

# ── Decide: pre-built or source ───────────────────────────────────

# --minimal or --features imply source
if [ "$MINIMAL" = true ] || [ -n "$USER_FEATURES" ]; then
  INSTALL_MODE="source"
fi

if [ "$INSTALL_MODE" = "" ]; then
  triple=$(detect_target_triple)
  if [ -n "$triple" ]; then
    if [ -t 0 ]; then
      echo
      printf "  %s\n" "$(bold "How would you like to install ZeroClaw?")"
      printf "  [P] Pre-built binary  — fast, no Rust required  %s\n" "$(bold "(default)")"
      printf "  [s] Build from source — custom features, latest code\n"
      printf "\n  Choice [P/s]: "
      read install_choice
      case "$install_choice" in
        [Ss]*) INSTALL_MODE="source" ;;
        *)     INSTALL_MODE="prebuilt" ;;
      esac
    else
      # Non-interactive (curl | bash): default to pre-built silently
      INSTALL_MODE="prebuilt"
    fi
  else
    INSTALL_MODE="source"
  fi
fi

if [ "$INSTALL_MODE" = "prebuilt" ]; then
  if install_prebuilt; then
    PREBUILT_OK=true
  else
    warn "Pre-built install failed — continuing with source build"
    INSTALL_MODE="source"
    PREBUILT_OK=false
  fi
fi

[ "${PREBUILT_OK:-false}" = true ] && [ "$DRY_RUN" != true ] && {
  BIN="$CARGO_HOME/bin/zeroclaw"
  if [ -f "$BIN" ]; then
    NEW_VERSION=$("$BIN" --version 2>/dev/null | awk '{print $NF}' || echo "?")
    SIZE=$(du -h "$BIN" | awk '{print $1}')
    echo
    info "Installed: $BIN (v$NEW_VERSION, $SIZE)"
  fi
}

# ── Locate source ─────────────────────────────────────────────────

[ "${PREBUILT_OK:-false}" = true ] && {
  # Jump past the source build to PATH + onboard
  SOURCE_SKIPPED=true
}

if [ "${SOURCE_SKIPPED:-false}" != true ]; then

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

fi  # end source build block

BIN="$CARGO_HOME/bin/zeroclaw"

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

if [ "$SKIP_ONBOARD" = false ] && [ "$DRY_RUN" != true ] && [ -f "$BIN" ]; then
  if [ -t 0 ]; then
    # Per #6292: present a 3-way choice rather than launching CLI onboard
    # unconditionally. Skip the prompt and auto-launch CLI onboard if
    # onboarding is not needed (status check would say "already onboarded"),
    # or fall through to non-interactive skip when stdin is not a TTY.
    echo
    printf "%s\n" "$(bold "ZeroClaw installed. How would you like to complete onboarding?")"
    printf "  [1] CLI/TUI  (zeroclaw onboard)\n"
    printf "  [2] Open gateway in browser (zeroclaw daemon + dashboard)\n"
    printf "  [3] Skip for now\n"
    printf "  Choice [1-3, default 1]: "
    read onboard_choice
    case "${onboard_choice:-1}" in
      1|"")
        echo
        "$BIN" onboard || warn "Onboard wizard exited with an error — run 'zeroclaw onboard' manually"
        ;;
      2)
        echo
        info "Starting gateway daemon for browser-based onboarding..."
        info "Open the dashboard in your browser; pair with the code shown in logs."
        info "Stop the daemon with Ctrl+C when done; then run 'zeroclaw service install' for always-on."
        "$BIN" daemon || warn "Daemon exited with an error — run 'zeroclaw daemon' manually"
        ;;
      3)
        info "Skipped onboarding. Run 'zeroclaw onboard' (CLI) or 'zeroclaw daemon' (browser) when ready."
        ;;
      *)
        warn "Unknown choice '$onboard_choice' — skipping. Run 'zeroclaw onboard' to configure."
        ;;
    esac
  else
    info "Non-interactive — skipping onboard prompt. Run 'zeroclaw onboard' to configure."
  fi
fi

echo
info "Done. Run $(bold "zeroclaw agent") to start chatting."
echo
