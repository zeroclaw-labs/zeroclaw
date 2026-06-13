//! Canonical install spec. install.sh@HEAD is the behavioral reference; this
//! spec reproduces it and every surface (install.sh, setup.bat, ...) renders
//! from it. Dry-run is intrinsic: a step renders its `narration` in
//! `Mode::DryRun` and performs its `action` in `Mode::Execute`. Nothing is
//! hardcoded in a surface; values flow from `Cargo.toml` and the resolver.

use std::path::Path;

/// Platforms a step applies to. Renderers skip steps not in their set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Unix,
    Windows,
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

/// Execution mode for a plan. This is the structured home of the dry-run
/// concept: a plan is rendered (or executed) in exactly one mode, and the mode
/// (not a per-step flag or a vec convention) decides whether each step emits
/// its narration or performs its action.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Perform each step's `action`.
    Execute,
    /// Emit each step's narration as a `[dry-run] Would ...` line; perform
    /// nothing. Every step that has an action MUST have a narration, so dry-run
    /// coverage is total by construction (enforced by `Plan::validate`).
    DryRun,
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
    /// narration, so `Mode::DryRun` describes the entire plan with no silent
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

/// What a step emits in `Mode::DryRun`: the narration prefixed so users see it
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

/// Meta/aggregate features that are not user-selectable rows. This bit is not
/// derivable from the feature graph, so it lives in the canonical registry at
/// `[package.metadata.zeroclaw] non_row_features` in Cargo.toml. Read from
/// there via `cargo_metadata`; never shadowed by a literal list here or in any
/// surface.
pub fn non_row_features(
    meta: &cargo_metadata::Metadata,
    pkg: &cargo_metadata::Package,
) -> Vec<String> {
    let _ = meta;
    read_registry_list(pkg, "non_row_features")
}

/// Heavyweight non-channel features excluded from `Selection::Dist`. Read from
/// `[package.metadata.zeroclaw] heavyweight_features`; the non-derivable "too
/// big for the default download" bit, in the registry, never shadowed.
pub fn heavyweight_features(pkg: &cargo_metadata::Package) -> Vec<String> {
    read_registry_list(pkg, "heavyweight_features")
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
    let root = meta
        .root_package()
        .cloned()
        .or_else(|| meta.workspace_packages().into_iter().next().cloned())
        .ok_or_else(|| anyhow::Error::msg("no root/workspace package"))?;
    let all_features: Vec<String> = root.features.keys().cloned().collect();
    let non_row = non_row_features(&meta, &root);
    let heavyweight = heavyweight_features(&root);
    let ctx = FeatureCtx {
        graph: &root.features,
        all: &all_features,
        non_row: &non_row,
        heavyweight: &heavyweight,
    };
    selection.to_feature_list(&ctx)
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
    let heavyweight = heavyweight_features(root);
    let default_features = expand_default(&root.features, &non_row);

    let ctx = FeatureCtx {
        graph: &root.features,
        all: &all_features,
        non_row: &non_row,
        heavyweight: &heavyweight,
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
    /// Full default feature set (install.sh `--preset full`).
    Full,
    /// Kernel only (`--no-default-features`).
    Minimal,
    /// Default binary distribution: all channels (`channels-full`) plus the
    /// default runtime, minus registry-flagged heavyweight features. The set a
    /// single-artifact package manager ships.
    Dist,
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
            Selection::Dist => "all channels, no heavyweight extras (recommended)",
            Selection::All => "every feature including hardware and browser",
            Selection::Features(_) => "custom feature selection",
        }
    }

    /// The selections a packaged/menu surface offers, in menu order. Single
    /// source for "which build modes exist"; surfaces walk this, they do not
    /// enumerate modes themselves.
    pub fn menu() -> Vec<Selection> {
        vec![
            Selection::Minimal,
            Selection::Dist,
            Selection::Full,
            Selection::All,
        ]
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
            Selection::Dist => {
                let mut s = ctx.expand("channels-full");
                s.extend(ctx.expand("default"));
                s.retain(|f| !ctx.heavyweight.contains(f));
                s
            }
            Selection::All => ctx
                .all
                .iter()
                .filter(|f| !ctx.non_row.contains(f) && !ctx.is_alias(f))
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
    pub heavyweight: &'a [String],
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

/// The web/dist path EXPRESSION each surface emits - resolved on the END
/// USER's machine at install time, never baked to the generator host. A test
/// asserts these expressions agree with `directories::BaseDirs::data_local_dir`
/// so the rendered path matches the gateway's runtime auto-detect.
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
    fn dist_has_all_channels_minus_heavyweight() {
        let r = resolve(&root(), &Selection::Dist).unwrap();
        // All channels: a representative non-default channel from channels-full.
        assert!(
            r.cargo_flags.contains("channel-discord"),
            "dist must ship all channels"
        );
        assert!(r.cargo_flags.contains("channel-slack"));
        // Heavyweight excluded.
        assert!(
            !r.cargo_flags.contains("hardware"),
            "dist must exclude heavyweight hardware"
        );
        assert!(!r.cargo_flags.contains("browser-native"));
        // Default runtime retained.
        assert!(r.cargo_flags.contains("gateway"));
    }

    #[test]
    fn all_is_superset_of_dist() {
        let dist = resolve(&root(), &Selection::Dist).unwrap();
        let all = resolve(&root(), &Selection::All).unwrap();
        // All includes heavyweight that Dist drops.
        assert!(
            all.cargo_flags.contains("hardware"),
            "all is the kitchen sink"
        );
        assert!(dist.cargo_flags.len() < all.cargo_flags.len());
    }

    #[test]
    fn heavyweight_read_from_registry() {
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
        let hw = heavyweight_features(&pkg);
        assert!(
            hw.contains(&"hardware".to_string()),
            "heavyweight from Cargo.toml registry"
        );
        assert!(!hw.is_empty());
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
        // Mode::DryRun can describe the whole plan with no silent mutations.
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
    fn web_data_dir_expr_matches_directories_runtime() {
        let win = web_data_dir_expr(Platform::Windows);
        assert!(win.contains("LOCALAPPDATA") && win.ends_with("zeroclaw\\web\\dist"));
        let unix = web_data_dir_expr(Platform::Unix);
        assert!(unix.contains("XDG_DATA_HOME") && unix.ends_with("zeroclaw/web/dist"));
    }
}
