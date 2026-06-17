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

info() { printf "  ${GREEN}✓${RESET} %s\n" "$*"; }
warn() { printf "  ${YELLOW}⚠${RESET} %s\n" "$*" >&2; }
die() {
  printf "  ${RED}✗${RESET} %s\n" "$*" >&2
  exit 1
}
bold() { printf "${BOLD}%s${RESET}" "$*"; }

TUI_BIN_NAME="zerocode"

# Apps installed by default (the rest are discovered and listed but off
# until selected via --apps or the interactive picker). Intentionally a
# fixed list: zeroclaw-desktop needs the Tauri toolchain + webview deps,
# so it ships off-by-default.
DEFAULT_APPS="zerocode"

# ── Parse Cargo.toml (source of truth) ────────────────────────────

parse_cargo_toml() {
  local toml="$1"
  [ -f "$toml" ] || die "Cargo.toml not found at $toml"

  VERSION=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{split($0,a,"\"");print a[2]}' "$toml")
  MSRV=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^rust-version *=/{split($0,a,"\"");print a[2]}' "$toml")
  EDITION=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p && /^edition *=/{split($0,a,"\"");print a[2]}' "$toml")

  DEFAULT_FEATURES=$(feature_members "$toml" default | paste -sd, -)

  ALL_FEATURES=$(awk '/^\[features\]/{p=1;next} /^\[/{p=0} p && /^[a-z][a-z0-9_-]* *=/{sub(/ *=.*/,"");print}' "$toml")
}

