//! Canonical install spec. install.sh@HEAD is the behavioral reference; this
//! spec reproduces it and every surface (install.sh, setup.bat, ...) renders
//! from it. Dry-run is intrinsic: each step pairs a `narration` (the dry-run
//! line) with its `action` (the real op), and the rendered script chooses

use std::path::Path;

/// Platforms a step applies to. Renderers skip steps not in their set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Unix,
    Windows,
}

/// Named user-facing installation paths. Renderers consume this route-level
/// contract before choosing the lower-level install action plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteId {
    UnixFast,
    UnixGuided,
    WindowsPrebuilt,
    AdvancedSource,
}

/// How a user enters an installation route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invocation {
    PipedScript,
    InteractiveScript,
    ManualPrebuilt,
    SourceScript,
    CargoInstall,
}

impl Invocation {
    /// Exact entry command for invocations whose command lives in this spec.
    /// The Windows prebuilt route points to a maintained PowerShell block
    /// instead of duplicating that release-sensitive script here.
    pub fn command(self) -> Option<&'static str> {
        match self {
            Self::PipedScript => Some(
                "curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | sh",
            ),
            Self::InteractiveScript => Some(
                "git clone https://github.com/zeroclaw-labs/zeroclaw.git\ncd zeroclaw\n./install.sh",
            ),
            Self::ManualPrebuilt => None,
            Self::SourceScript => Some("./install.sh --source"),
            Self::CargoInstall => Some("cargo install --locked --path ."),
        }
    }
}

/// Whether the route permits interactive choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interaction {
    NonInteractive,
    Guided,
}

/// Which install branch or branches the route can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchPolicy {
    PreferPrebuiltThenSource,
    UserChoice,
    PrebuiltOnly,
    SourceOnly,
}

/// Which optional app policy applies to a route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppPolicy {
    ArchiveOptional(&'static [&'static str]),
    SourceDefaultsSelectable(&'static [&'static str]),
    ArchiveOptionalOrSourceDefaults(&'static [&'static str]),
    ArchiveOptionalOrSourceDefaultsSelectable(&'static [&'static str]),
    CoreOnly,
}

impl AppPolicy {
    pub fn selectable(self) -> bool {
        matches!(
            self,
            Self::SourceDefaultsSelectable(_) | Self::ArchiveOptionalOrSourceDefaultsSelectable(_)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeaturePolicy {
    Fixed,
    Selectable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RustPolicy {
    NotRequired,
    BootstrapIfMissing,
    RequiredAheadOfTime,
}

/// The shell-visible result of adding the installed binaries to PATH.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathEffect {
    ProfileUpdateRequiresReload,
    CurrentAndFutureShell,
    NoInstallerChange,
}

/// What the route offers immediately after installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickstartHandoff {
    PrintCommand,
    OfferCliOrBrowser,
    RunAutomatically,
    RunExplicitly,
}

pub const QUICKSTART_COMMAND: &str = "zeroclaw quickstart";
pub const ZEROCODE_APP: &str = "zerocode";

/// Platform-specific first-run semantics within one user-visible route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallVariant {
    pub platform: Platform,
    pub invocation: Invocation,
    pub interaction: Interaction,
    pub branch: BranchPolicy,
    pub apps: AppPolicy,
    pub features: FeaturePolicy,
    pub path_effect: PathEffect,
    pub handoff: QuickstartHandoff,
    pub quickstart_command: &'static str,
    pub rust: RustPolicy,
}

impl InstallVariant {
    fn validate(&self, route_id: RouteId) -> anyhow::Result<()> {
        anyhow::ensure!(
            !(matches!(self.interaction, Interaction::NonInteractive)
                && matches!(self.branch, BranchPolicy::UserChoice)),
            "non-interactive install route {:?} cannot require a user branch choice",
            route_id
        );

        match self.branch {
            BranchPolicy::PrebuiltOnly => anyhow::ensure!(
                matches!(self.apps, AppPolicy::ArchiveOptional(_)),
                "prebuilt-only install route {:?} must use ArchiveOptional",
                route_id
            ),
            BranchPolicy::SourceOnly => anyhow::ensure!(
                matches!(
                    self.apps,
                    AppPolicy::SourceDefaultsSelectable(_) | AppPolicy::CoreOnly
                ),
                "source-only install route {:?} must use a source app policy",
                route_id
            ),
            BranchPolicy::PreferPrebuiltThenSource | BranchPolicy::UserChoice => anyhow::ensure!(
                matches!(
                    self.apps,
                    AppPolicy::ArchiveOptionalOrSourceDefaults(_)
                        | AppPolicy::ArchiveOptionalOrSourceDefaultsSelectable(_)
                ),
                "mixed-branch install route {:?} must use a mixed app policy",
                route_id
            ),
        }

        anyhow::ensure!(
            !matches!(self.branch, BranchPolicy::PrebuiltOnly)
                || self.features == FeaturePolicy::Fixed,
            "prebuilt-only install route {:?} cannot expose feature selection",
            route_id
        );
        anyhow::ensure!(
            !(matches!(self.interaction, Interaction::NonInteractive)
                && (self.apps.selectable() || self.features == FeaturePolicy::Selectable)),
            "non-interactive install route {:?} cannot expose app or feature selection",
            route_id
        );
        anyhow::ensure!(
            !matches!(self.branch, BranchPolicy::SourceOnly)
                || self.rust != RustPolicy::NotRequired,
            "source-only install route {:?} must define how Rust is provided",
            route_id
        );
        anyhow::ensure!(
            self.invocation != Invocation::CargoInstall
                || self.rust == RustPolicy::RequiredAheadOfTime,
            "cargo-install route {:?} must require Rust ahead of time",
            route_id
        );
        anyhow::ensure!(
            self.quickstart_command == QUICKSTART_COMMAND,
            "install route {:?} must use the canonical Quickstart command",
            route_id
        );
        Ok(())
    }
}

/// Canonical first-run semantics for one user-visible installation route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallRoute {
    pub id: RouteId,
    pub variants: &'static [InstallVariant],
}

impl InstallRoute {
    pub fn variant(&self, platform: Platform) -> anyhow::Result<&InstallVariant> {
        let mut variants = self
            .variants
            .iter()
            .filter(|variant| variant.platform == platform);
        let variant = variants.next().ok_or_else(|| {
            anyhow::Error::msg(format!(
                "install route {:?} has no {:?} variant",
                self.id, platform
            ))
        })?;
        anyhow::ensure!(
            variants.next().is_none(),
            "install route {:?} has duplicate {:?} variants",
            self.id,
            platform
        );
        Ok(variant)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.variants.is_empty(),
            "install route {:?} must define at least one platform variant",
            self.id
        );
        for (index, variant) in self.variants.iter().enumerate() {
            anyhow::ensure!(
                !self.variants[..index]
                    .iter()
                    .any(|prior| prior.platform == variant.platform),
                "install route {:?} has duplicate {:?} variants",
                self.id,
                variant.platform
            );
            variant.validate(self.id)?;
        }
        Ok(())
    }
}

