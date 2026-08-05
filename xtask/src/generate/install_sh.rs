//! install.sh renderer. Hand-authored helpers surround generated route-control
//! zones so the published invocation, selection, fallback, Rust, app, PATH,
//! and Quickstart contracts drive their executable shell boundaries.

use super::spec::{
    self, AppPolicy, BranchPolicy, FeaturePolicy, InstallRoute, InstallVariant, Interaction,
    Invocation, PathEffect, Platform, QuickstartHandoff, RouteId, RustPolicy, Selection,
};
use std::path::Path;

fn begin(zone: &str) -> String {
    format!("  # >>> generated:{zone} by `cargo generate installers` - do not edit <<<")
}
fn end(zone: &str) -> String {
    format!("  # >>> end generated:{zone} <<<")
}

const ZONE_ROUTE_CONTRACT: &str = "route-contract";
const ZONE_ROUTE_FLAGS: &str = "route-flags";
const ZONE_ROUTE_DECISION: &str = "route-decision";
const ZONE_SOURCE_DISPATCH_OPEN: &str = "source-dispatch-open";
const ZONE_RUST_BOOTSTRAP: &str = "rust-bootstrap";
const ZONE_FEATURE_PICKER_HELPER: &str = "feature-picker-helper";
const ZONE_FEATURE_PICKER: &str = "feature-picker";
const ZONE_APP_SELECTION: &str = "app-selection";
const ZONE_CARGO_INSTALL: &str = "source-cargo-install";
const ZONE_SOURCE_DISPATCH_CLOSE: &str = "source-dispatch-close";
const ZONE_PATH_HANDOFF: &str = "path-handoff";
const ZONE_QUICKSTART_HANDOFF: &str = "quickstart-handoff";
#[derive(Clone, Copy)]
struct ZoneLayout {
    zone: &'static str,
    range_start: &'static str,
    range_end: &'static str,
}

const ZONE_LAYOUTS: [ZoneLayout; 12] = [
    ZoneLayout {
        zone: ZONE_ROUTE_CONTRACT,
        range_start: "bold() { printf \"${BOLD}%s${RESET}\" \"$*\"; }",
        range_end: "# ── Parse Cargo.toml (source of truth) ────────────────────────────",
    },
    ZoneLayout {
        zone: ZONE_FEATURE_PICKER_HELPER,
        range_start: "# ── Interactive feature picker ───────────────────────────────────",
        range_end: "# Resolve the platform data directory the gateway auto-detects for the",
    },
    ZoneLayout {
        zone: ZONE_ROUTE_FLAGS,
        range_start: "  --skip-quickstart) SKIP_QUICKSTART=true ;;",
        range_end: "  --uninstall) UNINSTALL=true ;;",
    },
    ZoneLayout {
        zone: ZONE_ROUTE_DECISION,
        range_start: "# ── Decide: pre-built or source ───────────────────────────────────",
        range_end: "[ \"${PREBUILT_OK:-false}\" = true ] && [ \"$DRY_RUN\" != true ] && {",
    },
    ZoneLayout {
        zone: ZONE_SOURCE_DISPATCH_OPEN,
        range_start: "# ── Locate source ─────────────────────────────────────────────────",
        range_end: "  printf \"%s\\n\" \"$(bold \"ZeroClaw — source install\")\"",
    },
    ZoneLayout {
        zone: ZONE_RUST_BOOTSTRAP,
        range_start: "  # ── Preflight: Rust ───────────────────────────────────────────────",
        range_end: "  # ── Preflight: 32-bit ARM ────────────────────────────────────────",
    },
    ZoneLayout {
        zone: ZONE_FEATURE_PICKER,
        range_start: "  # Interactive picker — only when the operator did not pin features or",
        range_end: "  # ── Detect existing installs ──────────────────────────────────────",
    },
    ZoneLayout {
        zone: ZONE_CARGO_INSTALL,
        range_start: "      build_web_dashboard \"$INSTALL_DIR\" true\n    fi\n  fi",
        range_end: "  # ── Web dashboard (gateway feature only) ──────────────────────────",
    },
    ZoneLayout {
        zone: ZONE_APP_SELECTION,
        range_start: "  # ── Apps (standalone binaries under apps/<dir>) ──────────────────",
        range_end: "  # ── Summary ───────────────────────────────────────────────────────",
    },
    ZoneLayout {
        zone: ZONE_SOURCE_DISPATCH_CLOSE,
        range_start: "  # ── Summary ───────────────────────────────────────────────────────",
        range_end: "BIN=\"$CARGO_HOME/bin/zeroclaw\"\n\n# ── PATH setup",
    },
    ZoneLayout {
        zone: ZONE_PATH_HANDOFF,
        range_start: "# ── PATH setup ────────────────────────────────────────────────────",
        range_end: "# ── Quickstart prompt ─────────────────────────────────────────────",
    },
    ZoneLayout {
        zone: ZONE_QUICKSTART_HANDOFF,
        range_start: "# ── Quickstart prompt ─────────────────────────────────────────────",
        range_end: "# Next-step hint",
    },
];

