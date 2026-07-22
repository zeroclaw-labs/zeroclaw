//! install.sh renderer. install.sh@HEAD is the behavioral reference and stays
//! hand-authored; we generate sentinel zones for the route contract and
//! source-build cargo-install step so installer behavior, dry-run narration,
//! and the real command come from the canonical spec and cannot drift.

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
const ZONE_CARGO_INSTALL: &str = "source-cargo-install";

fn route_by_id(routes: &[InstallRoute], id: RouteId) -> anyhow::Result<&InstallRoute> {
    routes
        .iter()
        .find(|route| route.id == id)
        .ok_or_else(|| anyhow::Error::msg(format!("missing canonical install route {id:?}")))
}

fn unix_variant(route: &InstallRoute) -> anyhow::Result<&InstallVariant> {
    route.variant(Platform::Unix)
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

/// Splice generated step zones into install.sh, leaving hand-written glue
/// untouched.
pub fn render_file(root: &Path, current: &str) -> anyhow::Result<String> {
    let routes = spec::install_routes()?;
    let with_routes = splice(
        current,
        ZONE_ROUTE_CONTRACT,
        &render_route_contract(&routes)?,
    )?;
    splice(
        &with_routes,
        ZONE_CARGO_INSTALL,
        &render_cargo_install(root)?,
    )
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let b = begin(zone);
    let e = end(zone);
    let begin_at = current.find(&b).ok_or_else(|| {
        anyhow::Error::msg(format!(
            "install.sh missing generated:{zone} BEGIN sentinel"
        ))
    })?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..].find(&e).ok_or_else(|| {
        anyhow::Error::msg(format!("install.sh missing generated:{zone} END sentinel"))
    })?;
    let end_at = after_begin + end_rel;
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
        assert!(once.contains("generated:route-contract"));
        assert!(once.contains("generated:source-cargo-install"));
        assert_eq!(once, twice, "render must be idempotent");
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