const ZEROCODE_APPS: &[&str] = &[ZEROCODE_APP];

const UNIX_FAST_ROUTE: InstallRoute = InstallRoute {
    id: RouteId::UnixFast,
    variants: &[InstallVariant {
        platform: Platform::Unix,
        invocation: Invocation::PipedScript,
        interaction: Interaction::NonInteractive,
        branch: BranchPolicy::PreferPrebuiltThenSource,
        apps: AppPolicy::ArchiveOptionalOrSourceDefaults(ZEROCODE_APPS),
        features: FeaturePolicy::Fixed,
        path_effect: PathEffect::ProfileUpdateRequiresReload,
        handoff: QuickstartHandoff::PrintCommand,
        quickstart_command: QUICKSTART_COMMAND,
        rust: RustPolicy::BootstrapIfMissing,
    }],
};

const UNIX_GUIDED_ROUTE: InstallRoute = InstallRoute {
    id: RouteId::UnixGuided,
    variants: &[InstallVariant {
        platform: Platform::Unix,
        invocation: Invocation::InteractiveScript,
        interaction: Interaction::Guided,
        branch: BranchPolicy::UserChoice,
        apps: AppPolicy::ArchiveOptionalOrSourceDefaultsSelectable(ZEROCODE_APPS),
        features: FeaturePolicy::Selectable,
        path_effect: PathEffect::ProfileUpdateRequiresReload,
        handoff: QuickstartHandoff::OfferCliOrBrowser,
        quickstart_command: QUICKSTART_COMMAND,
        rust: RustPolicy::BootstrapIfMissing,
    }],
};

const WINDOWS_PREBUILT_ROUTE: InstallRoute = InstallRoute {
    id: RouteId::WindowsPrebuilt,
    variants: &[InstallVariant {
        platform: Platform::Windows,
        invocation: Invocation::ManualPrebuilt,
        interaction: Interaction::NonInteractive,
        branch: BranchPolicy::PrebuiltOnly,
        apps: AppPolicy::ArchiveOptional(ZEROCODE_APPS),
        features: FeaturePolicy::Fixed,
        path_effect: PathEffect::CurrentAndFutureShell,
        handoff: QuickstartHandoff::RunAutomatically,
        quickstart_command: QUICKSTART_COMMAND,
        rust: RustPolicy::NotRequired,
    }],
};

const ADVANCED_SOURCE_ROUTE: InstallRoute = InstallRoute {
    id: RouteId::AdvancedSource,
    variants: &[
        InstallVariant {
            platform: Platform::Unix,
            invocation: Invocation::SourceScript,
            interaction: Interaction::Guided,
            branch: BranchPolicy::SourceOnly,
            apps: AppPolicy::SourceDefaultsSelectable(ZEROCODE_APPS),
            features: FeaturePolicy::Selectable,
            path_effect: PathEffect::ProfileUpdateRequiresReload,
            handoff: QuickstartHandoff::OfferCliOrBrowser,
            quickstart_command: QUICKSTART_COMMAND,
            rust: RustPolicy::BootstrapIfMissing,
        },
        InstallVariant {
            platform: Platform::Windows,
            invocation: Invocation::CargoInstall,
            interaction: Interaction::NonInteractive,
            branch: BranchPolicy::SourceOnly,
            apps: AppPolicy::CoreOnly,
            features: FeaturePolicy::Fixed,
            path_effect: PathEffect::NoInstallerChange,
            handoff: QuickstartHandoff::RunExplicitly,
            quickstart_command: QUICKSTART_COMMAND,
            rust: RustPolicy::RequiredAheadOfTime,
        },
    ],
};

/// The canonical install routes, in the order user-facing renderers present
/// them. Validation is fallible so callers receive contract errors rather than
/// a process panic when a future route becomes inconsistent.
pub fn install_routes() -> anyhow::Result<Vec<InstallRoute>> {
    let routes = vec![
        UNIX_FAST_ROUTE,
        UNIX_GUIDED_ROUTE,
        WINDOWS_PREBUILT_ROUTE,
        ADVANCED_SOURCE_ROUTE,
    ];
    validate_install_routes(&routes)?;
    Ok(routes)
}

pub fn validate_install_routes(routes: &[InstallRoute]) -> anyhow::Result<()> {
    anyhow::ensure!(
        routes.iter().map(|route| route.id).collect::<Vec<_>>()
            == [
                RouteId::UnixFast,
                RouteId::UnixGuided,
                RouteId::WindowsPrebuilt,
                RouteId::AdvancedSource,
            ],
        "canonical install routes must contain each supported route exactly once in presentation order"
    );
    for route in routes {
        route.validate()?;
    }
    for (route, expected_platforms) in routes.iter().zip([
        &[Platform::Unix][..],
        &[Platform::Unix][..],
        &[Platform::Windows][..],
        &[Platform::Unix, Platform::Windows][..],
    ]) {
        let actual: Vec<_> = route
            .variants
            .iter()
            .map(|variant| variant.platform)
            .collect();
        anyhow::ensure!(
            actual == expected_platforms,
            "install route {:?} has an incomplete platform contract",
            route.id
        );
    }
    Ok(())
}

/// A value resolved from canonical sources, never a literal in a surface.
/// Renderers expand these into their platform dialect (`$VAR` vs `%VAR%`).
#[derive(Clone)]
pub enum Value {
    /// Workspace version from `[workspace.package] version`.
    Version,
    /// MSRV from `[workspace.package] rust-version`.
    Msrv,
    /// Resolved cargo feature flags for the active preset/selection.
    CargoFlags,
    /// Platform web/dist data dir (matches gateway auto-detect).
    WebDataDir,
    /// Install bin dir (cargo bin on Unix, %USERPROFILE%\.zeroclaw\bin on Win).
    BinDir,
    /// Literal text that is platform-invariant and not drift-prone.
    Lit(String),
    /// Concatenation, so descriptions interpolate resolved values.
    Concat(Vec<Value>),
}

/// What a step does when executed for real. Each renderer emits the
/// platform-specific command; the abstract op is the single definition.
#[derive(Clone)]
pub enum Action {
    /// Download the prebuilt asset to a temp dir.
    DownloadPrebuilt,
    /// Install the main binary into BinDir.
    InstallBinary,
    /// Install a named app binary (e.g. zerocode) into BinDir.
    InstallApp { app: String },
    /// Bootstrap the Rust toolchain via rustup.
    InstallToolchain,
    /// `cargo install --path . --locked --force <CargoFlags>`.
    CargoInstallSelf,
    /// `cargo install --path <dir> --locked --force`.
    CargoInstallApp { path: Value },
    /// Build the web dashboard (`cargo web build`).
    BuildWebDashboard,
    /// Copy built web/dist into WebDataDir.
    InstallWebDist,
    /// Add BinDir to PATH.
    AddToPath,
}