fn route_by_id(routes: &[InstallRoute], id: RouteId) -> anyhow::Result<&InstallRoute> {
    routes
        .iter()
        .find(|route| route.id == id)
        .ok_or_else(|| anyhow::Error::msg(format!("missing canonical install route {id:?}")))
}

fn unix_variant(route: &InstallRoute) -> anyhow::Result<&InstallVariant> {
    route.variant(Platform::Unix)
}

fn ensure_once(text: &str, needle: &str, label: &str) -> anyhow::Result<()> {
    let count = text.match_indices(needle).count();
    anyhow::ensure!(
        count == 1,
        "{label} must appear exactly once in install.sh (found {count})"
    );
    Ok(())
}

fn render_route_contract(routes: &[InstallRoute]) -> anyhow::Result<String> {
    let piped_route = route_by_id(routes, RouteId::UnixFast)?;
    let piped = unix_variant(piped_route)?;
    anyhow::ensure!(
        piped.branch == BranchPolicy::PreferPrebuiltThenSource
            && piped.handoff == QuickstartHandoff::PrintCommand
            && piped.invocation == Invocation::PipedScript
            && piped.interaction == Interaction::NonInteractive
            && piped.features == FeaturePolicy::Fixed
            && piped.rust == RustPolicy::BootstrapIfMissing,
        "UnixFast route must be a noninteractive piped install with fixed choices"
    );
    let piped_defaults = match piped.apps {
        AppPolicy::ArchiveOptionalOrSourceDefaults(apps) => apps,
        _ => anyhow::bail!("UnixFast route must provide source-fallback app defaults"),
    };

    let guided_route = route_by_id(routes, RouteId::UnixGuided)?;
    let guided = unix_variant(guided_route)?;
    anyhow::ensure!(
        guided.branch == BranchPolicy::UserChoice
            && guided.handoff == QuickstartHandoff::OfferCliOrBrowser
            && guided.invocation == Invocation::InteractiveScript
            && guided.interaction == Interaction::Guided
            && guided.features == FeaturePolicy::Selectable
            && guided.rust == RustPolicy::BootstrapIfMissing,
        "UnixGuided route must be an interactive script with selectable choices"
    );
    let guided_defaults = match guided.apps {
        AppPolicy::ArchiveOptionalOrSourceDefaultsSelectable(apps) => apps,
        _ => anyhow::bail!("UnixGuided route must provide source-choice app defaults"),
    };

    for (id, variant) in [(piped_route.id, piped), (guided_route.id, guided)] {
        anyhow::ensure!(
            variant.path_effect == PathEffect::ProfileUpdateRequiresReload,
            "Unix install route {:?} must require a profile reload",
            id
        );
    }

    let source_route = route_by_id(routes, RouteId::AdvancedSource)?;
    let source = unix_variant(source_route)?;
    anyhow::ensure!(
        source.branch == BranchPolicy::SourceOnly
            && source.invocation == Invocation::SourceScript
            && source.interaction == Interaction::Guided
            && source.features == FeaturePolicy::Selectable
            && source.handoff == QuickstartHandoff::OfferCliOrBrowser
            && source.rust == RustPolicy::BootstrapIfMissing
            && source.path_effect == PathEffect::ProfileUpdateRequiresReload,
        "AdvancedSource Unix route must match install.sh source behavior"
    );
    let source_defaults = match source.apps {
        AppPolicy::SourceDefaultsSelectable(apps) => apps,
        _ => anyhow::bail!("AdvancedSource route must provide selectable source app defaults"),
    };
    anyhow::ensure!(
        piped_defaults == guided_defaults && guided_defaults == source_defaults,
        "Unix source-capable routes must agree on default apps"
    );
    anyhow::ensure!(
        piped.quickstart_command == guided.quickstart_command
            && guided.quickstart_command == source.quickstart_command,
        "Unix install routes must agree on the Quickstart command"
    );
    let mut quickstart_parts = guided.quickstart_command.split_whitespace();
    anyhow::ensure!(
        quickstart_parts.next() == Some("zeroclaw"),
        "guided Quickstart command must invoke zeroclaw"
    );
    let quickstart_subcommand = quickstart_parts
        .next()
        .ok_or_else(|| anyhow::Error::msg("guided Quickstart command must have a subcommand"))?;
    anyhow::ensure!(
        quickstart_parts.next().is_none(),
        "guided Quickstart execution cannot safely split command arguments"
    );
    anyhow::ensure!(
        source_defaults.len() == 1,
        "Unix installer requires exactly one canonical TUI app"
    );
    let default_apps = source_defaults.join(" ");

    Ok([
        format!("TUI_BIN_NAME=\"{}\"", source_defaults[0]),
        format!("DEFAULT_APPS=\"{default_apps}\""),
        "PIPED_INSTALL_MODE=\"prebuilt\"".to_owned(),
        format!("QUICKSTART_COMMAND=\"{}\"", guided.quickstart_command),
        format!("QUICKSTART_SUBCOMMAND=\"{quickstart_subcommand}\""),
        "GUIDED_INSTALL_MODE=\"choice\"".to_owned(),
        "GUIDED_QUICKSTART_MODE=\"offer\"".to_owned(),
        "UNIX_PATH_RELOAD=\"true\"".to_owned(),
    ]
    .join("\n"))
}