# Print the members of one feature from `[features]`, one per line. Spans
# multi-line array literals. The single source of truth for reading the
# feature graph out of Cargo.toml.
feature_members() {
  awk -v key="$2" '
    $0 ~ "^" key " *= *\\[" {p=1}
    p {while (match($0,/"[^"]+"/)) {print substr($0,RSTART+1,RLENGTH-2); $0=substr($0,RSTART+RLENGTH)}}
    p && /\]/ {exit}
  ' "$1"
}

# Aggregate/meta features and deprecated aliases: internal groupings, not
# individual picker rows. The single source of truth for what to skip when
# rendering rows and what to expand when resolving `default`.
NON_ROW_FEATURES="default default-channels channels-full ci-all fantoccini landlock metrics embedded-web"

is_aggregate() {
  case " $NON_ROW_FEATURES " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}

# Expand `default` to the picker rows it implies: walk aggregates
# (default-channels, etc.) until only real feature names remain. Reads the
# graph from Cargo.toml — no hardcoded channel list.
expand_default_features() {
  local toml="$1" queue leaf=" " f members
  queue=$(printf '%s' "$DEFAULT_FEATURES" | tr ',' ' ')
  while [ -n "$queue" ]; do
    f=${queue%% *}; queue=${queue#"$f"}; queue=${queue# }
    case "$f" in dep:* | */*) continue ;; esac
    if is_aggregate "$f"; then
      members=$(feature_members "$toml" "$f" | tr '\n' ' ')
      queue="$queue $members"
    else
      case "$leaf" in *" $f "*) ;; *) leaf="$leaf$f " ;; esac
    fi
  done
  printf '%s' "$leaf"
}

# ── App registry ──────────────────────────────────────────────────
#
# Apps are standalone binaries under `apps/<dir>` installed via
# `cargo install --path apps/<dir>` — they are NOT cargo features of the
# main binary. The installable set is discovered from `apps/*/Cargo.toml`
# so adding an app surfaces here without editing this script. `zerocode`
# (the TUI) is the default app. Tauri-based apps (e.g. zeroclaw-desktop)
# need the Tauri toolchain + system webview deps and are excluded from the
# simple `cargo install` path.
discover_apps() {
  APPS=""
  for dir in apps/*/; do
    [ -f "${dir}Cargo.toml" ] || continue
    name=$(awk -F'"' '/^name *=/{print $2; exit}' "${dir}Cargo.toml")
    [ -n "$name" ] || continue
    APPS="${APPS:+$APPS }$name"
  done
}

# Resolve the app directory for a given app/bin name.
app_dir_for() {
  for dir in apps/*/; do
    [ -f "${dir}Cargo.toml" ] || continue
    name=$(awk -F'"' '/^name *=/{print $2; exit}' "${dir}Cargo.toml")
    if [ "$name" = "$1" ]; then
      printf '%s' "${dir%/}"
      return 0
    fi
  done
  return 1
}

validate_app() {
  case " $APPS " in
  *" $1 "*) return 0 ;;
  *) die "Unknown app '$1'. Installable apps: $APPS" ;;
  esac
}

# ── Feature validation ────────────────────────────────────────────

validate_feature() {
  case "$1" in
  fantoccini)
    warn "'fantoccini' is deprecated — use 'browser-native'"
    return 0
    ;;
  landlock)
    warn "'landlock' is deprecated — use 'sandbox-landlock'"
    return 0
    ;;
  metrics)
    warn "'metrics' is deprecated — use 'observability-prometheus'"
    return 0
    ;;
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
    default | ci-all | fantoccini | landlock | metrics) continue ;;
    channel-*) channels="${channels:+$channels, }$feat" ;;
    observability-*) observability="${observability:+$observability, }$feat" ;;
    hardware | peripheral-* | sandbox-* | browser-* | probe | rag-pdf | webauthn)
      platform="${platform:+$platform, }$feat"
      ;;
    *) other="${other:+$other, }$feat" ;;
    esac
  done

  [ -n "$channels" ] && printf "  %s\n    %s\n\n" "$(bold "Channels:")" "$channels"
  [ -n "$observability" ] && printf "  %s\n    %s\n\n" "$(bold "Observability:")" "$observability"
  [ -n "$platform" ] && printf "  %s\n    %s\n\n" "$(bold "Platform:")" "$platform"
  [ -n "$other" ] && printf "  %s\n    %s\n\n" "$(bold "Other:")" "$other"

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
  zsh) echo "$HOME/.zshrc" ;;
  fish) echo "$HOME/.config/fish/config.fish" ;;
  *) echo "$HOME/.bashrc" ;;
  esac
}

shell_export_syntax() {
  local shell_name
  shell_name=$(basename "${SHELL:-/bin/bash}")
  case "$shell_name" in
  fish) printf 'set -gx PATH "%s/bin" $PATH' "$CARGO_HOME" ;;
  *) printf 'export PATH="%s/bin:$PATH"' "$CARGO_HOME" ;;
  esac
}

# ── Platform / target triple detection ───────────────────────────

detect_libc() {
  if [ -e /lib/ld-musl-*.so.1 ] 2>/dev/null || \
     ldd --version 2>&1 | grep -qi musl || \
     { [ -r /etc/os-release ] && grep -qiE 'alpine|postmarket' /etc/os-release; }; then
    echo "musl"
  else
    echo "gnu"
  fi
}

detect_target_triple() {
  local os arch libc
  os=$(uname -s)
  arch=$(uname -m)

  case "$os" in
  Darwin) echo "aarch64-apple-darwin" ;; # presume M-series
  Linux)
    libc=$(detect_libc)
    case "$arch" in
    x86_64) echo "x86_64-unknown-linux-${libc}" ;;
    aarch64 | arm64) echo "aarch64-unknown-linux-${libc}" ;;
    armv7l) echo "armv7-unknown-linux-gnueabihf" ;;
    armv6l | arm*) echo "arm-unknown-linux-gnueabihf" ;;
    *) echo "" ;;
    esac
    ;;
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
  version=$(curl -fsSL "https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest" |
    grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\(.*\)".*/\1/')

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
  web_data_dir=$(resolve_web_data_dir)

  if [ "$DRY_RUN" = true ]; then
    info "[dry-run] Would download $asset_url"
    info "[dry-run] Would install to $CARGO_HOME/bin/zeroclaw"
    info "[dry-run] Would install $TUI_BIN_NAME to $CARGO_HOME/bin/$TUI_BIN_NAME (if in tarball)"
    info "[dry-run] Would install web dashboard to $web_data_dir"
    return 0
  fi

  tmp_dir=$(mktemp -d)
  trap 'rm -rf "$tmp_dir"' EXIT

  curl -fSL --progress-bar "$asset_url" -o "$tmp_dir/$asset_name" ||
    {
      warn "Download failed — falling back to source build"
      rm -rf "$tmp_dir"
      return 1
    }

  # Verify checksum — all failure modes fall back to source rather than install unverified
  if ! curl -fsSL "$sha256_url" -o "$tmp_dir/SHA256SUMS" 2>/dev/null; then
    warn "Could not fetch SHA256SUMS — falling back to source build"
    rm -rf "$tmp_dir"
    return 1
  fi

  expected=$(grep "$asset_name" "$tmp_dir/SHA256SUMS" | awk '{print $1}')
  if [ -z "$expected" ]; then
    warn "Asset not found in SHA256SUMS — falling back to source build"
    rm -rf "$tmp_dir"
    return 1
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp_dir/$asset_name" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp_dir/$asset_name" | awk '{print $1}')
  else
    warn "No checksum tool available (sha256sum/shasum) — falling back to source build"
    rm -rf "$tmp_dir"
    return 1
  fi

  if [ "$actual" != "$expected" ]; then
    die "Checksum mismatch — download may be corrupt. Expected: $expected  Got: $actual"
  fi
  info "Checksum verified"

  tar -xzf "$tmp_dir/$asset_name" -C "$tmp_dir"
  mkdir -p "$CARGO_HOME/bin"
  install -m 755 "$tmp_dir/zeroclaw" "$CARGO_HOME/bin/zeroclaw"
  if [ -f "$tmp_dir/$TUI_BIN_NAME" ]; then
    install -m 755 "$tmp_dir/$TUI_BIN_NAME" "$CARGO_HOME/bin/$TUI_BIN_NAME"
  fi

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
  --apps X,Y           Select apps to install (e.g. zerocode); "none" to skip all
  --with-gateway       Force the gateway feature on (overrides preset/feature default)
  --without-gateway    Force the gateway feature off (overrides preset/feature default)
  --without-tui        Skip building the TUI ($TUI_BIN_NAME) [alias for --apps without it]
  --list-features      Print all available features and exit
  --prefix PATH        Install everything under PATH (default: \$HOME)
                       Sets CARGO_HOME, RUSTUP_HOME, source checkout, config
  --dry-run            Show what would happen without building or installing
  --skip-quickstart       Skip the post-install quickstart prompt
  --uninstall          Remove ZeroClaw binary and optionally config/data
  -h, --help           Show this help
  -V, --version        Show version from Cargo.toml

Examples:
  $0                                           # interactive: asks prebuilt or source
  $0 --prebuilt                                # download pre-built binary (fast)
  $0 --source                                  # always build from source
  $0 --source --minimal                        # smallest possible binary
  $0 --source --features agent-runtime,channel-discord  # custom feature set
  $0 --skip-quickstart                            # install only, configure later
  $0 --prefix /tmp/zc-test --skip-quickstart      # isolated test install
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

  local tui_bin="$CARGO_HOME/bin/$TUI_BIN_NAME"
  if [ -f "$tui_bin" ]; then
    rm -f "$tui_bin"
    info "Removed $tui_bin"
  fi

  local config_dir="$PREFIX/.zeroclaw"
  if [ -d "$config_dir" ]; then
    if [ -t 0 ]; then
      printf "  Remove config and data (%s)? [y/N] " "$config_dir"
      read -r confirm
      case "$confirm" in
      [Yy]*)
        rm -rf "$config_dir"
        info "Removed $config_dir"
        ;;
      *) info "Config preserved at $config_dir" ;;
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

# ── Quickstart-needed status check ───────────────────────────────
#
# Detect whether the operator already has a configured ZeroClaw so the
# 3-way "how would you like to complete setup?" prompt can skip silently
# on a re-install. We treat setup as complete when a config file exists
# at the expected path AND it contains at least one `[providers.models.*]`
# or `[providers.fallback]` line — i.e. some provider is configured.
# Empty or default config files still trigger the prompt.
quickstart_needed() {
  cfg="$PREFIX/.zeroclaw/config.toml"
  [ -f "$cfg" ] || return 0 # no config → run quickstart
  # Already-configured signal: any of these patterns means a provider was set.
  if grep -qE '^\[providers\.models\.|^fallback *=|^default_provider *=' "$cfg" 2>/dev/null; then
    return 1 # configured → skip
  fi
  return 0 # config exists but empty → run quickstart
}

# ── Interactive feature picker ───────────────────────────────────
#
# POSIX-sh number-toggle picker over the OPTIONAL feature set (channel-*,
# observability-*, hardware/peripheral/sandbox/browser flavours). Default
# features are always on; this only surfaces the opt-in extras. The output
# is a comma-separated list of selected features written to stdout.
#
# Invoked from the interactive flow when the operator runs install.sh in a
# TTY without `--minimal`, `--preset`, or `--features`. Skipped in
# non-interactive runs (curl | bash) and in CI.
interactive_feature_picker() {
  toml="$1"
  parse_cargo_toml "$toml"
  discover_apps

  # Split features into channels (channel-*) and everything else. Skip
  # aggregate/meta features (see $NON_ROW_FEATURES) — they are internal
  # groupings, not individual toggles. Defaults are pre-checked below.
  channel_features=""
  other_features=""
  for feat in $ALL_FEATURES; do
    if is_aggregate "$feat"; then continue; fi
    case "$feat" in
    channel-*)
      channel_features="${channel_features:+$channel_features }$feat"
      ;;
    *)
      other_features="${other_features:+$other_features }$feat"
      ;;
    esac
  done

  # Apps default-on set (zerocode); features pre-checked from the crate's
  # `default = [...]` list, expanded transitively so aggregate defaults like
  # `default-channels` pre-check their leaf channel-* rows.
  selected_apps="$DEFAULT_APPS"
  selected_features=$(expand_default_features "$toml")

  # Flat entry list, in display order: apps, then features, then channels.
  # Each entry is tagged "app:" or "feat:" so toggling routes to the right
  # selection set.
  entries=""
  for a in $APPS; do entries="${entries:+$entries }app:$a"; done
  for f in $other_features; do entries="${entries:+$entries }feat:$f"; done
  for c in $channel_features; do entries="${entries:+$entries }feat:$c"; done

  # Prompt-side output goes to stderr; the result is returned via globals.
  echo >&2
  printf "  %s\n" "$(bold "Select apps and optional features:")" >&2
  printf "  %s\n" "Type the numbers to toggle, blank line to confirm." >&2
  printf "  %s\n" "Checked (✓) items are on by default — uncheck to drop them." >&2
  echo >&2

  while :; do
    i=1
    last_section=""
    for entry in $entries; do
      kind=${entry%%:*}
      name=${entry#*:}
      # Section header when the group changes.
      section=""
      case "$kind" in
      app) section="Apps (--apps)" ;;
      feat) case "$name" in channel-*) section="Channels (--features)" ;; *) section="Features (--features)" ;; esac ;;
      esac
      if [ "$section" != "$last_section" ]; then
        [ -n "$last_section" ] && echo >&2
        printf "  %s\n" "$(bold "$section:")" >&2
        last_section="$section"
      fi
      mark=" "
      case "$kind" in
      app) case " $selected_apps " in *" $name "*) mark="✓" ;; esac ;;
      feat) case " $selected_features " in *" $name "*) mark="✓" ;; esac ;;
      esac
      printf "    [%2d] %s %s\n" "$i" "$mark" "$name" >&2
      i=$((i + 1))
    done
    echo >&2
    printf "  toggle (e.g. \"1 3 5\"), %s confirm: " "$(bold "Enter to")" >&2
    read -r choices
    [ -z "$choices" ] && break
    for n in $choices; do
      case "$n" in
      '' | *[!0-9]*) continue ;;
      esac
      idx=1
      for entry in $entries; do
        if [ "$idx" -eq "$n" ]; then
          kind=${entry%%:*}
          name=${entry#*:}
          if [ "$kind" = app ]; then
            case " $selected_apps " in
            *" $name "*) selected_apps=$(printf '%s' "$selected_apps" | tr ' ' '\n' | grep -vx "$name" | paste -sd' ' -) ;;
            *) selected_apps="${selected_apps:+$selected_apps }$name" ;;
            esac
          else
            case " $selected_features " in
            *" $name "*) selected_features=$(printf '%s' "$selected_features" | tr ' ' '\n' | grep -vx "$name" | paste -sd' ' -) ;;
            *) selected_features="${selected_features:+$selected_features }$name" ;;
            esac
          fi
          break
        fi
        idx=$((idx + 1))
      done
    done
  done

  PICKED_FEATURES=$(printf '%s' "$selected_features" | tr ' ' ',')
  PICKED_APPS=$(printf '%s' "$selected_apps" | tr ' ' ',')
}

# Resolve the platform data directory the gateway auto-detects for the
# dashboard bundle. Single source of truth so the prebuilt and source
# install paths cannot drift.
resolve_web_data_dir() {
  case "$(uname -s)" in
  Darwin)
    printf '%s' "${HOME}/Library/Application Support/zeroclaw/web/dist"
    ;;
  MINGW* | CYGWIN* | MSYS*)
    printf '%s' "${LOCALAPPDATA}/zeroclaw/web/dist"
    ;;
  *)
    printf '%s' "${XDG_DATA_HOME:-${PREFIX}/.local/share}/zeroclaw/web/dist"
    ;;
  esac
}

# Copy a built web/dist into the gateway data directory so a
# systemd-launched daemon finds it regardless of CWD.
install_web_dist() {
  src_dist="$1"
  if [ ! -f "$src_dist/index.html" ]; then
    return 0
  fi
  web_data_dir=$(resolve_web_data_dir)
  if [ "$DRY_RUN" = true ]; then
    info "[dry-run] Would install web dashboard to $web_data_dir"
    return 0
  fi
  mkdir -p "$web_data_dir"
  cp -r "$src_dist/." "$web_data_dir/"
  info "Web dashboard installed to $web_data_dir"
}

# ── Web dashboard build for source installs ──────────────────────
#
# When a source build includes the `gateway` feature, the dashboard
# (`web/dist`) needs to be built so the gateway can serve it. If Node.js
# is on PATH we run `cargo web build` from the source root so the
# generated API client is refreshed before TypeScript compiles, then the
# built bundle is copied into the gateway data directory. Without Node.js
# we warn and tell the user to re-run the installer once Node.js is
# present — never to run cargo by hand.
build_web_dashboard() {
  src_dir="$1"
  if [ ! -d "$src_dir/web" ]; then
    warn "Source has no web/ directory; skipping dashboard build."
    return 0
  fi
  if ! command -v npm >/dev/null 2>&1; then
    warn "npm not found — skipping dashboard build. The gateway will run"
    warn "  in API-only mode. Install Node.js (npm) and re-run ./install.sh"
    warn "  --source to build and install the dashboard."
    return 0
  fi
  # Always rebuild — a stale dist from a prior revision serves outdated
  # assets against an updated gateway. Incremental caching keeps no-op
  # re-runs cheap.
  info "Building web dashboard (cargo web build)..."
  (cd "$src_dir" && cargo web build) || {
    warn "Dashboard build failed — gateway will run in API-only mode."
    return 0
  }
  info "Web dashboard built at $src_dir/web/dist"
  install_web_dist "$src_dir/web/dist"
}

# ── Low-memory build heuristic ────────────────────────────────────
#
# [profile.release] in Cargo.toml uses fat LTO + codegen-units = 1.
# With heavy crates in the graph (matrix-sdk-crypto, ruma, vodozemac)
# a single rustc process can peak past 7 GB RSS during the cross-crate
# type pass, OOM-ing 8 GB ARM devices. Thin LTO trades a small
# binary-size hit for a much lower build-time RAM peak. Apply it as
# a default on Linux hosts with under ~12 GiB MemTotal, but only when
# the user has not already pinned CARGO_PROFILE_RELEASE_LTO.
apply_low_mem_lto_default() {
  [ "$(uname -s)" = "Linux" ] || return 0
  [ -r /proc/meminfo ] || return 0
  [ -n "${CARGO_PROFILE_RELEASE_LTO:-}" ] && return 0

  mem_kb=$(awk '/^MemTotal:/{print $2; exit}' /proc/meminfo 2>/dev/null)
  case "$mem_kb" in
  '' | *[!0-9]*) return 0 ;;
  esac
  # 12 GiB in KiB = 12 * 1024 * 1024
  if [ "$mem_kb" -lt 12582912 ]; then
    mem_gib=$((mem_kb / 1048576))
    export CARGO_PROFILE_RELEASE_LTO=thin
    info "Low-memory device detected (${mem_gib} GiB RAM): using thin LTO to keep build RAM bounded. Set CARGO_PROFILE_RELEASE_LTO=fat to override."
  fi
}

# ── Parse arguments ───────────────────────────────────────────────

MINIMAL=false
USER_FEATURES=""
SKIP_QUICKSTART=false
LIST_FEATURES=false
UNINSTALL=false
DRY_RUN=false
PREFIX="$HOME"
INSTALL_MODE="" # ""=ask, "prebuilt"=force prebuilt, "source"=force source
PRESET=""       # ""=unset, "minimal"=alias for --minimal, "full"=default-features
WITH_GATEWAY="" # ""=unset (preset/feature default applies), "true"/"false"=explicit toggle
WITHOUT_TUI=""  # ""=unset (default: install TUI), "true"=skip TUI
USER_APPS=""    # ""=unset (default apps), "none"=skip all, or comma list (e.g. "zerocode")

# Support legacy env var
if [ -n "${ZEROCLAW_CARGO_FEATURES:-}" ]; then
  USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$ZEROCLAW_CARGO_FEATURES"
fi

while [ $# -gt 0 ]; do
  case "$1" in
  --minimal) MINIMAL=true ;;
  --preset)
    if [ $# -lt 2 ]; then
      die "Missing value for --preset. Expected: --preset minimal|full"
    fi
    shift
    case "$1" in
    minimal)
      PRESET="minimal"
      MINIMAL=true
      ;;
    full) PRESET="full" ;;
    *) die "Unknown preset '$1'. Expected: minimal or full" ;;
    esac
    ;;
  --features)
    if [ $# -lt 2 ]; then
      die "Missing value for --features. Expected: --features X,Y"
    fi
    shift
    USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$1"
    ;;
  --apps)
    if [ $# -lt 2 ]; then
      die "Missing value for --apps. Expected: --apps zerocode[,...] or --apps none"
    fi
    shift
    USER_APPS="${USER_APPS:+$USER_APPS,}$1"
    ;;
  --with-gateway) WITH_GATEWAY="true" ;;
  --without-gateway) WITH_GATEWAY="false" ;;
  --without-tui) WITHOUT_TUI=true ;;
  --list-features) LIST_FEATURES=true ;;
  --prefix)
    if [ $# -lt 2 ]; then
      die "Missing value for --prefix. Expected: --prefix /path"
    fi
    shift
    PREFIX=$(echo "$1" | sed 's|/*$||')
    ;;
  --dry-run) DRY_RUN=true ;;
  --skip-quickstart) SKIP_QUICKSTART=true ;;
  --prebuilt) INSTALL_MODE="prebuilt" ;;
  --source) INSTALL_MODE="source" ;;
  --uninstall) UNINSTALL=true ;;
  -h | --help)
    usage
    exit 0
    ;;
  -V | --version)
    if [ -f "Cargo.toml" ]; then
      parse_cargo_toml "Cargo.toml"
      echo "install.sh for ZeroClaw v$VERSION"
    else
      echo "install.sh (version unknown — not in repo)"
    fi
    exit 0
    ;;
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

# --minimal, --features, --apps, --without-gateway, or --preset full imply
# source. Prebuilt binaries always ship with default features and no apps,
# so any flag that changes the feature set or selects apps must force a
# source build.
if [ "$MINIMAL" = true ] || [ -n "$USER_FEATURES" ] || [ -n "$USER_APPS" ] ||
  [ "$WITH_GATEWAY" = "false" ] || [ "$PRESET" = "full" ]; then
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
      read -r install_choice
      case "$install_choice" in
      [Ss]*) INSTALL_MODE="source" ;;
      *) INSTALL_MODE="prebuilt" ;;
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
  TUI_BIN="$CARGO_HOME/bin/$TUI_BIN_NAME"
  if [ -f "$TUI_BIN" ]; then
    TUI_SIZE=$(du -h "$TUI_BIN" | awk '{print $1}')
    info "Installed: $TUI_BIN ($TUI_SIZE)"
  fi
}

# ── Locate source ─────────────────────────────────────────────────

[ "${PREBUILT_OK:-false}" = true ] && {
  # Jump past the source build to PATH + quickstart
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
  armv7l | armv6l | armhf)
    die "32-bit ARM detected — the default feature 'observability-prometheus'
requires 64-bit atomics and will not compile on this architecture.

Example (full agent without prometheus):
  $0 --minimal --features agent-runtime,schema-export

See all available features:
  $0 --list-features"
    ;;
  esac

  # ── Build feature flags ──────────────────────────────────────────
  #
  # Cargo cannot remove individual entries from `default`, so toggling
  # `gateway` off requires `--no-default-features` plus an explicit list
  # of the rest. Derive that list from $DEFAULT_FEATURES (parsed from
  # Cargo.toml above) so it stays in sync automatically.

  CARGO_FLAGS=""

  if [ "$MINIMAL" = true ]; then
    CARGO_FLAGS="--no-default-features"
  fi

  # `--without-gateway` overrides the default-features set: switch to
  # --no-default-features and re-add everything in `default` except gateway.
  if [ "$WITH_GATEWAY" = "false" ] && [ "$MINIMAL" != true ]; then
    CARGO_FLAGS="--no-default-features"
    defaults_no_gateway=$(printf '%s' "$DEFAULT_FEATURES" | tr ',' '\n' | grep -vx gateway | paste -sd, -)
    USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}$defaults_no_gateway"
  fi

  # `--with-gateway` is a no-op when default features are on (gateway is
  # already there), and additive when --no-default-features is in play.
  if [ "$WITH_GATEWAY" = "true" ]; then
    case "$CARGO_FLAGS" in
    *--no-default-features*) USER_FEATURES="${USER_FEATURES:+$USER_FEATURES,}gateway" ;;
    esac
  fi

  # Interactive picker — only when the operator did not pin features or
  # apps via the CLI and is running under a TTY. Skipped on `--minimal`,
  # `--preset`, `--features`, `--apps`, `--with-gateway` /
  # `--without-gateway`, and any non-interactive run (curl | bash).
  if [ -t 0 ] &&
    [ "$MINIMAL" != true ] &&
    [ -z "$USER_FEATURES" ] &&
    [ -z "$USER_APPS" ] &&
    [ -z "$PRESET" ] &&
    [ -z "$WITH_GATEWAY" ]; then
    discover_apps
    interactive_feature_picker "Cargo.toml"
    # The picker pre-checks the crate defaults and lets the operator add or
    # remove any of them, so its result is the authoritative, complete
    # feature set — build with --no-default-features and exactly what was
    # checked. This makes unchecking a default (e.g. gateway) actually drop
    # it instead of silently leaving the default applied.
    CARGO_FLAGS="--no-default-features"
    USER_FEATURES="$PICKED_FEATURES"
    info "Picked features: ${USER_FEATURES:-<none>}"
    # Picker always resolves the app set explicitly (selected or none).
    USER_APPS="${PICKED_APPS:-none}"
    info "Picked apps: $USER_APPS"
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
        read -r confirm
        case "$confirm" in
        [Nn]*)
          echo "Aborted."
          exit 0
          ;;
        esac
      fi
    fi
    if [ "$PRESET" = "full" ] && [ "$DRY_RUN" != true ] && [ -t 1 ]; then
      info "--preset full: building from source with the full default feature set."
    fi
  fi

  # ── Build profile RAM heuristic (Linux low-mem hosts) ─────────────

  apply_low_mem_lto_default

  # ── Build and install ─────────────────────────────────────────────

  echo
  printf "%s\n" "$(bold "Building ZeroClaw v$VERSION")"
  if [ -n "$CARGO_FLAGS" ]; then
    info "Feature flags: $CARGO_FLAGS"
  else
    info "Feature flags: (defaults)"
  fi
  echo

  # >>> generated:source-cargo-install by `cargo generate installers` - do not edit <<<
  if [ "$DRY_RUN" = true ]; then
    # shellcheck disable=SC2086
    info "[dry-run] Would run: cargo install --path . --locked --force $CARGO_FLAGS"
  else
    # shellcheck disable=SC2086
    cargo install --path . --locked --force $CARGO_FLAGS
  fi
  # >>> end generated:source-cargo-install <<<

  # ── Web dashboard (gateway feature only) ──────────────────────────
  # When the install includes the `gateway` feature, build `web/dist` so
  # the dashboard route serves something. Skips silently when the build
  # excluded gateway (`--without-gateway`, `--minimal` without explicit
  # gateway in --features, etc).
  WANT_GATEWAY=true
  case "$CARGO_FLAGS" in
  *--no-default-features*)
    case ",$USER_FEATURES," in
    *,gateway,*) ;;
    *) WANT_GATEWAY=false ;;
    esac
    ;;
  esac
  if [ "$WANT_GATEWAY" = true ]; then
    if [ "$DRY_RUN" = true ]; then
      info "[dry-run] Would build web dashboard"
    else
      build_web_dashboard "$INSTALL_DIR"
    fi
  fi

  # ── Apps (standalone binaries under apps/<dir>) ──────────────────
  # Apps connect to zeroclaw-runtime's RPC server, so they need the
  # agent-runtime feature. Without it there's no daemon — skip apps.
  discover_apps

  # Resolve the app set: explicit --apps list, "none" to skip, or the
  # full installable set by default. --without-tui is back-compat for
  # dropping the TUI app from the default set.
  if [ "$USER_APPS" = "none" ]; then
    WANT_APPS=""
  elif [ -n "$USER_APPS" ]; then
    WANT_APPS=$(printf '%s' "$USER_APPS" | tr ',[:space:]' '\n' | grep -v '^$' | sort -u | paste -sd' ' -)
    for app in $WANT_APPS; do validate_app "$app"; done
  else
    WANT_APPS="$DEFAULT_APPS"
    if [ "$WITHOUT_TUI" = true ]; then
      WANT_APPS=$(printf '%s' "$WANT_APPS" | tr ' ' '\n' | grep -vx "$TUI_BIN_NAME" | paste -sd' ' -)
    fi
  fi

  # agent-runtime is a default feature; if defaults are stripped and it
  # wasn't re-added, no daemon exists to back the apps.
  case "$CARGO_FLAGS" in
  *--no-default-features*)
    case ",$USER_FEATURES," in
    *,agent-runtime,*) ;;
    *) WANT_APPS="" ;;
    esac
    ;;
  esac

  for app in $WANT_APPS; do
    app_path=$(app_dir_for "$app") || continue
    if [ "$DRY_RUN" = true ]; then
      info "[dry-run] Would run: cargo install --path $app_path --locked --force"
    else
      echo
      printf "%s\n" "$(bold "Building $app")"
      echo
      cargo install --path "$app_path" --locked --force
    fi
  done

  # ── Summary ───────────────────────────────────────────────────────

  if [ "$DRY_RUN" != true ]; then
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
    TUI_BIN="$CARGO_HOME/bin/$TUI_BIN_NAME"
    if [ -f "$TUI_BIN" ]; then
      TUI_SIZE=$(du -h "$TUI_BIN" | awk '{print $1}')
      info "Installed: $TUI_BIN ($TUI_SIZE)"
    fi
  fi

fi # end source build block

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

# ── Quickstart prompt ─────────────────────────────────────────────

if [ "$SKIP_QUICKSTART" = false ] && [ "$DRY_RUN" != true ] && [ -f "$BIN" ]; then
  # Skip the prompt entirely when the operator already has a configured
  # ZeroClaw — re-installs should not re-prompt.
  if ! quickstart_needed; then
    info "Existing ZeroClaw config detected at $PREFIX/.zeroclaw/config.toml — skipping setup prompt."
    info "Run 'zeroclaw quickstart' to reconfigure."
  elif [ -t 0 ]; then
    # 3-way setup choice. Bare Enter accepts the [1] CLI quickstart default;
    # option [2] foregrounds the daemon so the operator can finish in the
    # browser and Ctrl+C to return; [3] skips and prints a follow-up hint.
    # Non-TTY runs fall through to the silent skip in the else branch.
    echo
    printf "%s\n" "$(bold "ZeroClaw installed. How would you like to complete setup?")"
    printf "  [1] CLI quickstart  (zeroclaw quickstart)\n"
    printf "  [2] Open gateway in browser (zeroclaw daemon + dashboard)\n"
    printf "  [3] Skip for now\n"
    printf "  Choice [1-3, default 1]: "
    read -r quickstart_choice
    case "${quickstart_choice:-1}" in
    1 | "")
      echo
      "$BIN" quickstart || warn "Quickstart exited with an error — run 'zeroclaw quickstart' manually"
      ;;
    2)
      echo
      info "Starting gateway daemon for browser-based setup..."
      info "Open the dashboard in your browser; pair with the code shown in logs."
      info "Stop the daemon with Ctrl+C when done; then run 'zeroclaw service install' for always-on."
      "$BIN" daemon || warn "Daemon exited with an error — run 'zeroclaw daemon' manually"
      ;;
    3)
      info "Skipped setup. Run 'zeroclaw quickstart' (CLI) or 'zeroclaw daemon' (browser) when ready."
      ;;
    *)
      warn "Unknown choice '$quickstart_choice' — skipping. Run 'zeroclaw quickstart' to configure."
      ;;
    esac
  else
    info "Non-interactive — skipping setup prompt. Run 'zeroclaw quickstart' to configure."
  fi
fi

echo
# Next-step hint, smartest-first: if zerocode (the TUI) was installed, that's
# the best place to start; otherwise point at the daemon + web dashboard, then
# fall back to a one-off CLI agent run.
if [ -f "$CARGO_HOME/bin/$TUI_BIN_NAME" ]; then
  info "Done. Run $(bold "$TUI_BIN_NAME") to launch the terminal UI and start working."
elif [ -f "$CARGO_HOME/bin/zeroclaw" ] && "$CARGO_HOME/bin/zeroclaw" --help 2>/dev/null | grep -q '\bdaemon\b'; then
  info "Done. Run $(bold "zeroclaw daemon") for the always-on daemon + web dashboard,"
  info "or $(bold "zeroclaw agent") for a one-off CLI chat."
else
  info "Done. Run $(bold "zeroclaw agent") to start chatting."
fi
echo