/// One install step. Pairs the real op with how it narrates itself. A step
/// never prints a literal path: `narrate()` interpolates resolved `Value`s, so
/// the dry-run line and the real action are guaranteed to describe the same
/// thing from the same data.
#[derive(Clone)]
pub struct Step {
    pub id: &'static str,
    /// How this step narrates its intent (used to build the dry-run line).
    pub narration: Value,
    pub action: Action,
    pub when: When,
    pub platforms: &'static [Platform],
}

impl Step {
    /// Whether this step participates on the given platform.
    pub fn applies_to(&self, p: Platform) -> bool {
        self.platforms.contains(&p)
    }
}

/// The install flow as it actually is: a divergence (prebuilt vs source) that
/// reconverges at a shared tail. The tree makes branch + convergence a property
/// of the type, not something a reader reconstructs from per-step `when` flags.
pub struct Plan {
    /// Mutually exclusive install branches, selected at runtime.
    pub diverge: Branches,
    /// Steps both branches run after converging (PATH, quickstart).
    pub converge: Vec<Step>,
    /// Resolved canonical data the steps interpolate.
    pub resolved: Resolved,
}

/// The two mutually exclusive install branches.
pub struct Branches {
    pub prebuilt: Vec<Step>,
    pub source: Vec<Step>,
}