fn render_route_flags() -> &'static str {
    r#"  --prebuilt) INSTALL_MODE="prebuilt" ;;
  --source) INSTALL_MODE="source" ;;"#
}

fn render_route_decision() -> &'static str {
    r#"# --minimal, --features, --apps, --without-gateway, or --preset full imply
# source. Prebuilt archives have fixed feature and app contents, so any flag
# that changes the feature set or selects apps must force a source build.
if [ "$MINIMAL" = true ] || [ -n "$USER_FEATURES" ] || [ -n "$USER_APPS" ] ||
  [ "$WITH_GATEWAY" = "false" ] || [ "$PRESET" = "full" ]; then
  INSTALL_MODE="source"
fi

if [ "$INSTALL_MODE" = "" ]; then
  triple=$(detect_target_triple)
  if [ -n "$triple" ]; then
    if [ "$GUIDED_INSTALL_MODE" = "choice" ] && [ -t 0 ]; then
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
      INSTALL_MODE="$PIPED_INSTALL_MODE"
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
fi"#
}

fn render_source_dispatch_open() -> &'static str {
    r#"if [ "${PREBUILT_OK:-false}" != true ]; then"#
}

fn render_rust_bootstrap() -> &'static str {
    r#"  NEED_RUST=false
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
  fi"#
}

fn render_feature_picker_helper() -> &'static str {
    r#"# Offers every non-aggregate feature and installable app. Defaults start
# checked and can be toggled off. Prompts are written to stderr; the selected
# values are returned through PICKED_FEATURES and PICKED_APPS. The caller only
# invokes this helper for an interactive TTY, never for a piped `curl | sh` run.
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
}"#
}

fn render_feature_picker() -> &'static str {
    r#"  if [ -t 0 ] &&
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
  fi"#
}