impl Plan {
    /// Build the canonical plan for a selection on a platform.
    pub fn build(
        manifest_dir: &Path,
        platform: Platform,
        selection: &Selection,
    ) -> anyhow::Result<Plan> {
        let resolved = resolve(manifest_dir, selection)?;
        let keep = |steps: Vec<Step>| -> Vec<Step> {
            steps
                .into_iter()
                .filter(|s| s.applies_to(platform))
                .collect()
        };
        let plan = Plan {
            diverge: Branches {
                prebuilt: keep(prebuilt_branch()),
                source: keep(source_branch()),
            },
            converge: keep(converge_tail()),
            resolved,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Every step the plan can run, in no particular order - for invariants and
    /// coverage checks that don't care about branch structure.
    pub fn all_steps(&self) -> impl Iterator<Item = &Step> {
        self.diverge
            .prebuilt
            .iter()
            .chain(self.diverge.source.iter())
            .chain(self.converge.iter())
    }

    /// Invariant that makes dry-run trustworthy: every step has a non-empty
    /// narration, so the dry-run pass describes the entire plan with no silent
    /// gaps. A mutating step without a dry-run line is a bug, caught here.
    pub fn validate(&self) -> anyhow::Result<()> {
        for s in self.all_steps() {
            anyhow::ensure!(
                !matches!(s.narration, Value::Lit(ref l) if l.is_empty()),
                "step `{}` has empty narration; dry-run would hide it",
                s.id
            );
        }
        Ok(())
    }
}

/// What a step emits in dry-run: the narration prefixed so users see it
/// is a no-op preview. Renderers turn this into their dialect (`info`/`echo`).
pub fn dry_run_line(narration_text: &str) -> String {
    format!("[dry-run] Would {narration_text}")
}

/// Conditions for an *intra-branch* step. Branch selection (prebuilt vs source)
/// is structural - it lives in `Plan`, not here - so this only covers the
/// genuinely conditional steps within a branch, mirroring install.sh's guards.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Unconditional within its branch.
    Always,
    /// Source branch: toolchain missing, bootstrap rustup.
    ToolchainMissing,
    /// Source branch: gateway feature resolved in.
    GatewayResolved,
    /// Source branch: gateway resolved AND npm present (else skip-warn).
    GatewayResolvedAndNpm,
}

/// The resolved canonical data a render pass needs, computed once from
/// Cargo.toml + the requested preset/features.
pub struct Resolved {
    pub version: String,
    pub msrv: String,
    pub edition: String,
    pub default_features: Vec<String>,
    pub all_features: Vec<String>,
    /// Cargo flag string for the active selection (e.g. "--no-default-features
    /// --features agent-runtime,gateway" or "" for full default build).
    pub cargo_flags: String,
}

pub fn non_row_features(
    meta: &cargo_metadata::Metadata,
    pkg: &cargo_metadata::Package,
) -> Vec<String> {
    let _ = meta;
    read_registry_list(pkg, "non_row_features")
}

/// Features intentionally added to Cargo defaults for standard distribution
/// artifacts. This policy is not derivable from the feature graph, so it lives
/// in the canonical registry rather than in release or packaging scripts.
pub fn dist_extra_features(pkg: &cargo_metadata::Package) -> anyhow::Result<Vec<String>> {
    required_registry_list(pkg, "dist_extra_features")
}

/// Target-specific compatibility exclusions for standard distribution builds.
/// Release workflows provide a target triple and never duplicate this policy.
pub fn dist_target_exclusions(
    pkg: &cargo_metadata::Package,
) -> anyhow::Result<std::collections::BTreeMap<String, Vec<String>>> {
    parse_target_exclusions(pkg.metadata.get("zeroclaw"))
}

/// Features whose build needs system libraries/tooling absent from the minimal
/// static container image, excluded from `Selection::All`. Read from
/// `[package.metadata.zeroclaw] container_excluded_features`; never shadowed.
pub fn container_excluded_features(pkg: &cargo_metadata::Package) -> Vec<String> {
    read_registry_list(pkg, "container_excluded_features")
}

fn read_registry_list(pkg: &cargo_metadata::Package, key: &str) -> Vec<String> {
    pkg.metadata
        .get("zeroclaw")
        .and_then(|z| z.get(key))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn required_registry_list(pkg: &cargo_metadata::Package, key: &str) -> anyhow::Result<Vec<String>> {
    parse_required_registry_list(pkg.metadata.get("zeroclaw"), key)
}

fn parse_required_registry_list(
    registry: Option<&serde_json::Value>,
    key: &str,
) -> anyhow::Result<Vec<String>> {
    let value = registry
        .and_then(|zeroclaw| zeroclaw.get(key))
        .ok_or_else(|| anyhow::Error::msg(format!("missing [package.metadata.zeroclaw] {key}")))?;
    parse_nonempty_string_list(value, &format!("[package.metadata.zeroclaw] {key}"))
}

fn parse_nonempty_string_list(
    value: &serde_json::Value,
    path: &str,
) -> anyhow::Result<Vec<String>> {
    let entries = value
        .as_array()
        .ok_or_else(|| anyhow::Error::msg(format!("{path} must be an array")))?;
    anyhow::ensure!(!entries.is_empty(), "{path} must not be empty");

    let mut values = Vec::with_capacity(entries.len());
    for entry in entries {
        let feature = entry
            .as_str()
            .filter(|feature| !feature.is_empty())
            .ok_or_else(|| {
                anyhow::Error::msg(format!("{path} entries must be non-empty strings"))
            })?;
        anyhow::ensure!(
            !values.iter().any(|existing| existing == feature),
            "duplicate {path} entry `{feature}`"
        );
        values.push(feature.to_owned());
    }
    Ok(values)
}

fn parse_target_exclusions(
    registry: Option<&serde_json::Value>,
) -> anyhow::Result<std::collections::BTreeMap<String, Vec<String>>> {
    let value = registry
        .and_then(|zeroclaw| zeroclaw.get("dist_target_exclusions"))
        .ok_or_else(|| {
            anyhow::Error::msg("missing [package.metadata.zeroclaw.dist_target_exclusions]")
        })?;
    let entries = value.as_object().ok_or_else(|| {
        anyhow::Error::msg("[package.metadata.zeroclaw.dist_target_exclusions] must be a table")
    })?;
    anyhow::ensure!(
        !entries.is_empty(),
        "dist_target_exclusions must not be empty"
    );

    let mut targets = std::collections::BTreeMap::new();
    for (target, value) in entries {
        anyhow::ensure!(!target.is_empty(), "dist target must not be empty");
        targets.insert(
            target.to_owned(),
            parse_nonempty_string_list(value, &format!("dist_target_exclusions.{target}"))?,
        );
    }
    Ok(targets)
}

/// Remove target-specific features from a resolved selection. Exclusions fail
/// closed when they are stale or unknown so release policy cannot silently
/// drift away from the canonical base set.
pub fn exclude_features(
    mut features: Vec<String>,
    excluded: &[String],
) -> anyhow::Result<Vec<String>> {
    for feature in excluded {
        anyhow::ensure!(
            features.iter().any(|candidate| candidate == feature),
            "cannot exclude `{feature}` because it is not in the resolved selection"
        );
        features.retain(|candidate| candidate != feature);
    }
    Ok(features)
}

/// Prebuilt install branch, reproducing install.sh@HEAD's prebuilt path.
pub fn prebuilt_branch() -> Vec<Step> {
    use Action::*;
    use Platform::{Unix, Windows};
    use When::Always;
    vec![
        Step {
            id: "download-prebuilt",
            narration: Value::Lit("download the prebuilt asset".into()),
            action: DownloadPrebuilt,
            when: Always,
            platforms: &[Unix, Windows],
        },
        Step {
            id: "install-binary",
            narration: Value::Concat(vec![Value::Lit("install to ".into()), Value::BinDir]),
            action: InstallBinary,
            when: Always,
            platforms: &[Unix, Windows],
        },
        Step {
            id: "install-web-dist-bundled",
            narration: Value::Concat(vec![
                Value::Lit("install web dashboard to ".into()),
                Value::WebDataDir,
            ]),
            action: InstallWebDist,
            when: Always,
            platforms: &[Unix, Windows],
        },
    ]
}

/// Source build branch, reproducing install.sh@HEAD's source path. Intra-branch
/// conditionals stay as `When`; branch selection is structural (this fn IS the
/// source branch).
pub fn source_branch() -> Vec<Step> {
    use Action::*;
    use Platform::{Unix, Windows};
    use When::*;
    vec![
        Step {
            id: "install-toolchain",
            narration: Value::Lit("install Rust via rustup".into()),
            action: InstallToolchain,
            when: ToolchainMissing,
            platforms: &[Unix, Windows],
        },
        Step {
            id: "cargo-install-self",
            narration: Value::Concat(vec![
                Value::Lit("run: cargo install --path . --locked --force ".into()),
                Value::CargoFlags,
            ]),
            action: CargoInstallSelf,
            when: Always,
            platforms: &[Unix, Windows],
        },
        Step {
            id: "build-web-dashboard",
            narration: Value::Lit("build the web dashboard".into()),
            action: BuildWebDashboard,
            when: GatewayResolvedAndNpm,
            platforms: &[Unix, Windows],
        },
        Step {
            id: "install-web-dist",
            narration: Value::Concat(vec![
                Value::Lit("install web dashboard to ".into()),
                Value::WebDataDir,
            ]),
            action: InstallWebDist,
            when: GatewayResolved,
            platforms: &[Unix, Windows],
        },
    ]
}

/// Shared tail both branches reconverge to.
pub fn converge_tail() -> Vec<Step> {
    use Action::*;
    use Platform::{Unix, Windows};
    use When::Always;
    vec![Step {
        id: "add-to-path",
        narration: Value::Concat(vec![
            Value::Lit("add ".into()),
            Value::BinDir,
            Value::Lit(" to PATH".into()),
        ]),
        action: AddToPath,
        when: Always,
        platforms: &[Unix, Windows],
    }]
}

/// Resolve just the cargo flag string for a selection (public entry for
/// renderers that need per-selection flags without a full Plan).
pub fn resolve_flags(manifest_dir: &Path, selection: &Selection) -> anyhow::Result<String> {
    Ok(resolve(manifest_dir, selection)?.cargo_flags)
}

/// Resolve the canonical workspace version from Cargo.toml.
pub fn resolve_version(manifest_dir: &Path) -> anyhow::Result<String> {
    let meta = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest_dir.join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let root = meta
        .root_package()
        .cloned()
        .or_else(|| meta.workspace_packages().into_iter().next().cloned())
        .ok_or_else(|| anyhow::Error::msg("no root/workspace package"))?;
    Ok(root.version.to_string())
}

/// Resolve the explicit feature list for a selection - the names a `--features`
/// arg would carry. For `Full` (Cargo default) this returns the resolved
/// default leaves so container/packaging surfaces are explicit and
/// drift-checkable rather than relying on implicit cargo defaults.
pub fn resolve_feature_list(
    manifest_dir: &Path,
    selection: &Selection,
) -> anyhow::Result<Vec<String>> {
    let meta = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest_dir.join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let root = workspace_root_package(&meta)?;
    resolve_feature_list_from_package(&meta, root, selection)
}

fn workspace_root_package(
    meta: &cargo_metadata::Metadata,
) -> anyhow::Result<&cargo_metadata::Package> {
    meta.root_package()
        .or_else(|| meta.workspace_packages().into_iter().next())
        .ok_or_else(|| anyhow::Error::msg("no root/workspace package"))
}

fn resolve_feature_list_from_package(
    meta: &cargo_metadata::Metadata,
    root: &cargo_metadata::Package,
    selection: &Selection,
) -> anyhow::Result<Vec<String>> {
    let all_features: Vec<String> = root.features.keys().cloned().collect();
    let non_row = non_row_features(meta, root);
    let dist_extra = dist_extra_features(root)?;
    let container_excluded = container_excluded_features(root);
    let ctx = FeatureCtx {
        graph: &root.features,
        all: &all_features,
        non_row: &non_row,
        dist_extra: &dist_extra,
        container_excluded: &container_excluded,
    };
    selection.to_feature_list(&ctx)
}

/// Resolve a selection and apply the registry-owned compatibility policy for
/// one build target. Target policy is defined only for distribution selections.
pub fn resolve_feature_list_for_target(
    manifest_dir: &Path,
    selection: &Selection,
    target: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let meta = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest_dir.join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let root = workspace_root_package(&meta)?;
    let features = resolve_feature_list_from_package(&meta, root, selection)?;
    let Some(target) = target else {
        return Ok(features);
    };
    anyhow::ensure!(!target.is_empty(), "target must not be empty");
    anyhow::ensure!(
        matches!(selection, Selection::Dist | Selection::DistBroad),
        "target-specific exclusions are only defined for distribution selections"
    );

    let exclusions = dist_target_exclusions(root)?;
    for (configured_target, excluded) in &exclusions {
        for feature in excluded {
            anyhow::ensure!(
                features.iter().any(|candidate| candidate == feature),
                "dist_target_exclusions.{configured_target} names `{feature}`, which is not in the selected distribution"
            );
        }
    }

    exclude_features(
        features,
        exclusions
            .get(target)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    )
}

/// Read canonical data via `cargo_metadata` - Cargo's own resolver, so the
/// feature graph matches what builds actually see. No awk, no hand-parsing.
pub fn resolve(manifest_dir: &Path, selection: &Selection) -> anyhow::Result<Resolved> {
    let meta = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest_dir.join("Cargo.toml"))
        .no_deps()
        .exec()?;

    let root = meta
        .root_package()
        .cloned()
        .or_else(|| meta.workspace_packages().into_iter().next().cloned())
        .ok_or_else(|| anyhow::Error::msg("no root/workspace package"))?;
    let root = &root;

    let version = root.version.to_string();
    let msrv = root
        .rust_version
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let edition = format!("{:?}", root.edition);

    let all_features: Vec<String> = root.features.keys().cloned().collect();
    let non_row = non_row_features(&meta, root);
    let dist_extra = dist_extra_features(root)?;
    let container_excluded = container_excluded_features(root);
    let default_features = expand_default(&root.features, &non_row);

    let ctx = FeatureCtx {
        graph: &root.features,
        all: &all_features,
        non_row: &non_row,
        dist_extra: &dist_extra,
        container_excluded: &container_excluded,
    };
    let cargo_flags = selection.to_cargo_flags(&ctx)?;

    Ok(Resolved {
        version,
        msrv,
        edition,
        default_features,
        all_features,
        cargo_flags,
    })
}

/// Expand `default` to leaf features, walking aggregates - the typed twin of
/// install.sh `expand_default_features`. `non_row` is the registry-declared
/// aggregate/meta set (from Cargo.toml metadata), never a literal here.
fn expand_default(
    features: &std::collections::BTreeMap<String, Vec<String>>,
    non_row: &[String],
) -> Vec<String> {
    let is_aggregate = |f: &str| non_row.iter().any(|n| n == f);
    let mut leaf = Vec::new();
    let mut queue: Vec<String> = features.get("default").cloned().unwrap_or_default();
    while let Some(f) = queue.pop() {
        if f.starts_with("dep:") || f.contains('/') {
            continue;
        }
        if is_aggregate(&f) {
            if let Some(members) = features.get(&f) {
                queue.extend(members.iter().cloned());
            }
        } else if !leaf.contains(&f) {
            leaf.push(f);
        }
    }
    leaf.sort();
    leaf
}

/// What the user asked to build: a named preset or an explicit feature set.
pub enum Selection {
    /// Cargo default feature set.
    Full,
    /// Kernel only (`--no-default-features`).
    Minimal,
    /// Standard binary distribution: lean Cargo defaults plus the explicit
    /// registry-owned distribution additions. The set a single-artifact
    /// package manager ships.
    Dist,
    /// Measurement-only broad distribution: `Dist` plus the canonical
    /// `channels-full` aggregate. Not offered by installer menus until a
    /// stable broad artifact lifecycle exists.
    DistBroad,
    /// Every selectable feature (all − non_row − pure-alias). The docker
    /// `:all-features` kitchen sink.
    All,
    /// Explicit comma/space feature list (`--features X,Y`).
    Features(Vec<String>),
}

impl Selection {
    /// Canonical short id for this selection (menu key, docker tag stem, etc.).
    /// Surfaces render from this - they never type the name literally.
    pub fn id(&self) -> &'static str {
        match self {
            Selection::Full => "default",
            Selection::Minimal => "minimal",
            Selection::Dist => "dist",
            Selection::DistBroad => "dist-broad",
            Selection::All => "all",
            Selection::Features(_) => "custom",
        }
    }