fn render_app_selection() -> &'static str {
    r#"  if [ "$FULL_APPS" = true ] && [ -z "$USER_APPS" ]; then
    # --full installs every discovered app (an explicit --apps still wins) —
    # except Tauri-based apps (tauri.conf.json present): they need the Tauri
    # toolchain + system webview deps (webkit2gtk/GTK on Linux), which most
    # machines don't have. Request them explicitly via --apps to opt in.
    WANT_APPS=""
    for app in $APPS; do
      app_path=$(app_dir_for "$app") || continue
      if [ -f "$app_path/tauri.conf.json" ]; then
        info "Skipping $app: Tauri app needs system webview deps — install explicitly with --apps $app"
        continue
      fi
      WANT_APPS="${WANT_APPS:+$WANT_APPS }$app"
    done
  elif [ "$USER_APPS" = "none" ]; then
    WANT_APPS=""
  elif [ -n "$USER_APPS" ]; then
    WANT_APPS=$(printf '%s' "$USER_APPS" | tr ',[:space:]' '\n' | grep -v '^$' | sort -u | paste -sd' ' -)
    for app in $WANT_APPS; do validate_app "$app"; done
  else
    WANT_APPS="$DEFAULT_APPS"
  fi

  # --without-tui drops the TUI app from the resolved default or --full
  # set (an explicit --apps list is honored as-is).
  if [ "$WITHOUT_TUI" = true ] && [ -z "$USER_APPS" ]; then
    WANT_APPS=$(printf '%s' "$WANT_APPS" | tr ' ' '\n' | grep -vx "$TUI_BIN_NAME" | paste -sd' ' -)
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
  done"#
}

fn render_source_dispatch_close() -> &'static str {
    "fi # end source build block"
}

fn render_path_handoff() -> &'static str {
    r#"PROFILE=$(detect_shell_profile)
EXPORT_LINE=$(shell_export_syntax)

# Is our bin dir already on PATH via the profile — either pre-existing or
# from a prior run of this installer? If so there's nothing to do.
PATH_ALREADY_SET=false
if [ -f "$PROFILE" ] && grep -q "$CARGO_HOME/bin" "$PROFILE" 2>/dev/null; then
  PATH_ALREADY_SET=true
fi

print_path_help() {
  echo
  printf "  %s (%s):\n" "$(bold "Add to your shell profile")" "$PROFILE"
  echo
  printf "    %s\n" "$EXPORT_LINE"
  echo
  printf "  Then reload:\n"
  echo
  printf "    source %s\n" "$PROFILE"
  echo
}

if [ "$PATH_ALREADY_SET" = true ]; then
  : # already on PATH — nothing to do
elif [ "$MODIFY_PATH" = true ] && [ "$PREFIX" = "$HOME" ]; then
  # Auto-append to the profile, wrapped in a marker block so re-installs
  # stay idempotent and an uninstall can strip it cleanly.
  if [ "$DRY_RUN" = true ]; then
    info "[dry-run] Would add $CARGO_HOME/bin to PATH in $PROFILE"
  elif {
    printf '\n# >>> zeroclaw >>>\n'
    printf '%s\n' "$EXPORT_LINE"
    printf '# <<< zeroclaw <<<\n'
  } >>"$PROFILE" 2>/dev/null; then
    info "Added $CARGO_HOME/bin to PATH in $PROFILE"
    if [ "$UNIX_PATH_RELOAD" = true ]; then
      printf "    Reload your shell or run: source %s\n" "$PROFILE"
    fi
  else
    warn "Could not write to $PROFILE — add this line manually:"
    print_path_help
  fi
else
  # --no-modify-path, or a custom --prefix install we won't auto-edit for.
  print_path_help
fi"#
}

fn render_quickstart_handoff() -> &'static str {
    r#"if [ "$SKIP_QUICKSTART" = false ] && [ "$DRY_RUN" != true ] && [ -f "$BIN" ]; then
  # Skip the prompt entirely when the operator already has a configured
  # ZeroClaw — re-installs should not re-prompt.
  if ! quickstart_needed; then
    info "Existing ZeroClaw config detected at $PREFIX/.zeroclaw/config.toml — skipping setup prompt."
    info "Run '$QUICKSTART_COMMAND' to reconfigure."
  elif [ "$GUIDED_QUICKSTART_MODE" = "offer" ] && [ -t 0 ]; then
    # 3-way setup choice. Bare Enter accepts the [1] CLI quickstart default;
    # option [2] foregrounds the daemon so the operator can finish in the
    # browser and Ctrl+C to return; [3] skips and prints a follow-up hint.
    # Non-TTY runs fall through to the silent skip in the else branch.
    echo
    printf "%s\n" "$(bold "ZeroClaw installed. How would you like to complete setup?")"
    printf "  [1] CLI quickstart  ($QUICKSTART_COMMAND)\n"
    printf "  [2] Open gateway in browser (zeroclaw daemon + dashboard)\n"
    printf "  [3] Skip for now\n"
    printf "  Choice [1-3, default 1]: "
    read -r quickstart_choice
    case "${quickstart_choice:-1}" in
    1 | "")
      echo
      "$BIN" "$QUICKSTART_SUBCOMMAND" || warn "Quickstart exited with an error — run '$QUICKSTART_COMMAND' manually"
      ;;
    2)
      echo
      info "Starting gateway daemon for browser-based setup..."
      info "Open the dashboard in your browser; pair with the code shown in logs."
      info "Stop the daemon with Ctrl+C when done; then run 'zeroclaw service install' for always-on."
      "$BIN" daemon || warn "Daemon exited with an error — run 'zeroclaw daemon' manually"
      ;;
    3)
      info "Skipped setup. Run '$QUICKSTART_COMMAND' (CLI) or 'zeroclaw daemon' (browser) when ready."
      ;;
    *)
      warn "Unknown choice '$quickstart_choice' — skipping. Run '$QUICKSTART_COMMAND' to configure."
      ;;
    esac
  else
    info "Non-interactive — skipping setup prompt. Run '$QUICKSTART_COMMAND' to configure."
  fi