    /// One-line human description, rendered into menus/help. Derived here so no
    /// surface hardcodes it.
    pub fn describe(&self) -> &'static str {
        match self {
            Selection::Full => "default feature set",
            Selection::Minimal => "core only, no default features",
            Selection::Dist => "lean standard distribution (recommended)",
            Selection::DistBroad => "broad-channel distribution measurement build",
            Selection::All => "every feature including hardware and browser",
            Selection::Features(_) => "custom feature selection",
        }
    }

    /// Every named selection accepted by the feature resolver.
    pub fn named() -> Vec<Selection> {
        vec![
            Selection::Minimal,
            Selection::Dist,
            Selection::DistBroad,
            Selection::Full,
            Selection::All,
        ]
    }

    /// The selections a packaged/menu surface offers, in menu order.
    pub fn menu() -> Vec<Selection> {
        Self::named()
            .into_iter()
            .filter(|selection| !matches!(selection, Selection::DistBroad))
            .collect()
    }

    /// Resolve a named build selection without coupling it to installer menus.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::named()
            .into_iter()
            .find(|selection| selection.id() == id)
    }

    /// Cargo flag string for this selection (wraps `to_feature_list`).
    fn to_cargo_flags(&self, ctx: &FeatureCtx) -> anyhow::Result<String> {
        match self {
            Selection::Full => Ok(String::new()),
            _ => Ok(ctx.flags_from(self.to_feature_list(ctx)?)),
        }
    }

    /// The explicit feature name list this selection resolves to. `Full`
    /// returns the resolved default leaves (so callers can be explicit);
    /// `Minimal` returns empty.
    fn to_feature_list(&self, ctx: &FeatureCtx) -> anyhow::Result<Vec<String>> {
        let mut set = match self {
            Selection::Minimal => Vec::new(),
            Selection::Full => ctx.expand("default"),
            Selection::Dist | Selection::DistBroad => {
                let mut s = ctx.expand("default");
                for feature in ctx.dist_extra {
                    anyhow::ensure!(
                        ctx.all.contains(feature),
                        "unknown dist_extra_features entry `{feature}` (not in [features])"
                    );
                    s.push(feature.clone());
                }
                if matches!(self, Selection::DistBroad) {
                    s.extend(ctx.expand("channels-full"));
                }
                s
            }
            Selection::All => ctx
                .all
                .iter()
                .filter(|f| {
                    !ctx.non_row.contains(f)
                        && !ctx.is_alias(f)
                        && !ctx.container_excluded.contains(f)
                })
                .cloned()
                .collect(),
            Selection::Features(feats) => {
                let picked: Vec<String> = feats
                    .iter()
                    .flat_map(|f| f.split([',', ' ']))
                    .map(str::trim)
                    .filter(|f| !f.is_empty())
                    .map(str::to_owned)
                    .collect();
                for f in &picked {
                    anyhow::ensure!(
                        ctx.all.contains(f),
                        "unknown feature `{f}` (not in [features])"
                    );
                }
                picked
            }
        };
        set.sort();
        set.dedup();
        Ok(set)
    }
}

/// Feature-graph context for resolving a `Selection`, assembled from
/// `cargo_metadata` + the Cargo.toml registry sets. The single place selection
/// math reads the graph; nothing here is a literal feature list.
pub struct FeatureCtx<'a> {
    pub graph: &'a std::collections::BTreeMap<String, Vec<String>>,
    pub all: &'a [String],
    pub non_row: &'a [String],
    pub dist_extra: &'a [String],
    pub container_excluded: &'a [String],
}

impl FeatureCtx<'_> {
    /// Expand one feature to its real leaf features (walks aggregates, skips
    /// deps and cross-crate refs). Same walk as `expand_default`.
    fn expand(&self, feature: &str) -> Vec<String> {
        let mut leaf = Vec::new();
        let mut queue: Vec<String> = self.graph.get(feature).cloned().unwrap_or_default();
        while let Some(f) = queue.pop() {
            if f.starts_with("dep:") || f.contains('/') {
                continue;
            }
            if self.non_row.contains(&f) {
                if let Some(m) = self.graph.get(&f) {
                    queue.extend(m.iter().cloned());
                }
            } else if !leaf.contains(&f) {
                leaf.push(f);
            }
        }
        leaf
    }

    /// A pure alias: a non-meta feature whose only member is another local
    /// feature (e.g. `channel-feishu = ["channel-lark"]`) - not separately
    /// selectable in `All`.
    fn is_alias(&self, feature: &str) -> bool {
        match self.graph.get(feature) {
            Some(members) => {
                members.len() == 1
                    && members.iter().all(|m| self.all.contains(m))
                    && !feature.starts_with("channel-")
            }
            None => false,
        }
    }

    /// Render a feature set into a cargo flag string (sorted, deduped).
    fn flags_from(&self, mut set: Vec<String>) -> String {
        set.sort();
        set.dedup();
        if set.is_empty() {
            "--no-default-features".into()
        } else {
            format!("--no-default-features --features {}", set.join(","))
        }
    }
}