fi"#
}

/// Render the source-build cargo-install step dispatcher, matching install.sh's
/// style (2-space indent, shellcheck pragmas), with the command derived from the
/// canonical spec. Dry-run narration and execute action are one source.
fn render_cargo_install(root: &Path) -> anyhow::Result<String> {
    // Resolve to confirm the spec is readable; the cargo flags are interpolated
    // at runtime via $CARGO_FLAGS, not baked, so the body is selection-stable.
    let _ = spec::resolve_flags(root, &Selection::Full)?;
    Ok([
        "  if [ \"$DRY_RUN\" = true ]; then",
        "    # shellcheck disable=SC2086",
        "    info \"[dry-run] Would run: cargo install --path . --locked --force $CARGO_FLAGS\"",
        "  else",
        "    # shellcheck disable=SC2086",
        "    cargo install --path . --locked --force $CARGO_FLAGS",
        "  fi",
    ]
    .join("\n"))
}

/// Splice generated route-control zones into the surrounding hand-written
/// installer helpers.
pub fn render_file(root: &Path, current: &str) -> anyhow::Result<String> {
    validate_zone_order(current)?;
    let routes = spec::install_routes()?;
    let route_contract = render_route_contract(&routes)?;
    let mut rendered = splice(current, ZONE_ROUTE_CONTRACT, &route_contract)?;
    for (zone, body) in [
        (ZONE_FEATURE_PICKER_HELPER, render_feature_picker_helper()),
        (ZONE_ROUTE_FLAGS, render_route_flags()),
        (ZONE_ROUTE_DECISION, render_route_decision()),
        (ZONE_SOURCE_DISPATCH_OPEN, render_source_dispatch_open()),
        (ZONE_RUST_BOOTSTRAP, render_rust_bootstrap()),
        (ZONE_FEATURE_PICKER, render_feature_picker()),
        (ZONE_APP_SELECTION, render_app_selection()),
        (ZONE_SOURCE_DISPATCH_CLOSE, render_source_dispatch_close()),
        (ZONE_PATH_HANDOFF, render_path_handoff()),
        (ZONE_QUICKSTART_HANDOFF, render_quickstart_handoff()),
    ] {
        rendered = splice(&rendered, zone, body)?;
    }
    splice(&rendered, ZONE_CARGO_INSTALL, &render_cargo_install(root)?)
}