pub fn web_data_dir_expr(platform: Platform) -> &'static str {
    match platform {
        // Unix renderer's else-arm (Linux). The macOS arm is emitted by the
        // sh renderer's own case; both forms live in the renderer, not baked.
        Platform::Unix => "${XDG_DATA_HOME:-${PREFIX}/.local/share}/zeroclaw/web/dist",
        Platform::Windows => "%LOCALAPPDATA%\\zeroclaw\\web\\dist",
    }
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

    fn route(routes: &[InstallRoute], id: RouteId) -> &InstallRoute {
        routes
            .iter()
            .find(|route| route.id == id)
            .unwrap_or_else(|| panic!("missing install route {id:?}"))
    }

    fn mutated_variant(
        route: &InstallRoute,
        platform: Platform,
        mutate: impl FnOnce(&mut InstallVariant),
    ) -> InstallRoute {
        let mut variants = route.variants.to_vec();
        mutate(
            variants
                .iter_mut()
                .find(|variant| variant.platform == platform)
                .unwrap(),
        );
        InstallRoute {
            variants: Box::leak(variants.into_boxed_slice()),
            ..*route
        }
    }

    #[test]
    fn install_routes_cover_the_four_user_paths() {
        let routes = install_routes().unwrap();

        assert_eq!(
            routes.iter().map(|route| route.id).collect::<Vec<_>>(),
            vec![
                RouteId::UnixFast,
                RouteId::UnixGuided,
                RouteId::WindowsPrebuilt,
                RouteId::AdvancedSource,
            ]
        );
    }

    #[test]
    fn install_route_contract_rejects_impossible_combinations() {
        let routes = install_routes().unwrap();

        let non_interactive_user_choice = mutated_variant(
            route(&routes, RouteId::UnixGuided),
            Platform::Unix,
            |variant| variant.interaction = Interaction::NonInteractive,
        );
        assert!(
            non_interactive_user_choice
                .validate()
                .unwrap_err()
                .to_string()
                .contains("cannot require a user branch choice")
        );

        for non_interactive_selection in [
            mutated_variant(
                route(&routes, RouteId::UnixFast),
                Platform::Unix,
                |variant| {
                    variant.apps =
                        AppPolicy::ArchiveOptionalOrSourceDefaultsSelectable(ZEROCODE_APPS);
                },
            ),
            mutated_variant(
                route(&routes, RouteId::UnixFast),
                Platform::Unix,
                |variant| {
                    variant.features = FeaturePolicy::Selectable;
                },
            ),
        ] {
            assert!(
                non_interactive_selection
                    .validate()
                    .unwrap_err()
                    .to_string()
                    .contains("cannot expose app or feature selection")
            );
        }

        let prebuilt_with_feature_selection = mutated_variant(
            route(&routes, RouteId::WindowsPrebuilt),
            Platform::Windows,
            |variant| variant.features = FeaturePolicy::Selectable,
        );
        assert!(
            prebuilt_with_feature_selection
                .validate()
                .unwrap_err()
                .to_string()
                .contains(
                    "prebuilt-only install route WindowsPrebuilt cannot expose feature selection"
                )
        );

        let mut missing_platform = routes.clone();
        let source = route(&missing_platform, RouteId::AdvancedSource);
        let unix_only = [*source.variant(Platform::Unix).unwrap()];
        missing_platform
            .iter_mut()
            .find(|route| route.id == RouteId::AdvancedSource)
            .unwrap()
            .variants = Box::leak(Box::new(unix_only));
        assert!(
            validate_install_routes(&missing_platform)
                .unwrap_err()
                .to_string()
                .contains("incomplete platform contract")
        );
    }

    #[test]
    fn install_route_contract_rejects_incompatible_app_policy_and_selection() {
        let routes = install_routes().unwrap();

        let incompatible_mixed_branch_policy = mutated_variant(
            route(&routes, RouteId::UnixFast),
            Platform::Unix,
            |variant| variant.apps = AppPolicy::ArchiveOptional(ZEROCODE_APPS),
        );
        assert!(
            incompatible_mixed_branch_policy
                .validate()
                .unwrap_err()
                .to_string()
                .contains("mixed-branch install route UnixFast must use a mixed app policy")
        );

        let source_with_archive_policy = mutated_variant(
            route(&routes, RouteId::AdvancedSource),
            Platform::Unix,
            |variant| variant.apps = AppPolicy::ArchiveOptional(ZEROCODE_APPS),
        );
        assert!(
            source_with_archive_policy
                .validate()
                .unwrap_err()
                .to_string()
                .contains("source-only install route AdvancedSource must use a source app policy")
        );

        let archive_with_app_selection = mutated_variant(
            route(&routes, RouteId::WindowsPrebuilt),
            Platform::Windows,
            |variant| variant.apps = AppPolicy::SourceDefaultsSelectable(ZEROCODE_APPS),
        );
        assert!(
            archive_with_app_selection
                .validate()
                .unwrap_err()
                .to_string()
                .contains("prebuilt-only install route WindowsPrebuilt must use ArchiveOptional")
        );
    }

    #[test]
    fn install_route_contract_rejects_duplicate_platforms() {
        let routes = install_routes().unwrap();
        let fast = route(&routes, RouteId::UnixFast);
        let duplicate_platforms = InstallRoute {
            variants: Box::leak(vec![fast.variants[0], fast.variants[0]].into_boxed_slice()),
            ..*fast
        };

        assert!(
            duplicate_platforms
                .validate()
                .unwrap_err()
                .to_string()
                .contains("install route UnixFast has duplicate Unix variants")
        );
    }

    #[test]
    fn unix_fast_and_guided_have_distinct_handoffs() {
        let routes = install_routes().unwrap();
        let fast = route(&routes, RouteId::UnixFast);
        let guided = route(&routes, RouteId::UnixGuided);
        let fast = fast.variant(Platform::Unix).unwrap();
        let guided = guided.variant(Platform::Unix).unwrap();

        assert_eq!(fast.interaction, Interaction::NonInteractive);
        assert_eq!(fast.handoff, QuickstartHandoff::PrintCommand);
        assert_eq!(guided.interaction, Interaction::Guided);
        assert_eq!(guided.handoff, QuickstartHandoff::OfferCliOrBrowser);
    }

    #[test]
    fn reads_non_row_from_registry_not_hardcoded() {
        let meta = cargo_metadata::MetadataCommand::new()
            .manifest_path(root().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        let pkg = meta
            .root_package()
            .cloned()
            .or_else(|| meta.workspace_packages().into_iter().next().cloned())
            .unwrap();
        let nr = non_row_features(&meta, &pkg);
        assert!(
            nr.contains(&"channels-full".to_string()),
            "registry must declare channels-full meta"
        );
        assert!(nr.contains(&"embedded-web".to_string()));
        assert!(!nr.is_empty(), "non_row must come from Cargo.toml metadata");
    }

    #[test]
    fn default_expands_to_leaves_excluding_aggregates() {
        let r = resolve(&root(), &Selection::Full).unwrap();
        assert!(!r.default_features.is_empty());
        assert!(
            r.default_features.iter().all(|f| f != "default-channels"),
            "aggregates must expand, not appear as leaves"
        );
        assert!(r.default_features.contains(&"gateway".to_string()));
    }

    #[test]
    fn minimal_is_no_default_features() {
        let r = resolve(&root(), &Selection::Minimal).unwrap();
        assert_eq!(r.cargo_flags, "--no-default-features");
    }

    #[test]
    fn explicit_features_validated() {
        assert!(
            resolve(
                &root(),
                &Selection::Features(vec!["nonexistent-xyz".into()])
            )
            .is_err()
        );
        let r = resolve(&root(), &Selection::Features(vec!["gateway".into()])).unwrap();
        assert!(
            r.cargo_flags
                .contains("--no-default-features --features gateway")
        );
    }

    #[test]
    fn dist_matches_lean_release_contract() {
        let features = resolve_feature_list(&root(), &Selection::Dist).unwrap();
        let mut expected = resolve_feature_list(&root(), &Selection::Full).unwrap();
        expected.extend(["channel-matrix", "channel-lark", "whatsapp-web"].map(str::to_owned));
        expected.sort();
        expected.dedup();
        assert_eq!(features, expected);
    }

    #[test]
    fn dist_broad_derives_membership_from_channels_full() {
        let meta = cargo_metadata::MetadataCommand::new()
            .manifest_path(root().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        let pkg = workspace_root_package(&meta).unwrap();
        let all_features: Vec<String> = pkg.features.keys().cloned().collect();
        let non_row = non_row_features(&meta, pkg);
        let dist_extra = dist_extra_features(pkg).unwrap();
        let container_excluded = container_excluded_features(pkg);
        let ctx = FeatureCtx {
            graph: &pkg.features,
            all: &all_features,
            non_row: &non_row,
            dist_extra: &dist_extra,
            container_excluded: &container_excluded,
        };

        let mut expected = resolve_feature_list(&root(), &Selection::Dist).unwrap();
        expected.extend(ctx.expand("channels-full"));
        expected.sort();
        expected.dedup();

        let broad = resolve_feature_list(&root(), &Selection::DistBroad).unwrap();
        assert_eq!(broad, expected);
        assert!(broad.contains(&"channel-slack".to_string()));
        assert!(!broad.contains(&"hardware".to_string()));
    }

    #[test]
    fn dist_broad_is_named_but_not_offered_in_installer_menus() {
        assert_eq!(Selection::DistBroad.id(), "dist-broad");
        assert!(matches!(
            Selection::from_id("dist-broad"),
            Some(Selection::DistBroad)
        ));
        assert!(Selection::from_id("unknown-selection").is_none());
        assert!(
            Selection::menu()
                .iter()
                .all(|selection| selection.id() != Selection::DistBroad.id())
        );
    }

    #[test]
    fn all_is_superset_of_dist() {
        let dist = resolve(&root(), &Selection::Dist).unwrap();
        let all = resolve(&root(), &Selection::All).unwrap();
        // All includes optional features outside the lean distribution.
        assert!(
            all.cargo_flags.contains("hardware"),
            "all is the kitchen sink"
        );
        assert!(dist.cargo_flags.len() < all.cargo_flags.len());
    }

    #[test]
    fn dist_extras_read_from_registry() {
        let meta = cargo_metadata::MetadataCommand::new()
            .manifest_path(root().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        let pkg = meta
            .root_package()
            .cloned()
            .or_else(|| meta.workspace_packages().into_iter().next().cloned())
            .unwrap();
        let extras = dist_extra_features(&pkg).unwrap();
        assert!(
            extras.contains(&"channel-matrix".to_string()),
            "distribution extras come from Cargo.toml registry"
        );
        assert!(extras.contains(&"channel-lark".to_string()));
        assert!(extras.contains(&"whatsapp-web".to_string()));
    }

    #[test]
    fn required_registry_list_rejects_malformed_metadata() {
        assert!(parse_required_registry_list(None, "dist_extra_features").is_err());
        let wrong_type = serde_json::json!({ "dist_extra_features": "channel-lark" });
        assert!(parse_required_registry_list(Some(&wrong_type), "dist_extra_features").is_err());
        let duplicate = serde_json::json!({
            "dist_extra_features": ["channel-lark", "channel-lark"]
        });
        assert!(parse_required_registry_list(Some(&duplicate), "dist_extra_features").is_err());
    }

    #[test]
    fn target_exclusion_registry_rejects_malformed_metadata() {
        assert!(parse_target_exclusions(None).is_err());
        for malformed in [
            serde_json::json!({ "dist_target_exclusions": [] }),
            serde_json::json!({ "dist_target_exclusions": {} }),
            serde_json::json!({ "dist_target_exclusions": { "target": [] } }),
            serde_json::json!({ "dist_target_exclusions": { "target": [1] } }),
            serde_json::json!({
                "dist_target_exclusions": { "target": ["feature", "feature"] }
            }),
        ] {
            assert!(parse_target_exclusions(Some(&malformed)).is_err());
        }
    }

    #[test]
    fn release_exclusions_remove_only_named_features() {
        let dist = resolve_feature_list(&root(), &Selection::Dist).unwrap();
        let meta = cargo_metadata::MetadataCommand::new()
            .manifest_path(root().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        let exclusions = dist_target_exclusions(workspace_root_package(&meta).unwrap()).unwrap();

        for selection in [Selection::Dist, Selection::DistBroad] {
            let unfiltered = resolve_feature_list(&root(), &selection).unwrap();
            for (target, excluded) in &exclusions {
                let resolved =
                    resolve_feature_list_for_target(&root(), &selection, Some(target)).unwrap();
                assert_eq!(resolved.len(), unfiltered.len() - excluded.len());
                for feature in &unfiltered {
                    assert_eq!(
                        resolved.contains(feature),
                        !excluded.contains(feature),
                        "unexpected resolution for {target}: {feature}"
                    );
                }
            }
        }

        let host = resolve_feature_list_for_target(
            &root(),
            &Selection::Dist,
            Some("aarch64-apple-darwin"),
        )
        .unwrap();
        assert_eq!(host, dist);

        assert!(exclude_features(host, &["channel-slack".to_owned()]).is_err());
        let (target, excluded) = exclusions.iter().next().unwrap();
        let resolved =
            resolve_feature_list_for_target(&root(), &Selection::Dist, Some(target)).unwrap();
        assert!(exclude_features(resolved, &[excluded[0].clone()]).is_err());
    }

    #[test]
    fn dist_target_exclusions_are_declared_in_registry() {
        let meta = cargo_metadata::MetadataCommand::new()
            .manifest_path(root().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        let exclusions = dist_target_exclusions(workspace_root_package(&meta).unwrap()).unwrap();

        assert_eq!(exclusions["aarch64-linux-android"], vec!["whatsapp-web"]);
        assert_eq!(
            exclusions["arm-unknown-linux-gnueabihf"],
            vec!["observability-prometheus"]
        );
        assert_eq!(
            exclusions["armv7-unknown-linux-gnueabihf"],
            vec!["observability-prometheus"]
        );
    }

    #[test]
    fn release_workflows_delegate_target_policy_to_generator() {
        let meta = cargo_metadata::MetadataCommand::new()
            .manifest_path(root().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        let exclusions = dist_target_exclusions(workspace_root_package(&meta).unwrap()).unwrap();
        let release =
            std::fs::read_to_string(root().join(".github/workflows/release-stable-manual.yml"))
                .unwrap();
        assert!(release.contains("features --selection dist --target \"${{ matrix.target }}\""));
        assert!(!release.contains("excluded_features"));

        let manual = std::fs::read_to_string(
            root().join(".github/workflows/cross-platform-build-manual.yml"),
        )
        .unwrap();
        assert!(manual.contains("distribution:"));
        assert!(
            manual.contains(
                "default: dist\n        options:\n          - dist\n          - dist-broad"
            )
        );
        assert!(
            manual.contains(
                "timeout-minutes: ${{ inputs.distribution == 'dist-broad' && 90 || 40 }}"
            )
        );
        assert!(manual.contains("--selection \"${{ inputs.distribution }}\""));
        assert!(manual.contains("echo \"- Binary bytes: $bytes\""));
        assert!(manual.contains("echo \"- Resolved features: \\`$FEATURES\\`\""));
        assert_eq!(
            manual
                .matches("if: matrix.target != 'aarch64-linux-android'")
                .count(),
            2,
            "both the ZeroCode build and upload must skip Android"
        );
        assert!(!manual.contains("excluded_features"));

        let target_env = manual.find("- name: Configure target environment").unwrap();
        let release_step = manual.find("- name: Build release").unwrap();
        let zeroclaw_upload = manual
            .find("name: zeroclaw-manual-${{ inputs.distribution }}-${{ matrix.target }}")
            .unwrap();
        let companion = manual.find("- name: Build ZeroCode companion").unwrap();
        assert!(
            target_env < release_step
                && release_step < zeroclaw_upload
                && zeroclaw_upload < companion
        );
        assert!(
            manual[target_env..release_step]
                .contains("echo \"${{ matrix.linker_env }}=${{ matrix.linker }}\"")
        );
        assert!(manual[target_env..release_step].contains(">> \"$GITHUB_ENV\""));
        assert!(!manual[release_step..companion].contains("matrix.linker_env"));

        for feature in exclusions.values().flatten() {
            assert!(!release.contains(feature));
            assert!(!manual.contains(feature));
        }
    }

    #[test]
    fn plan_diverges_and_converges() {
        let p = Plan::build(&root(), Platform::Unix, &Selection::Full).unwrap();
        // Both branches exist and differ (divergence is structural, not a flag).
        assert!(!p.diverge.prebuilt.is_empty());
        assert!(!p.diverge.source.is_empty());
        let pb_ids: Vec<_> = p.diverge.prebuilt.iter().map(|s| s.id).collect();
        let src_ids: Vec<_> = p.diverge.source.iter().map(|s| s.id).collect();
        assert!(pb_ids.contains(&"download-prebuilt"));
        assert!(src_ids.contains(&"cargo-install-self"));
        assert!(
            !pb_ids.contains(&"cargo-install-self"),
            "branches must not bleed"
        );
        // Convergence: shared tail runs after either branch.
        let tail: Vec<_> = p.converge.iter().map(|s| s.id).collect();
        assert!(tail.contains(&"add-to-path"), "branches reconverge at PATH");
    }

    #[test]
    fn dry_run_coverage_is_total() {
        // validate() already ran in build(); assert every step narrates so
        // the dry-run pass can describe the whole plan with no silent mutations.
        let p = Plan::build(&root(), Platform::Windows, &Selection::Full).unwrap();
        for s in p.all_steps() {
            let empty = matches!(s.narration, Value::Lit(ref l) if l.is_empty());
            assert!(!empty, "step {} has no dry-run narration", s.id);
        }
    }

    #[test]
    fn dry_run_line_prefixes_uniform_would() {
        assert_eq!(
            dry_run_line("build the web dashboard"),
            "[dry-run] Would build the web dashboard"
        );
    }

    #[test]
    fn web_data_dir_expr_matches_data_local_dir_semantics() {
        let win = web_data_dir_expr(Platform::Windows);
        assert!(win.contains("LOCALAPPDATA") && win.ends_with("zeroclaw\\web\\dist"));
        let unix = web_data_dir_expr(Platform::Unix);
        assert!(unix.contains("XDG_DATA_HOME") && unix.ends_with("zeroclaw/web/dist"));
    }
}