fn validate_zone_order(current: &str) -> anyhow::Result<()> {
    let mut previous_end = 0;
    for layout in ZONE_LAYOUTS {
        let zone = layout.zone;
        let b = begin(zone);
        let e = end(zone);
        ensure_once(current, &b, &format!("generated:{zone} begin sentinel"))?;
        ensure_once(current, &e, &format!("generated:{zone} end sentinel"))?;
        ensure_once(
            current,
            layout.range_start,
            &format!("generated:{zone} range start"),
        )?;
        ensure_once(
            current,
            layout.range_end,
            &format!("generated:{zone} range end"),
        )?;
        let range_start = current
            .find(layout.range_start)
            .ok_or_else(|| anyhow::Error::msg(format!("missing generated:{zone} range start")))?
            + layout.range_start.len();
        let range_end = current
            .find(layout.range_end)
            .ok_or_else(|| anyhow::Error::msg(format!("missing generated:{zone} range end")))?;
        let begin_at = current.find(&b).ok_or_else(|| {
            anyhow::Error::msg(format!("missing generated:{zone} begin sentinel"))
        })?;
        let end_at = current
            .find(&e)
            .ok_or_else(|| anyhow::Error::msg(format!("missing generated:{zone} end sentinel")))?;
        anyhow::ensure!(
            begin_at >= previous_end && begin_at < end_at,
            "generated installer zones are out of canonical order at {zone}"
        );
        anyhow::ensure!(
            range_start <= begin_at && end_at + e.len() <= range_end,
            "generated:{zone} must remain inside its canonical structural range"
        );
        previous_end = end_at + e.len();
    }
    Ok(())
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let b = begin(zone);
    let e = end(zone);
    ensure_once(current, &b, &format!("generated:{zone} begin sentinel"))?;
    ensure_once(current, &e, &format!("generated:{zone} end sentinel"))?;
    let begin_at = current.find(&b).ok_or_else(|| {
        anyhow::Error::msg(format!(
            "install.sh missing generated:{zone} begin sentinel"
        ))
    })?;
    let after_begin = begin_at + b.len();
    let end_at = current.find(&e).ok_or_else(|| {
        anyhow::Error::msg(format!("install.sh missing generated:{zone} end sentinel"))
    })?;
    anyhow::ensure!(
        after_begin < end_at,
        "generated:{zone} sentinels are out of order"
    );
    let mut out = String::new();
    out.push_str(&current[..after_begin]);
    out.push('\n');
    out.push_str(body);
    out.push('\n');
    out.push_str(&current[end_at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn cargo_install_zone_has_dryrun_and_execute() {
        let z = render_cargo_install(&root()).unwrap();
        assert!(z.contains("[dry-run] Would run: cargo install"));
        assert!(z.contains("cargo install --path . --locked --force $CARGO_FLAGS"));
        assert!(z.contains("if [ \"$DRY_RUN\" = true ]; then"));
    }

    #[test]
    fn route_contract_comes_from_canonical_routes() {
        let contract = render_route_contract(&spec::install_routes().unwrap()).unwrap();
        assert!(contract.contains("DEFAULT_APPS=\"zerocode\""));
        assert!(contract.contains("PIPED_INSTALL_MODE=\"prebuilt\""));
        assert!(contract.contains("QUICKSTART_COMMAND=\"zeroclaw quickstart\""));
        assert!(contract.contains("QUICKSTART_SUBCOMMAND=\"quickstart\""));
        assert!(contract.contains("GUIDED_INSTALL_MODE=\"choice\""));
        assert!(contract.contains("GUIDED_QUICKSTART_MODE=\"offer\""));
        assert!(contract.contains("UNIX_PATH_RELOAD=\"true\""));
    }

    #[test]
    fn route_contract_rejects_source_default_or_policy_drift() {
        let mut divergent_defaults = spec::install_routes().unwrap();
        mutate_unix_variant(&mut divergent_defaults, RouteId::UnixGuided, |variant| {
            variant.apps = AppPolicy::ArchiveOptionalOrSourceDefaultsSelectable(&[
                "zerocode",
                "zeroclaw-desktop",
            ]);
        });
        assert!(render_route_contract(&divergent_defaults).is_err());

        let mut wrong_policy = spec::install_routes().unwrap();
        mutate_unix_variant(&mut wrong_policy, RouteId::UnixFast, |variant| {
            variant.apps = AppPolicy::ArchiveOptional(&["zerocode"]);
        });
        assert!(render_route_contract(&wrong_policy).is_err());
    }

    #[test]
    fn route_contract_rejects_invocation_and_interaction_drift() {
        let mut wrong_invocation = spec::install_routes().unwrap();
        mutate_unix_variant(&mut wrong_invocation, RouteId::UnixFast, |variant| {
            variant.invocation = Invocation::InteractiveScript;
        });
        assert!(render_route_contract(&wrong_invocation).is_err());

        let mut wrong_interaction = spec::install_routes().unwrap();
        mutate_unix_variant(&mut wrong_interaction, RouteId::UnixGuided, |variant| {
            variant.interaction = Interaction::NonInteractive;
        });
        assert!(render_route_contract(&wrong_interaction).is_err());
    }

    #[test]
    fn route_contract_rejects_source_path_and_quickstart_drift() {
        let mut wrong_path = spec::install_routes().unwrap();
        mutate_unix_variant(&mut wrong_path, RouteId::AdvancedSource, |variant| {
            variant.path_effect = PathEffect::NoInstallerChange;
        });
        assert!(render_route_contract(&wrong_path).is_err());

        let mut wrong_quickstart = spec::install_routes().unwrap();
        mutate_unix_variant(&mut wrong_quickstart, RouteId::UnixGuided, |variant| {
            variant.quickstart_command = "zeroclaw configure";
        });
        assert!(render_route_contract(&wrong_quickstart).is_err());
    }

    #[test]
    fn route_contract_rejects_guided_branch_drift() {
        let mut routes = spec::install_routes().unwrap();
        mutate_unix_variant(&mut routes, RouteId::UnixGuided, |variant| {
            variant.branch = BranchPolicy::PrebuiltOnly;
        });
        assert!(render_route_contract(&routes).is_err());
    }

    fn route_by_id_mut(routes: &mut [InstallRoute], id: RouteId) -> &mut InstallRoute {
        routes.iter_mut().find(|route| route.id == id).unwrap()
    }

    fn mutate_unix_variant(
        routes: &mut [InstallRoute],
        id: RouteId,
        mutate: impl FnOnce(&mut InstallVariant),
    ) {
        let route = route_by_id_mut(routes, id);
        let mut variants = route.variants.to_vec();
        mutate(
            variants
                .iter_mut()
                .find(|variant| variant.platform == Platform::Unix)
                .unwrap(),
        );
        route.variants = Box::leak(variants.into_boxed_slice());
    }

    #[test]
    fn real_install_sh_contains_every_generated_zone() {
        let cur = std::fs::read_to_string(root().join("install.sh")).unwrap();
        let once = render_file(&root(), &cur).unwrap();
        let twice = render_file(&root(), &once).unwrap();
        assert_eq!(cur, once, "checked-in installer must be freshly rendered");
        assert_eq!(once, twice, "render must be idempotent");
    }

    #[test]
    fn splice_rejects_duplicate_zone_markers() {
        let b = begin(ZONE_ROUTE_DECISION);
        let e = end(ZONE_ROUTE_DECISION);
        let duplicate = format!("before\n{b}\nfirst\n{e}\n{b}\nsecond\n{e}\nafter");
        assert!(
            splice(&duplicate, ZONE_ROUTE_DECISION, "replacement").is_err(),
            "duplicate generated zones must fail closed"
        );
    }

    #[test]
    fn installer_contract_restores_unix_route_drift() {
        let script = std::fs::read_to_string(root().join("install.sh")).unwrap();
        let mutations = [
            (
                "explicit prebuilt invocation",
                "  --prebuilt) INSTALL_MODE=\"prebuilt\" ;;",
                "  --prebuilt) INSTALL_MODE=\"source\" ;;",
            ),
            (
                "source invocation",
                "  --source) INSTALL_MODE=\"source\" ;;",
                "  --source) INSTALL_MODE=\"prebuilt\" ;;",
            ),
            (
                "source dispatch",
                "if [ \"${PREBUILT_OK:-false}\" != true ]; then",
                "if false; then",
            ),
            (
                "prebuilt fallback transition",
                "    warn \"Pre-built install failed — continuing with source build\"\n    INSTALL_MODE=\"source\"",
                "    die \"Pre-built install failed\"",
            ),
            ("feature-picker guard", "  if [ -t 0 ] &&", "  if true &&"),
            (
                "feature selection",
                "    interactive_feature_picker \"Cargo.toml\"",
                "    : # picker removed",
            ),
            (
                "selected feature application",
                "      CARGO_FLAGS=\"$CARGO_FLAGS --features $USER_FEATURES\"",
                "      : # selected features ignored",
            ),
            (
                "default apps",
                "    WANT_APPS=\"$DEFAULT_APPS\"",
                "    WANT_APPS=\"\"",
            ),
            (
                "selected app installation",
                "      cargo install --path \"$app_path\" --locked --force",
                "      : # selected app ignored",
            ),
            (
                "Rust bootstrap",
                "  if [ \"$NEED_RUST\" = true ]; then",
                "  if false; then",
            ),
            (
                "PATH profile update",
                "  } >>\"$PROFILE\" 2>/dev/null; then",
                "  } >\"$PROFILE\" 2>/dev/null; then",
            ),
            (
                "PATH outer guard",
                "if [ \"$PATH_ALREADY_SET\" = true ]; then",
                "if false; then",
            ),
            (
                "Quickstart outer guard",
                "if [ \"$SKIP_QUICKSTART\" = false ] && [ \"$DRY_RUN\" != true ] && [ -f \"$BIN\" ]; then",
                "if false; then",
            ),
            (
                "Quickstart offer",
                "  elif [ \"$GUIDED_QUICKSTART_MODE\" = \"offer\" ] && [ -t 0 ]; then",
                "  elif false; then",
            ),
        ];

        for (name, current, divergent) in mutations {
            let mismatch = script.replacen(current, divergent, 1);
            assert_ne!(script, mismatch, "{name} fixture must modify install.sh");
            assert_eq!(
                script,
                render_file(&root(), &mismatch).unwrap(),
                "installer generation must restore {name} drift"
            );
        }
    }

    #[test]
    fn installer_contract_restores_picker_helper_drift() {
        let script = std::fs::read_to_string(root().join("install.sh")).unwrap();
        let mismatch = script.replacen(
            "  selected_features=$(expand_default_features \"$toml\")",
            "  selected_features=\"\"",
            1,
        );
        assert_ne!(script, mismatch, "fixture must modify the picker helper");
        assert_eq!(
            script,
            render_file(&root(), &mismatch).unwrap(),
            "installer generation must restore picker helper drift"
        );
    }

    #[test]
    fn installer_contract_rejects_generated_zone_relocation() {
        let script = std::fs::read_to_string(root().join("install.sh")).unwrap();
        let path_begin = begin(ZONE_PATH_HANDOFF);
        let path_end = end(ZONE_PATH_HANDOFF);
        let start = script.find(&path_begin).unwrap();
        let finish = script.find(&path_end).unwrap() + path_end.len();
        let path_zone = &script[start..finish];
        let without_path = format!("{}{}", &script[..start], &script[finish..]);
        let insertion = without_path
            .find(&begin(ZONE_SOURCE_DISPATCH_CLOSE))
            .unwrap();
        let relocated = format!(
            "{}{}\n{}",
            &without_path[..insertion],
            path_zone,
            &without_path[insertion..]
        );
        assert_ne!(script, relocated);
        assert!(render_file(&root(), &relocated).is_err());
    }

    #[test]
    fn installer_contract_rejects_same_order_zone_relocation() {
        let script = std::fs::read_to_string(root().join("install.sh")).unwrap();
        let picker_begin = begin(ZONE_FEATURE_PICKER);
        let picker_end = end(ZONE_FEATURE_PICKER);
        let start = script.find(&picker_begin).unwrap();
        let finish = script.find(&picker_end).unwrap() + picker_end.len();
        let picker_zone = &script[start..finish];
        let without_picker = format!("{}{}", &script[..start], &script[finish..]);
        let insertion = without_picker
            .find("  # ── Build and install")
            .expect("build section anchor");
        let relocated = format!(
            "{}{}\n{}",
            &without_picker[..insertion],
            picker_zone,
            &without_picker[insertion..]
        );
        assert_ne!(script, relocated);
        assert!(
            render_file(&root(), &relocated).is_err(),
            "zone placement must be structural, not only relative"
        );
    }

    #[test]
    fn installer_contract_allows_adjacent_handwritten_edits() {
        let script = std::fs::read_to_string(root().join("install.sh")).unwrap();
        let marker = begin(ZONE_ROUTE_CONTRACT);
        let customized = script.replacen(&marker, &format!("# local note\n{marker}"), 1);
        let rendered = render_file(&root(), &customized).unwrap();
        assert!(rendered.contains("# local note\n"));
    }

    #[test]
    fn real_guided_flow_uses_generated_quickstart_command() {
        let script = std::fs::read_to_string(root().join("install.sh")).unwrap();
        let (_, guided_and_after) = script
            .split_once("# ── Quickstart prompt")
            .expect("quickstart prompt marker");
        let (guided, _) = guided_and_after
            .split_once("# Next-step hint")
            .expect("next-step marker");
        assert!(!guided.contains("zeroclaw quickstart"));
        assert!(guided.contains("$QUICKSTART_COMMAND"));
        assert!(guided.contains("\"$BIN\" \"$QUICKSTART_SUBCOMMAND\""));
    }
}
