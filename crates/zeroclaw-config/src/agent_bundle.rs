//! Agent bundle export planning.
//!
//! An *agent bundle* is a portable directory describing one agent:
//!
//! ```text
//! <bundle>/
//!   zeroclaw-agent.toml   — manifest (format version, root alias, provenance,
//!                            required secrets, carried skill bundles,
//!                            dropped refs, risk flags)
//!   config.toml           — the config closure this agent needs
//!   workspace/            — the agent's workspace tree
//!   skills/<alias>/       — content of each referenced skill bundle, which
//!                            lives outside the workspace on the source host
//! ```
//!
//! This module owns the *planning* half: given a live [`Config`] and an agent
//! alias, it computes the transitive config closure, strips credentials and
//! host-specific state, and reports what an operator on the receiving end
//! would be accepting. It performs no I/O — materializing the plan onto disk
//! is the caller's job — which keeps the security-relevant logic unit-testable
//! without a filesystem.
//!
//! The closure is deliberately narrow. An agent entry references providers,
//! profiles, bundles, MCP servers, channels, cron jobs and sibling agents; only
//! the subset that can be meaningfully reconstituted on a different install is
//! carried, and everything omitted is reported in [`ExportPlan::dropped`]
//! rather than silently disappearing.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Serialize;

use crate::multi_agent::AccessMode;
use crate::schema::{
    AliasedAgentConfig, Config, KnowledgeBundleConfig, McpBundleConfig, McpServerConfig,
    McpTransport, RiskProfileConfig, RuntimeProfileConfig, SkillBundleConfig,
};
use crate::traits::{MaskSecrets, is_masked_secret};

/// Bundle format version written to (and expected in) the manifest.
pub const BUNDLE_FORMAT_VERSION: u32 = 1;

/// Manifest filename inside a bundle directory.
pub const MANIFEST_FILE: &str = "zeroclaw-agent.toml";

/// Config-fragment filename inside a bundle directory.
pub const CONFIG_FILE: &str = "config.toml";

/// Workspace directory name inside a bundle directory.
pub const WORKSPACE_DIR: &str = "workspace";

/// Directory inside a bundle holding carried skill-bundle content, one
/// subdirectory per skill-bundle alias. Skills live under the install-wide
/// `<install>/shared/skills/` tree rather than the agent's workspace, so
/// carrying the config alone would import an agent whose skills are missing.
pub const SKILLS_DIR: &str = "skills";

/// Workspace subdirectory holding the agent's private memory store. Never
/// carried by bundle format 1 — see [`plan_export`].
pub const WORKSPACE_MEMORY_DIR: &str = "memory";

/// The memory snapshot at the workspace root, excluded alongside the store.
/// Re-exported from [`crate::paths`], which owns the name.
pub use crate::paths::MEMORY_SNAPSHOT_FILE;

/// Config-value prefixes produced by [`crate::secrets::SecretStore`]. A value
/// carrying one of these has escaped scrubbing and must never reach a bundle.
const CIPHERTEXT_PREFIXES: &[&str] = &["enc:", "enc2:"];

/// Why a piece of the agent's configuration was left out of the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Names accounts, addresses, or paths that only exist on the source host.
    HostSpecific,
    /// Names other agents or groups that will not exist on the target install.
    Relational,
    /// An outward-facing surface that must be re-enabled deliberately on the
    /// target rather than arriving switched on.
    DefaultClosed,
    /// The agent's private data, excluded unless explicitly requested.
    PrivateData,
    /// Not carried by this bundle format version yet.
    NotYetPortable,
    /// Referenced content the exporting host did not have, so there was
    /// nothing to carry. Recorded by the caller once the copy has run.
    SourceMissing,
}

impl DropReason {
    /// Stable wire string used in the manifest.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::HostSpecific => "host_specific",
            Self::Relational => "relational",
            Self::DefaultClosed => "default_closed",
            Self::PrivateData => "private_data",
            Self::NotYetPortable => "not_yet_portable",
            Self::SourceMissing => "source_missing",
        }
    }
}

/// One omitted piece of configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedRef {
    /// Dotted config path that was dropped, relative to the whole config.
    pub path: String,
    pub reason: DropReason,
    /// Operator-facing explanation.
    pub detail: String,
}

/// A capability in the bundle that widens the target install's trust boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskKind {
    /// Risk profile runs at `level = "full"` — no per-operation approval gate.
    FullAutonomy,
    /// The agent may read or write outside its workspace.
    FilesystemEscape,
    /// Sandboxing is explicitly disabled for this profile.
    SandboxDisabled,
    /// Approval gates that normally catch risky operations are switched off.
    ApprovalBypass,
    /// Host environment variables are forwarded into shell subprocesses.
    EnvPassthrough,
    /// Extra filesystem roots are granted beyond the workspace.
    ExtraFilesystemRoots,
    /// The agent may initiate delegation to other agents.
    DelegationEnabled,
    /// A stdio MCP server spawns a local process on the target host.
    ProcessSpawn,
    /// Server-controlled text is injected into the system prompt at startup.
    UntrustedStartupContext,
    /// The bundle carries skill content authored on the exporting install,
    /// which the agent reads as instructions once installed.
    CarriedSkills,
}

impl RiskKind {
    /// Stable wire string used in the manifest.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::FullAutonomy => "full_autonomy",
            Self::FilesystemEscape => "filesystem_escape",
            Self::SandboxDisabled => "sandbox_disabled",
            Self::ApprovalBypass => "approval_bypass",
            Self::EnvPassthrough => "env_passthrough",
            Self::ExtraFilesystemRoots => "extra_filesystem_roots",
            Self::DelegationEnabled => "delegation_enabled",
            Self::ProcessSpawn => "process_spawn",
            Self::UntrustedStartupContext => "untrusted_startup_context",
            Self::CarriedSkills => "carried_skills",
        }
    }
}

/// One flagged capability, bound to the config path that grants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskFlag {
    pub kind: RiskKind,
    /// Dotted config path within the bundle that carries the capability.
    pub path: String,
    /// Operator-facing explanation of what accepting this grants.
    pub detail: String,
}

/// Provenance recorded in the manifest. Supplied by the caller so the planner
/// stays a pure function of the config.
#[derive(Debug, Clone)]
pub struct Provenance {
    /// Version of the ZeroClaw binary that produced the bundle.
    pub zeroclaw_version: String,
    /// RFC 3339 timestamp of the export.
    pub exported_at: String,
}

/// One skill bundle's content to carry, resolved to a source directory.
///
/// The planner performs no I/O, so this names the directory and the filter
/// rather than the skills themselves; the caller enumerates and copies.
#[derive(Debug, Clone)]
pub struct SkillBundleSource {
    /// Bundle alias. Also the subdirectory name under [`SKILLS_DIR`], which is
    /// what lets an import map content back onto the target's own resolved
    /// directory for the same alias.
    pub alias: String,
    /// Absolute source directory on the exporting host, for messages and
    /// overlap checks.
    pub source: PathBuf,
    /// The same directory as a lexically-normalized path relative to the
    /// install root. Always under `shared/`: the planner validates the
    /// skill-bundle directory contract before planning a source. The copy
    /// opens the directory by walking these components no-follow from the
    /// install root, so the path the planner validated is the path the copy
    /// proves.
    pub relative: PathBuf,
    /// The bundle's own include/exclude filter. Applied to the copy so a skill
    /// this bundle excludes does not travel: [`SkillBundleConfig::admits_skill`]
    /// is the same filter the runtime applies.
    pub filter: SkillBundleConfig,
}

/// How the copy reaches each kind of source, and what that proves.
///
/// An anchored source is named relative to the install root and must be
/// opened by walking those components without following a symlink at any of
/// them. A canonicalize-then-compare check cannot give that guarantee twice
/// over: a symlinked `shared/skills` or `agents` that stays *inside* the
/// install passes the comparison while moving the source out of the subtree
/// its contract names, and the object the check resolved is not necessarily
/// the object a later by-path open finds. The install root itself is the
/// operator's configured location — like the config file it holds — so
/// symlinks above it are the operator's own and are followed.
#[derive(Debug, Clone)]
pub struct SourceBoundaries {
    /// The trusted install root every anchored source is walked from.
    pub install_root: PathBuf,
    /// Install-relative path of the agent's workspace when it uses the
    /// default location (`agents/<alias>/workspace`). `None` when
    /// `workspace.path` places it deliberately, since then the configured
    /// location is the operator's own boundary and [`ExportPlan`]'s
    /// `workspace_source` is opened as configured.
    pub workspace: Option<PathBuf>,
}

/// A computed export, ready to be written to disk.
#[derive(Debug, Clone)]
pub struct ExportPlan {
    /// Alias of the agent this bundle describes.
    pub root_alias: String,
    /// The config closure, as a TOML table rooted at the config root.
    pub config: toml::Table,
    /// Dotted paths whose values were scrubbed and must be supplied on import.
    pub required_secrets: Vec<String>,
    /// Configuration deliberately left out, with reasons.
    pub dropped: Vec<DroppedRef>,
    /// Capabilities the receiving operator is being asked to grant.
    pub risk_flags: Vec<RiskFlag>,
    /// Source workspace directory to copy into the bundle.
    pub workspace_source: PathBuf,
    /// Skill-bundle directories to copy into the bundle's `skills/` tree.
    pub skill_sources: Vec<SkillBundleSource>,
    /// Boundary each copy source must resolve inside once symlinks anywhere in
    /// its path are followed. `None` means the operator pointed the source
    /// somewhere deliberately (a configured `workspace.path`), so the
    /// configured location *is* the boundary and there is nothing to compare
    /// against.
    pub source_boundaries: SourceBoundaries,
    /// Workspace-relative path of the AIEOS identity document the closure
    /// still references, if it kept one. The caller checks that the workspace
    /// copy actually carried it: a reference the bundle cannot satisfy is
    /// dropped rather than shipped.
    pub identity_document: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(
        "agent '{0}' is not configured; run `zeroclaw agents list` to see the configured aliases"
    )]
    UnknownAgent(String),

    #[error("failed to serialize configuration for export: {0}")]
    Serialize(String),

    #[error(
        "refusing to write the bundle: encrypted config ciphertext survived scrubbing at `{path}`. \
         This is a bug in the export scrubber — please report it rather than working around it."
    )]
    UnscrubbedSecret { path: String },

    #[error(
        "the closure for agent '{alias}' does not validate on a clean install: {error}\n\
         A bundle has to stand on its own, so this one is not written. Fix the reported \
         field in this install's config and export again."
    )]
    ClosureInvalid { alias: String, error: String },
}

/// Compute the export closure for `alias`.
///
/// Fails only on an unknown alias or a serialization fault. A source config
/// with dangling references plans without complaint — the unresolvable
/// entries are simply absent from the closure — and is caught when the
/// closure is rendered, which is the last point at which its final content is
/// known. See [`render_config_toml`].
pub fn plan_export(config: &Config, alias: &str) -> Result<ExportPlan, ExportError> {
    let agent = config
        .agents
        .get(alias)
        .ok_or_else(|| ExportError::UnknownAgent(alias.to_string()))?;

    // Mask before selecting: `mask_secrets` is the schema's own definition of
    // which leaves are credentials (including dynamically-keyed maps like an
    // MCP server's `env`), so the selection below can never pick up a secret
    // the schema knows about.
    let mut masked = config.clone();
    masked.mask_secrets();
    let masked_root = to_table(&masked)?;

    let mut out = toml::Table::new();
    let mut dropped = Vec::new();

    // ── the agent entry itself ───────────────────────────────────────────
    let mut identity_document = None;
    let agent_table = sanitize_agent(
        &to_table(agent)?,
        alias,
        agent,
        &mut dropped,
        &mut identity_document,
    );
    let agent_table = prune(agent_table, &to_table(&AliasedAgentConfig::default())?);
    insert_at(
        &mut out,
        &["agents", alias],
        toml::Value::Table(agent_table),
    );

    // ── profiles ─────────────────────────────────────────────────────────
    let risk_alias = agent.risk_profile.trim();
    if !risk_alias.is_empty() {
        carry(
            &mut out,
            &masked_root,
            &["risk_profiles", risk_alias],
            &to_table(&RiskProfileConfig::default())?,
        );
    }
    let runtime_alias = agent.runtime_profile.trim();
    if !runtime_alias.is_empty() {
        carry(
            &mut out,
            &masked_root,
            &["runtime_profiles", runtime_alias],
            &to_table(&RuntimeProfileConfig::default())?,
        );
    }

    // ── bundles ──────────────────────────────────────────────────────────
    //
    // A skill bundle is two halves: the config entry, and the skills on disk
    // under `<install>/shared/skills/`. Carrying only the config would import
    // an agent whose skills silently do not exist, so the directory travels
    // too — see `skill_sources`.
    let skill_default = to_table(&SkillBundleConfig::default())?;
    let install_root = config.install_root_dir();
    let mut skill_sources = Vec::new();
    for bundle in dedup(&agent.skill_bundles) {
        carry(
            &mut out,
            &masked_root,
            &["skill_bundles", &bundle],
            &skill_default,
        );
        let Some(configured) = config.skill_bundles.get(&bundle) else {
            // A dangling bundle reference grants nothing and carries nothing;
            // the import-side `Config::validate()` reports it.
            continue;
        };
        // An absolute `directory` names this host. Worse, it would fail the
        // target's own validation, which requires bundle directories to sit
        // inside *its* `<install>/shared/`. Dropping it lets the target
        // resolve its default location for the alias, which is where the
        // carried content is mapped back to.
        if configured
            .directory
            .as_deref()
            .map(str::trim)
            .is_some_and(|dir| !dir.is_empty() && std::path::Path::new(dir).is_absolute())
            && let Some(table) = table_at_mut(&mut out, &["skill_bundles", &bundle])
        {
            table.remove("directory");
            dropped.push(DroppedRef {
                path: format!("skill_bundles.{bundle}.directory"),
                reason: DropReason::HostSpecific,
                detail: "the bundle directory is a source-host absolute path, and the target \
                         requires bundle directories inside its own install; the imported \
                         bundle uses the target's default location for this alias"
                    .to_string(),
            });
        }
        if let Some(reason) = unportable_component(&bundle) {
            // The alias names a directory inside the bundle. One that is not a
            // single component would place carried content outside the bundle
            // being built, and one a supported target cannot create would make
            // the bundle unopenable there, so nothing is planned for it.
            dropped.push(DroppedRef {
                path: format!("skill_bundles.{bundle}"),
                reason: DropReason::HostSpecific,
                detail: format!(
                    "the skill-bundle alias {reason}, so it is not a directory name every \
                     target can create for the bundle's content; rename the bundle to a \
                     plain name"
                ),
            });
            continue;
        }
        let Ok(source) = crate::skill_bundles::resolve_directory(config, &install_root, &bundle)
        else {
            continue;
        };
        // The planner enforces the skill-bundle directory contract itself
        // rather than assuming its `Config` was validated: content is only
        // planned from where `Config::validate` allows a bundle directory to
        // sit. A directory outside `<install>/shared/` is not a tree the copy
        // can walk from the install root, so nothing is planned for it and
        // the operator hears that the content stayed behind.
        //
        // A `..` component is refused even when it lexically stays inside
        // `shared/`: the runtime resolves the configured path through the
        // filesystem, where a symlink before the `..` lands somewhere the
        // lexical form does not name — the export must not carry a tree the
        // agent does not actually load from.
        let escapes = crate::skill_bundles::validate_directory(&source, &install_root).is_err()
            || configured.directory.as_deref().is_some_and(|dir| {
                std::path::Path::new(dir)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            });
        if escapes {
            if let Some(table) = table_at_mut(&mut out, &["skill_bundles", &bundle]) {
                table.remove("directory");
            }
            dropped.push(DroppedRef {
                path: format!("{SKILLS_DIR}/{bundle}"),
                reason: DropReason::HostSpecific,
                detail: "the bundle's directory is not a plain path inside the install's \
                         shared tree, so its content is not the install's to export and is \
                         not carried; its config travels without the directory, and the \
                         skills must be installed at the target's default location for this \
                         alias"
                    .to_string(),
            });
            continue;
        }
        let normalized = crate::paths::normalize_lexical(&source);
        let Ok(relative) = normalized.strip_prefix(crate::paths::normalize_lexical(&install_root))
        else {
            // Validation above proved the directory sits under
            // `<install>/shared/`, which is under the install root, so the
            // prefix strips. Kept as a refusal rather than an unwrap so a
            // future validator change fails closed here.
            continue;
        };
        skill_sources.push(SkillBundleSource {
            alias: bundle.clone(),
            relative: relative.to_path_buf(),
            source: normalized,
            filter: configured.clone(),
        });
    }
    let knowledge_default = to_table(&KnowledgeBundleConfig::default())?;
    for bundle in dedup(&agent.knowledge_bundles) {
        carry(
            &mut out,
            &masked_root,
            &["knowledge_bundles", &bundle],
            &knowledge_default,
        );
    }
    let mcp_bundle_default = to_table(&McpBundleConfig::default())?;
    for bundle in dedup(&agent.mcp_bundles) {
        carry(
            &mut out,
            &masked_root,
            &["mcp_bundles", &bundle],
            &mcp_bundle_default,
        );
    }

    // ── MCP servers the bundles actually grant ───────────────────────────
    //
    // Resolution goes through `mcp_servers_for_bundles` rather than a manual
    // union so `exclude` keeps winning here exactly as it does at runtime: the
    // bundle carries the servers this agent can really reach, no more.
    //
    // INVARIANT: a server name addresses exactly one entry in the closure.
    // The manifest's `required_secrets` paths are addressed by that name
    // (`mcp.servers.github.env.GITHUB_TOKEN`), and the promise that they can be
    // pasted into `zeroclaw config set` holds only while it is true. Two things
    // hold it up: `Config::validate` rejects duplicate `mcp.servers` names, and
    // `mcp_servers_for_bundles` resolves each granted name to the first
    // matching entry, so even a config hand-edited past validation collapses to
    // one entry per name here. `duplicate_server_names_collapse_to_one_entry`
    // pins the second half, which is the one this module owns.
    let granted = config.mcp_servers_for_bundles(&agent.mcp_bundles);
    debug_assert_eq!(
        granted
            .iter()
            .map(|s| &s.name)
            .collect::<BTreeSet<_>>()
            .len(),
        granted.len(),
        "mcp.servers names must be unique in the closure: required_secrets paths key on them"
    );
    if !granted.is_empty() {
        let server_default = to_table(&McpServerConfig::default())?;
        let mut servers = Vec::with_capacity(granted.len());
        for server in &granted {
            let table = masked_server_table(&masked_root, &server.name, server)?;
            let mut pruned = prune(table, &server_default);
            // `name` is the natural key every other surface addresses the
            // server by, so it stays even when pruning would drop it.
            pruned.insert("name".to_string(), toml::Value::String(server.name.clone()));
            servers.push(toml::Value::Table(pruned));
        }
        insert_at(&mut out, &["mcp", "servers"], toml::Value::Array(servers));
    }

    // ── provider entries, carried keyless ────────────────────────────────
    //
    // Every provider reference the agent names is carried, not just
    // `model_provider`: `Config::validate()` fails loud on any dangling
    // provider ref, so a bundle that carried only some of them would not
    // import. Credentials are scrubbed below, leaving keyless entries for the
    // operator to fill in.
    let mut provider_refs: Vec<(&str, &str)> = vec![
        ("providers.models", agent.model_provider.trim()),
        ("providers.models", agent.classifier_provider.trim()),
        ("providers.models", agent.summary_provider.trim()),
        ("providers.tts", agent.tts_provider.trim()),
        (
            "providers.transcription",
            agent.transcription_provider.trim(),
        ),
    ];
    // A carried runtime profile can name a summarizer provider of its own,
    // independently of the agent. `Config::validate()` checks that reference
    // per profile, so a closure carrying the profile without the provider
    // fails on the target even though the agent never named it.
    if let Some(profile) = config.runtime_profiles.get(runtime_alias) {
        provider_refs.push((
            "providers.models",
            profile.context_compression.summary_provider.trim(),
        ));
    }
    let mut carried_providers: BTreeSet<String> = BTreeSet::new();
    for (section, reference) in provider_refs {
        let Some((family, entry)) = split_provider_ref(reference) else {
            continue;
        };
        let path = format!("{section}.{family}.{entry}");
        if !carried_providers.insert(path) {
            continue;
        }
        let mut segments: Vec<&str> = section.split('.').collect();
        segments.push(family);
        segments.push(entry);
        // Provider entries are pruned against an empty default: the typed
        // default varies per family, and an over-pruned provider entry is
        // worse than a verbose one (a dropped `api_url` silently re-points
        // the agent at the family's built-in endpoint).
        carry(&mut out, &masked_root, &segments, &toml::Table::new());
    }

    // ── scrub, then verify the scrub ─────────────────────────────────────
    let mut required_secrets = Vec::new();
    let mut root = toml::Value::Table(out);
    visit_strings_mut(&mut root, "", &mut |path, value| {
        if is_masked_secret(value) {
            required_secrets.push(path.to_string());
            value.clear();
        }
    });
    if let Some(path) = find_ciphertext(&root, "") {
        return Err(ExportError::UnscrubbedSecret { path });
    }
    let toml::Value::Table(config_table) = root else {
        return Err(ExportError::Serialize(
            "export closure is not a TOML table".to_string(),
        ));
    };
    required_secrets.sort();
    required_secrets.dedup();

    // Memory does not travel in format 1, in either of the forms it takes on
    // disk. The store is a live SQLite database in WAL mode: copying its files
    // while the agent is running can capture a torn or stale database, and a
    // bundle is not a place to discover that. Carrying it needs a real
    // snapshot boundary, which a later format version can add.
    dropped.push(DroppedRef {
        path: format!("{WORKSPACE_DIR}/{WORKSPACE_MEMORY_DIR}"),
        reason: DropReason::NotYetPortable,
        detail: "the memory store is a live SQLite database in WAL mode, which cannot be \
                 copied as files without risking a torn or stale snapshot; bundle format 1 \
                 does not carry it"
            .to_string(),
    });
    dropped.push(DroppedRef {
        path: format!("{WORKSPACE_DIR}/{MEMORY_SNAPSHOT_FILE}"),
        reason: DropReason::PrivateData,
        detail: "the memory snapshot is an export of the agent's core memories, which an \
                 agent re-hydrates its store from; it stays with the store it came from"
            .to_string(),
    });

    let risk_flags = collect_risk_flags(config, alias, agent, &granted, &skill_sources);

    Ok(ExportPlan {
        root_alias: alias.to_string(),
        config: config_table,
        required_secrets,
        dropped,
        risk_flags,
        workspace_source: config.agent_workspace_dir(alias),
        skill_sources,
        source_boundaries: SourceBoundaries {
            // A configured `workspace.path` is the operator saying where the
            // workspace lives; only the default location is anchored to the
            // install. The relative form is derived from the same resolver
            // the runtime uses, so the subtree the copy walks is the subtree
            // the agent actually runs in.
            workspace: agent.workspace.path.is_none().then(|| {
                // The strip cannot fail for a path the resolver built from
                // the install root; an empty fallback fails the copy's
                // `agents/` subtree check rather than opening anything.
                crate::paths::normalize_lexical(&config.agent_workspace_dir(alias))
                    .strip_prefix(crate::paths::normalize_lexical(&install_root))
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default()
            }),
            install_root,
        },
        identity_document,
    })
}

/// Record that `missing` skill bundles contributed no content to the bundle.
///
/// The planner runs before any filesystem work, so it plans to carry every
/// referenced bundle. Only the caller learns which of them actually had
/// content, and the manifest is rendered from this plan: without folding that
/// back in, a bundle would advertise `skill_bundles` and a `carried_skills`
/// grant for a `skills/<alias>/` tree it does not contain, and the only
/// accurate record would be the terminal the export was run from.
///
/// Call before [`render_manifest_toml`].
pub fn record_missing_skill_content(plan: &mut ExportPlan, missing: &[String]) {
    for alias in missing {
        plan.skill_sources.retain(|source| &source.alias != alias);
        plan.risk_flags.retain(|flag| {
            flag.kind != RiskKind::CarriedSkills || flag.path != format!("skill_bundles.{alias}")
        });
        plan.dropped.push(DroppedRef {
            path: format!("{SKILLS_DIR}/{alias}"),
            reason: DropReason::SourceMissing,
            detail: "the skill bundle carried no skills from the exporting host: its \
                     directory is absent, or holds nothing the bundle admits. Its config \
                     travels, so install the skills on the target, or re-export from a host \
                     that has them"
                .to_string(),
        });
    }
}

/// Remove the parts of an agent entry that cannot travel, recording each.
///
/// Only fields that actually carry a value are reported, so the operator's
/// drop list describes this agent rather than the schema.
fn sanitize_agent(
    table: &toml::Table,
    alias: &str,
    agent: &AliasedAgentConfig,
    dropped: &mut Vec<DroppedRef>,
    identity_document: &mut Option<String>,
) -> toml::Table {
    let mut table = table.clone();

    let mut drop_key =
        |table: &mut toml::Table, key: &str, occupied: bool, reason, detail: &str| {
            table.remove(key);
            if occupied {
                dropped.push(DroppedRef {
                    path: format!("agents.{alias}.{key}"),
                    reason,
                    detail: detail.to_string(),
                });
            }
        };

    drop_key(
        &mut table,
        "channels",
        !agent.channels.is_empty(),
        DropReason::HostSpecific,
        "channel bindings name accounts and credentials that exist only on the source install; \
         bind the imported agent to a local channel instead",
    );
    drop_key(
        &mut table,
        "delegates",
        !agent.delegates.is_empty(),
        DropReason::Relational,
        "the delegate roster names sibling agents that will not exist on the target install",
    );
    drop_key(
        &mut table,
        "cron_jobs",
        !agent.cron_jobs.is_empty(),
        DropReason::NotYetPortable,
        "scheduled jobs are not carried by bundle format 1; re-create them on the target",
    );
    drop_key(
        &mut table,
        "a2a",
        agent.a2a.published || !agent.a2a.exposed_skills.is_empty(),
        DropReason::DefaultClosed,
        "A2A publication is an outward-facing surface; the imported agent arrives unpublished \
         and must be published deliberately",
    );

    // Dropping the explicit roster is not enough to close delegation reach:
    // `delegate_same_risk_profile` (default `true`) auto-allows delegation to
    // every agent sharing the carried risk profile, so an import would wire
    // the agent to whatever unrelated local agents happen to sit on it. The
    // bundle arrives with no reach, and the operator grants it deliberately.
    if agent.delegate_same_risk_profile {
        table.insert(
            "delegate_same_risk_profile".to_string(),
            toml::Value::Boolean(false),
        );
        dropped.push(DroppedRef {
            path: format!("agents.{alias}.delegate_same_risk_profile"),
            reason: DropReason::Relational,
            detail: "auto-delegation to same-profile peers would name agents on the target \
                     install that this agent has never been paired with; the imported agent \
                     arrives with no delegate reach"
                .to_string(),
        });
    }

    // The identity document travels only if it is part of the workspace the
    // bundle carries. The loader joins a relative `aieos_path` to the workspace
    // without rejecting `..`, so a path that escapes would have the imported
    // agent read outside its own workspace at startup.
    if let Some(raw) = agent.identity.aieos_path.as_deref() {
        match portable_identity_path(raw) {
            Ok(relative) => {
                // The carried config states the path in the bundle's own
                // grammar, not as the operator happened to spell it: one
                // string names the file in `config.toml`, in the copied tree,
                // and in the caller's carried-content check.
                if let Some(toml::Value::Table(identity)) = table.get_mut("identity") {
                    identity.insert(
                        "aieos_path".to_string(),
                        toml::Value::String(relative.clone()),
                    );
                }
                *identity_document = Some(relative);
            }
            Err(detail) => {
                if let Some(toml::Value::Table(identity)) = table.get_mut("identity") {
                    identity.remove("aieos_path");
                }
                dropped.push(DroppedRef {
                    path: format!("agents.{alias}.identity.aieos_path"),
                    reason: DropReason::HostSpecific,
                    detail,
                });
            }
        }
    }

    // The workspace block travels, but three of its fields cannot.
    if let Some(toml::Value::Table(workspace)) = table.get_mut("workspace") {
        workspace.remove("path");
        if agent.workspace.path.is_some() {
            dropped.push(DroppedRef {
                path: format!("agents.{alias}.workspace.path"),
                reason: DropReason::HostSpecific,
                detail: "the workspace override is a source-host absolute path; the imported \
                         agent uses the target's default workspace location"
                    .to_string(),
            });
        }
        workspace.remove("access");
        if !agent.workspace.access.is_empty() {
            dropped.push(DroppedRef {
                path: format!("agents.{alias}.workspace.access"),
                reason: DropReason::Relational,
                detail: "cross-agent filesystem grants name sibling agents that will not exist \
                         on the target install"
                    .to_string(),
            });
        }
        workspace.remove("read_memory_from");
        if !agent.workspace.read_memory_from.is_empty() {
            dropped.push(DroppedRef {
                path: format!("agents.{alias}.workspace.read_memory_from"),
                reason: DropReason::Relational,
                detail: "cross-agent memory grants name sibling agents that will not exist on \
                         the target install"
                    .to_string(),
            });
        }
    }

    table
}

/// Why `component` is not a name every supported target can materialize, or
/// `None` when it is one.
///
/// The rule itself is documented on [`is_safe_path_component`], the public
/// entry point. This form reports *which* rule a name broke, because the
/// answer is operator-facing: it becomes the detail on the dropped
/// reference, so a bundle that leaves a name behind says why.
fn unportable_component(component: &str) -> Option<&'static str> {
    if component.is_empty() {
        return Some("is empty");
    }
    if component != component.trim() {
        return Some("has leading or trailing whitespace");
    }
    if component == "." || component == ".." {
        return Some("names a relative directory rather than an entry");
    }
    if component.contains('/') || component.contains('\\') {
        return Some("contains a directory separator");
    }
    if component.contains([':', '<', '>', '"', '|', '?', '*']) {
        return Some("contains a character Windows does not allow in a filename");
    }
    if component.chars().any(char::is_control) {
        return Some("contains a control character");
    }
    if component.ends_with('.') {
        return Some("ends in a dot, which Windows strips to a different name");
    }
    if is_windows_device_name(component) {
        return Some("is a Windows device name");
    }
    None
}

/// Whether `component` names a Windows character device.
///
/// The device is matched on the stem, before the first dot: `nul.json` opens
/// the null device there exactly as `nul` does, so an extension is no escape.
fn is_windows_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    // `COM1`-`COM9` and `LPT1`-`LPT9`. `COM0` and `LPT0` are refused with
    // them rather than left to differ between Windows versions.
    let Some(port) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    port.len() == 1 && port.starts_with(|c: char| c.is_ascii_digit())
}

/// Whether `alias` is safe to use as one path component inside a bundle.
///
/// A skill-bundle alias is a config map key, and the bundle materializes its
/// content at `skills/<alias>/`. The skill-bundle validator constrains where a
/// bundle's *directory* may sit, not what its key may contain, so the key is
/// checked against the format's own grammar before it is ever joined to a
/// path.
///
/// That grammar governs the two kinds of name the bundle format controls: a
/// skill-bundle alias, and each component of a retained `aieos_path`. Both
/// are read by whoever opens the bundle rather than by the host that wrote
/// it, so the rule is the intersection of what the supported targets accept
/// rather than what this one happens to. Refused: an empty name, leading or
/// trailing whitespace, `.` and `..`, a directory separator in either
/// platform's reading, the characters `<>:"|?*`, control characters, a
/// trailing dot, and the Windows device names (`con`, `nul`, `lpt1` and
/// their kin, matched on the stem so an extension is no escape).
///
/// The last two are invisible from a Unix host and no less real. A name
/// ending in a space or a dot is silently stripped to a *different* name on
/// Windows, and a device name opens a device there, so a bundle carrying
/// either resolves somewhere its own manifest does not describe.
///
/// This governs the names the format itself controls. It is deliberately not
/// a filter over arbitrary workspace or skill content, which travels under
/// whatever names it already has.
#[must_use]
pub fn is_safe_path_component(alias: &str) -> bool {
    unportable_component(alias).is_none()
}

/// The workspace-relative form of an `aieos_path` the bundle can carry, or why
/// it cannot.
///
/// Portable means: relative, staying beneath the workspace once `..` is
/// resolved, and inside the part of the workspace the copy carries. Escape is
/// judged by [`crate::paths::resolve_under`], the same helper the scoped file
/// browser uses, against the workspace root the bundle recreates.
fn portable_identity_path(raw: &str) -> Result<String, String> {
    // The exporting host's `Path` semantics are not the importing host's. On
    // Unix a backslash is an ordinary filename character, so `..\\outside.json`
    // reads as one harmless name here and as a traversal on Windows, where the
    // loader joins it to the workspace. A bundle is judged by whoever opens it,
    // so the grammar below is the format's own: `/`-separated, no device or
    // host prefixes, no parent components, nothing a platform could read as a
    // separator or a control.
    // Judged before the components, so an absolute path is reported as one
    // rather than as an empty leading component. A leading `/` is checked
    // directly as well: Windows does not call it absolute, and the format
    // does not care which host is asking.
    if raw.is_empty() {
        return Err("the AIEOS identity document path is empty".to_string());
    }
    if raw.starts_with('/') || std::path::Path::new(raw).is_absolute() {
        return Err(
            "the AIEOS identity document is referenced by a source-host absolute path; \
                    point the imported agent at a path inside its own workspace"
                .to_string(),
        );
    }
    for segment in raw.split('/') {
        if segment == "." {
            // A no-op the normalizer removes before anything is joined. It
            // names nothing, so there is no name here to judge — unlike a
            // bundle alias `.`, which would have to become a directory.
            continue;
        }
        if segment == ".." {
            // Even a `..` that lexically stays inside the workspace is
            // refused: the loader resolves the path through the filesystem,
            // where a symlink before the `..` lands somewhere the lexical
            // form does not name, and the bundle would carry a different
            // file than the agent actually loads.
            return Err(
                "the AIEOS identity document path contains a `..` segment; name the file \
                        by its plain path from the workspace root"
                    .to_string(),
            );
        }
        // Whitespace is not trimmed away here, and neither is anything else:
        // a filename that ends in a space or a dot is a real name on this
        // host, and quietly exporting a different spelling would have the
        // imported agent load a different file than the source agent does.
        // The reference is dropped instead, and the operator hears why.
        if let Some(reason) = unportable_component(segment) {
            return Err(format!(
                "the AIEOS identity document path has a component that {reason} \
                 (`{segment}`); a bundle is opened on hosts other than this one, so it \
                 can carry only names every supported target can materialize"
            ));
        }
    }
    portable_identity_path_inner(raw)
}

/// The rest of the check, once the path is known to be one a target reads the
/// same way this host does.
fn portable_identity_path_inner(raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err("the AIEOS identity document path is empty".to_string());
    }
    if std::path::Path::new(raw).is_absolute() {
        return Err(
            "the AIEOS identity document is referenced by a source-host absolute path; \
                    point the imported agent at a path inside its own workspace"
                .to_string(),
        );
    }
    let root = std::path::Path::new(WORKSPACE_DIR);
    let Ok(resolved) = crate::paths::resolve_under(root, raw) else {
        return Err(
            "the AIEOS identity document path escapes the workspace, which would have \
                    the imported agent read outside its own tree at startup"
                .to_string(),
        );
    };
    let Ok(relative) = resolved.strip_prefix(root) else {
        return Err(
            "the AIEOS identity document path could not be resolved inside the workspace"
                .to_string(),
        );
    };
    if !workspace_entry_included(relative) {
        return Err(
            "the AIEOS identity document sits in the part of the workspace this bundle \
                    does not carry, so the reference would dangle on the target"
                .to_string(),
        );
    }
    // Serialize the validated components in the bundle's own grammar rather
    // than the exporting host's: `display()` would turn the separators back
    // into `\` on Windows, and a Unix target reads a backslash as part of a
    // filename. The string written is the string any target resolves.
    let mut portable = String::new();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(
                "the AIEOS identity document path could not be resolved inside the workspace"
                    .to_string(),
            );
        };
        let Some(name) = name.to_str() else {
            return Err(
                "the AIEOS identity document path could not be resolved inside the workspace"
                    .to_string(),
            );
        };
        if !portable.is_empty() {
            portable.push('/');
        }
        portable.push_str(name);
    }
    if portable.is_empty() {
        return Err("the AIEOS identity document path is empty".to_string());
    }
    Ok(portable)
}

/// Record that the workspace copy did not carry the identity document the
/// closure references, and drop the reference.
///
/// Whether the file is really there is a filesystem question, so the caller
/// answers it once the workspace has been copied. Call before
/// [`render_config_toml`].
pub fn record_missing_identity_document(plan: &mut ExportPlan) {
    let Some(relative) = plan.identity_document.take() else {
        return;
    };
    let alias = plan.root_alias.clone();
    if let Some(identity) = table_at_mut(&mut plan.config, &["agents", &alias, "identity"]) {
        identity.remove("aieos_path");
    }
    plan.dropped.push(DroppedRef {
        path: format!("agents.{alias}.identity.aieos_path"),
        reason: DropReason::SourceMissing,
        detail: format!(
            "the AIEOS identity document `{relative}` is not in the exported workspace, so the \
             reference is dropped rather than shipped pointing at nothing"
        ),
    });
}

/// Capabilities in this closure that widen the target install's trust boundary.
fn collect_risk_flags(
    config: &Config,
    alias: &str,
    agent: &AliasedAgentConfig,
    servers: &[McpServerConfig],
    skill_sources: &[SkillBundleSource],
) -> Vec<RiskFlag> {
    let mut flags = Vec::new();

    for source in skill_sources {
        flags.push(RiskFlag {
            kind: RiskKind::CarriedSkills,
            path: format!("skill_bundles.{}", source.alias),
            detail: "the bundle carries this skill bundle's content from the exporting install; \
                     skills are instructions the agent reads and act as code it may run"
                .to_string(),
        });
    }

    let risk_alias = agent.risk_profile.trim();
    if let Some(profile) = config.risk_profiles.get(risk_alias) {
        let at = |leaf: &str| format!("risk_profiles.{risk_alias}.{leaf}");

        if matches!(profile.level, crate::autonomy::AutonomyLevel::Full) {
            flags.push(RiskFlag {
                kind: RiskKind::FullAutonomy,
                path: at("level"),
                detail: "the agent acts autonomously within policy bounds — no per-operation \
                         approval prompt"
                    .to_string(),
            });
        }
        // The runtime does not take the authored `workspace_only` at face
        // value: `SecurityPolicy::from_profiles` forces it off under full
        // autonomy. Build the policy the target will build, so this names the
        // authority the imported agent actually gets rather than the value
        // someone typed into the profile.
        let effective = crate::policy::SecurityPolicy::from_profiles(
            profile,
            config.runtime_profiles.get(agent.runtime_profile.trim()),
            &config.agent_workspace_dir(alias),
        );
        if !effective.workspace_only {
            let (path, detail) = if profile.workspace_only {
                (
                    at("level"),
                    "full autonomy turns workspace confinement off, so filesystem access is not                      confined to the workspace even though `workspace_only` is set"
                        .to_string(),
                )
            } else {
                (
                    at("workspace_only"),
                    "filesystem access is not confined to the workspace".to_string(),
                )
            };
            flags.push(RiskFlag {
                kind: RiskKind::FilesystemEscape,
                path,
                detail,
            });
        }
        if profile.sandbox_enabled == Some(false) {
            flags.push(RiskFlag {
                kind: RiskKind::SandboxDisabled,
                path: at("sandbox_enabled"),
                detail: "shell execution runs unsandboxed on the host".to_string(),
            });
        }
        if !profile.block_high_risk_commands {
            flags.push(RiskFlag {
                kind: RiskKind::ApprovalBypass,
                path: at("block_high_risk_commands"),
                detail: "high-risk shell commands are not blocked".to_string(),
            });
        }
        if !profile.require_approval_for_medium_risk {
            flags.push(RiskFlag {
                kind: RiskKind::ApprovalBypass,
                path: at("require_approval_for_medium_risk"),
                detail: "medium-risk operations run without an approval prompt".to_string(),
            });
        }
        if !profile.shell_env_passthrough.is_empty() {
            flags.push(RiskFlag {
                kind: RiskKind::EnvPassthrough,
                path: at("shell_env_passthrough"),
                detail: format!(
                    "host environment variables are forwarded into shell subprocesses: {}",
                    profile.shell_env_passthrough.join(", ")
                ),
            });
        }
        if !profile.allowed_roots.is_empty() {
            flags.push(RiskFlag {
                kind: RiskKind::ExtraFilesystemRoots,
                path: at("allowed_roots"),
                detail: format!(
                    "extra filesystem roots are granted beyond the workspace: {}",
                    profile.allowed_roots.join(", ")
                ),
            });
        }
        if profile.delegation_policy.permits() {
            flags.push(RiskFlag {
                kind: RiskKind::DelegationEnabled,
                path: at("delegation_policy.mode"),
                detail: "the agent may initiate delegation to other agents on the target install"
                    .to_string(),
            });
        }
    }

    if agent.workspace.unrestricted_filesystem {
        flags.push(RiskFlag {
            kind: RiskKind::FilesystemEscape,
            path: format!("agents.{alias}.workspace.unrestricted_filesystem"),
            detail: "the agent can read and write anywhere the host filesystem permits".to_string(),
        });
    }

    for server in servers {
        if matches!(server.transport, McpTransport::Stdio) {
            flags.push(RiskFlag {
                kind: RiskKind::ProcessSpawn,
                path: format!("mcp.servers.{}.command", server.name),
                detail: format!(
                    "starts a local process on the target host: {}",
                    shell_preview(server)
                ),
            });
        }
        if !server.pinned_resources.is_empty() {
            flags.push(RiskFlag {
                kind: RiskKind::UntrustedStartupContext,
                path: format!("mcp.servers.{}.pinned_resources", server.name),
                detail: format!(
                    "server-controlled text is read into the system prompt at startup: {}",
                    server.pinned_resources.join(", ")
                ),
            });
        }
    }

    flags
}

/// Command line an stdio MCP server would run, for the risk report.
fn shell_preview(server: &McpServerConfig) -> String {
    if server.args.is_empty() {
        server.command.clone()
    } else {
        format!("{} {}", server.command, server.args.join(" "))
    }
}

/// Copy `path` out of the masked config into `out`, pruned against `defaults`.
/// A path that does not resolve is skipped: an unresolvable reference grants
/// nothing, and the import-side validation is what reports it.
fn carry(out: &mut toml::Table, masked_root: &toml::Table, path: &[&str], defaults: &toml::Table) {
    let Some(toml::Value::Table(entry)) = lookup(masked_root, path) else {
        return;
    };
    let pruned = prune(entry.clone(), defaults);
    insert_at(out, path, toml::Value::Table(pruned));
}

/// The masked table for one MCP server, matched by natural key.
///
/// Falls back to serializing the resolved entry when the masked array has no
/// element with this name — which cannot happen for a config that round-trips,
/// but must not panic if it does.
fn masked_server_table(
    masked_root: &toml::Table,
    name: &str,
    fallback: &McpServerConfig,
) -> Result<toml::Table, ExportError> {
    let found = lookup(masked_root, &["mcp", "servers"])
        .and_then(toml::Value::as_array)
        .and_then(|servers| {
            servers.iter().find_map(|server| {
                let table = server.as_table()?;
                let matches = table.get("name").and_then(toml::Value::as_str) == Some(name);
                matches.then(|| table.clone())
            })
        });
    match found {
        Some(table) => Ok(table),
        None => {
            let mut masked = fallback.clone();
            masked.mask_secrets();
            to_table(&masked)
        }
    }
}

// ── TOML helpers ─────────────────────────────────────────────────────────

fn to_table<T: Serialize>(value: &T) -> Result<toml::Table, ExportError> {
    match toml::Value::try_from(value) {
        Ok(toml::Value::Table(table)) => Ok(table),
        Ok(_) => Err(ExportError::Serialize("expected a TOML table".to_string())),
        Err(err) => Err(ExportError::Serialize(err.to_string())),
    }
}

/// Mutable handle on the table already carried at `path`, if there is one.
fn table_at_mut<'a>(root: &'a mut toml::Table, path: &[&str]) -> Option<&'a mut toml::Table> {
    let (last, prefix) = path.split_last()?;
    let mut cursor = root;
    for segment in prefix {
        cursor = cursor.get_mut(*segment)?.as_table_mut()?;
    }
    cursor.get_mut(*last)?.as_table_mut()
}

fn lookup<'a>(root: &'a toml::Table, path: &[&str]) -> Option<&'a toml::Value> {
    let (last, prefix) = path.split_last()?;
    let mut cursor = root;
    for segment in prefix {
        cursor = cursor.get(*segment)?.as_table()?;
    }
    cursor.get(*last)
}

/// Build the nested table `{a: {b: {c: value}}}` for path `[a, b, c]`.
fn nested(path: &[&str], value: toml::Value) -> toml::Table {
    let mut table = toml::Table::new();
    match path.split_first() {
        None => {}
        Some((head, [])) => {
            table.insert((*head).to_string(), value);
        }
        Some((head, rest)) => {
            table.insert((*head).to_string(), toml::Value::Table(nested(rest, value)));
        }
    }
    table
}

fn merge_into(dest: &mut toml::Table, src: toml::Table) {
    for (key, value) in src {
        match (dest.get_mut(&key), value) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(incoming)) => {
                merge_into(existing, incoming);
            }
            (_, value) => {
                dest.insert(key, value);
            }
        }
    }
}

fn insert_at(root: &mut toml::Table, path: &[&str], value: toml::Value) {
    merge_into(root, nested(path, value));
}

/// Drop every key whose value equals the schema default, so a bundle carries
/// the operator's actual choices instead of hundreds of lines of defaults.
fn prune(mut table: toml::Table, defaults: &toml::Table) -> toml::Table {
    crate::schema::prune_default_values(&mut table, defaults);
    table
}

fn dedup(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .filter(|value| seen.insert((*value).to_string()))
        .map(str::to_string)
        .collect()
}

/// Split a dotted `<family>.<alias>` provider reference. Returns `None` for an
/// empty or malformed reference — `Config::validate()` owns reporting those.
fn split_provider_ref(reference: &str) -> Option<(&str, &str)> {
    let (family, entry) = reference.split_once('.')?;
    (!family.is_empty() && !entry.is_empty()).then_some((family, entry))
}

/// Walk every string leaf, passing the dotted path and a mutable handle.
///
/// Array elements carrying a `name` are addressed by that natural key
/// (`mcp.servers.github.env.TOKEN`) so reported paths match the ones
/// `zeroclaw config set` accepts. That addressing is only unambiguous while
/// names are unique within the array; `mcp.servers` is the only array the
/// closure carries, and the invariant that keeps it so is recorded where it is
/// built in [`plan_export`].
fn visit_strings_mut(
    value: &mut toml::Value,
    path: &str,
    visit: &mut impl FnMut(&str, &mut String),
) {
    match value {
        toml::Value::String(text) => visit(path, text),
        toml::Value::Table(table) => {
            for (key, child) in table.iter_mut() {
                visit_strings_mut(child, &join(path, key), visit);
            }
        }
        toml::Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                let segment = array_segment(child, index);
                visit_strings_mut(child, &join(path, &segment), visit);
            }
        }
        _ => {}
    }
}

/// Segment addressing an array element: its natural `name` key when it has
/// one, else its index. Shared by both walks so a reported path means the same
/// thing whichever one produced it.
fn array_segment(value: &toml::Value, index: usize) -> String {
    value
        .as_table()
        .and_then(|table| table.get("name"))
        .and_then(toml::Value::as_str)
        .map_or_else(|| index.to_string(), str::to_string)
}

fn join(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix}.{segment}")
    }
}

/// First path holding config ciphertext, if any survived scrubbing.
///
/// The read-only twin of [`visit_strings_mut`]'s walk, sharing its path naming
/// through [`array_segment`]. Verifying the scrub is a check, not a rewrite, so
/// it neither copies the closure nor keeps walking past the first hit.
fn find_ciphertext(value: &toml::Value, path: &str) -> Option<String> {
    match value {
        toml::Value::String(text) => CIPHERTEXT_PREFIXES
            .iter()
            .any(|prefix| text.starts_with(prefix))
            .then(|| path.to_string()),
        toml::Value::Table(table) => table
            .iter()
            .find_map(|(key, child)| find_ciphertext(child, &join(path, key))),
        toml::Value::Array(items) => items.iter().enumerate().find_map(|(index, child)| {
            find_ciphertext(child, &join(path, &array_segment(child, index)))
        }),
        _ => None,
    }
}

// ── rendering ────────────────────────────────────────────────────────────

const CONFIG_HEADER: &str = "\
# ZeroClaw agent bundle — config closure.
#
# This is a FRAGMENT, not a complete config.toml. `zeroclaw agents import`
# merges it into the target install; it never replaces the target's config.
# Empty-string values are credentials that were scrubbed on export and must be
# supplied on the target — see `required_secrets` in zeroclaw-agent.toml.
#
# Scrubbing blanks the fields the schema marks secret. It is not credential
# detection: any other value here travels as written, so a token in an MCP
# server's `url`, or a credential in its `command` or `args`, is still present.
";

/// Render the closure as the bundle's `config.toml`.
pub fn render_config_toml(plan: &ExportPlan) -> Result<String, ExportError> {
    let body = toml::to_string_pretty(&plan.config)
        .map_err(|err| ExportError::Serialize(err.to_string()))?;
    let rendered = format!(
        "{CONFIG_HEADER}{}",
        crate::schema::ensure_blank_line_before_sections(&body)
    );
    validate_closure(&rendered, &plan.root_alias)?;
    Ok(rendered)
}

/// Prove `rendered` is a config in its own right, the way the importing
/// install will read it.
///
/// The bundle's central promise is a closure that stands alone: an import
/// starts from a config holding none of the source install's entries, so
/// every reference the closure carries has to resolve inside it. `carry`
/// copies a referenced entry only when the source config has one, which
/// leaves the agent pointing at nothing — a shape this code really is handed,
/// because `Config::load_or_init` reports validation errors and boots anyway
/// so an operator is never locked out of the CLI that repairs them. The
/// exporter is one of the commands that stays reachable.
///
/// The families are not enumerated here. A hand-kept list drifts from the
/// schema, and the schema already owns this question: the rendered text is
/// parsed back into a `Config` and put through the same `Config::validate()`
/// the target runs at load. Its error names the exact field, so the operator
/// is told what to fix rather than that something is wrong.
///
/// A deserialized fragment carries no `config_path`, so `install_root_dir()`
/// is the relative default and the skill-bundle directory rules resolve
/// against it the same way they will against the target's own install root.
fn validate_closure(rendered: &str, alias: &str) -> Result<(), ExportError> {
    let imported: Config = toml::from_str(rendered).map_err(|err| ExportError::ClosureInvalid {
        alias: alias.to_string(),
        error: err.to_string(),
    })?;
    imported
        .validate()
        .map_err(|err| ExportError::ClosureInvalid {
            alias: alias.to_string(),
            error: format!("{err:#}"),
        })
}

/// Build the bundle manifest table.
pub fn render_manifest(plan: &ExportPlan, provenance: &Provenance) -> toml::Table {
    let mut manifest = toml::Table::new();
    manifest.insert(
        "format_version".to_string(),
        toml::Value::Integer(i64::from(BUNDLE_FORMAT_VERSION)),
    );
    manifest.insert(
        "root_agent".to_string(),
        toml::Value::String(plan.root_alias.clone()),
    );
    manifest.insert(
        "required_secrets".to_string(),
        toml::Value::Array(
            plan.required_secrets
                .iter()
                .map(|path| toml::Value::String(path.clone()))
                .collect(),
        ),
    );
    // Bundle aliases whose content is carried under `skills/<alias>/`. An
    // import maps each back onto the directory the *target* resolves for that
    // alias, which is why the alias, not the source path, is what is recorded.
    //
    // This lists what the bundle *contains*, so it is only accurate once
    // `record_missing_skill_content` has folded in what the copy found. A
    // bundle whose content was unavailable is named in `dropped` instead.
    manifest.insert(
        "skill_bundles".to_string(),
        toml::Value::Array(
            plan.skill_sources
                .iter()
                .map(|source| toml::Value::String(source.alias.clone()))
                .collect(),
        ),
    );

    let mut provenance_table = toml::Table::new();
    provenance_table.insert(
        "zeroclaw_version".to_string(),
        toml::Value::String(provenance.zeroclaw_version.clone()),
    );
    provenance_table.insert(
        "exported_at".to_string(),
        toml::Value::String(provenance.exported_at.clone()),
    );
    manifest.insert(
        "provenance".to_string(),
        toml::Value::Table(provenance_table),
    );

    manifest.insert(
        "dropped".to_string(),
        toml::Value::Array(
            plan.dropped
                .iter()
                .map(|entry| {
                    let mut table = toml::Table::new();
                    table.insert("path".to_string(), toml::Value::String(entry.path.clone()));
                    table.insert(
                        "reason".to_string(),
                        toml::Value::String(entry.reason.as_wire().to_string()),
                    );
                    table.insert(
                        "detail".to_string(),
                        toml::Value::String(entry.detail.clone()),
                    );
                    toml::Value::Table(table)
                })
                .collect(),
        ),
    );

    manifest.insert(
        "risk_flags".to_string(),
        toml::Value::Array(
            plan.risk_flags
                .iter()
                .map(|flag| {
                    let mut table = toml::Table::new();
                    table.insert(
                        "kind".to_string(),
                        toml::Value::String(flag.kind.as_wire().to_string()),
                    );
                    table.insert("path".to_string(), toml::Value::String(flag.path.clone()));
                    table.insert(
                        "detail".to_string(),
                        toml::Value::String(flag.detail.clone()),
                    );
                    toml::Value::Table(table)
                })
                .collect(),
        ),
    );

    manifest
}

const MANIFEST_HEADER: &str = "\
# ZeroClaw agent bundle manifest.
#
# `required_secrets` lists config paths whose credentials were scrubbed on
# export. `risk_flags` lists capabilities the importing operator is being asked
# to grant. `dropped` lists configuration that could not travel.
#
# Scrubbing blanks the fields the schema marks secret in config.toml, and
# nothing more. Other config strings travel as written, and `risk_flags` below
# repeats stdio server command lines verbatim. The files under workspace/ and
# skills/ are copied as-is. None of it is scanned for secrets.
";

/// Render the manifest as the bundle's `zeroclaw-agent.toml`.
pub fn render_manifest_toml(
    plan: &ExportPlan,
    provenance: &Provenance,
) -> Result<String, ExportError> {
    let manifest = render_manifest(plan, provenance);
    let body =
        toml::to_string_pretty(&manifest).map_err(|err| ExportError::Serialize(err.to_string()))?;
    Ok(format!(
        "{MANIFEST_HEADER}{}",
        crate::schema::ensure_blank_line_before_sections(&body)
    ))
}

/// Whether a workspace-relative path should be copied into the bundle.
///
/// Both halves of the agent's memory are excluded: the store under
/// `memory/`, and the [`MEMORY_SNAPSHOT_FILE`] at the workspace root that an
/// agent re-hydrates the store from. Only the snapshot at the root is the
/// store's own; a file of that name deeper in the tree is the operator's.
///
/// Both matches are anchored at the workspace root on purpose, and neither is
/// a substring test: the store is `<workspace>/memory/`, so a nested
/// `notes/memory/` is the operator's own directory and travels.
#[must_use]
pub fn workspace_entry_included(relative: &std::path::Path) -> bool {
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return true;
    };
    if first.as_os_str() == WORKSPACE_MEMORY_DIR {
        return false;
    }
    components.next().is_some() || first.as_os_str() != MEMORY_SNAPSHOT_FILE
}

/// Cross-agent access modes present in a bundle, for the import-side report.
/// Exposed so the importer can describe grants without re-deriving them.
#[must_use]
pub fn access_mode_label(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::Read => "read",
        AccessMode::Write => "write",
        AccessMode::ReadWrite => "read+write",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autonomy::{AutonomyLevel, DelegationMode};
    use crate::multi_agent::AgentWorkspaceConfig;
    use std::collections::HashMap;

    /// Config with one agent wired to a risk profile, an MCP bundle granting
    /// two of three servers, and an Anthropic model provider carrying a key.
    fn fixture() -> Config {
        let mut config = Config::default();

        config
            .risk_profiles
            .insert("guarded".to_string(), RiskProfileConfig::default());

        config.mcp_bundles.insert(
            "research".to_string(),
            McpBundleConfig {
                servers: vec!["github".to_string(), "search".to_string()],
                exclude: vec!["search".to_string()],
            },
        );

        config.mcp.servers = vec![
            McpServerConfig {
                name: "github".to_string(),
                transport: McpTransport::Stdio,
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-github".to_string(),
                ],
                env: HashMap::from([(
                    "GITHUB_TOKEN".to_string(),
                    "ghp_realsecretvalue".to_string(),
                )]),
                ..Default::default()
            },
            McpServerConfig {
                name: "search".to_string(),
                transport: McpTransport::Http,
                url: Some("https://search.example.com/mcp".to_string()),
                ..Default::default()
            },
            McpServerConfig {
                name: "unrelated".to_string(),
                transport: McpTransport::Stdio,
                command: "other".to_string(),
                ..Default::default()
            },
        ];

        config.providers.models.anthropic.insert(
            "main".to_string(),
            crate::schema::AnthropicModelProviderConfig {
                base: crate::schema::ModelProviderConfig {
                    api_key: Some("sk-ant-realkey".to_string()),
                    model: Some("claude-opus-5".to_string()),
                    ..Default::default()
                },
            },
        );

        config.agents.insert(
            "researcher".to_string(),
            AliasedAgentConfig {
                model_provider: "anthropic.main".into(),
                risk_profile: "guarded".into(),
                mcp_bundles: vec!["research".to_string()],
                channels: vec!["telegram.work".into()],
                ..Default::default()
            },
        );

        config
    }

    #[test]
    fn unknown_alias_is_an_error() {
        let config = Config::default();
        let err = plan_export(&config, "nobody").unwrap_err();
        assert!(matches!(err, ExportError::UnknownAgent(alias) if alias == "nobody"));
    }

    #[test]
    fn closure_carries_only_the_servers_the_bundles_grant() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let servers = lookup(&plan.config, &["mcp", "servers"])
            .and_then(toml::Value::as_array)
            .expect("closure carries mcp.servers");
        let names: Vec<&str> = servers
            .iter()
            .filter_map(|s| s.get("name").and_then(toml::Value::as_str))
            .collect();
        // `search` is excluded by the bundle and `unrelated` is not referenced.
        assert_eq!(names, vec!["github"]);
    }

    /// `required_secrets` promises paths that can be pasted into
    /// `zeroclaw config set`, which holds only while a server name addresses
    /// one entry. `Config::validate` rejects duplicate names, so this pins what
    /// the exporter does on its own with a config that reached it hand-edited.
    #[test]
    fn duplicate_server_names_collapse_to_one_entry() {
        let mut config = fixture();
        config.mcp.servers.push(McpServerConfig {
            name: "github".to_string(),
            transport: McpTransport::Stdio,
            command: "impostor".to_string(),
            env: HashMap::from([("GITHUB_TOKEN".to_string(), "ghp_theotherone".to_string())]),
            ..Default::default()
        });

        let plan = plan_export(&config, "researcher").unwrap();

        let servers = lookup(&plan.config, &["mcp", "servers"])
            .and_then(toml::Value::as_array)
            .expect("closure carries mcp.servers");
        let names: Vec<&str> = servers
            .iter()
            .filter_map(|s| s.get("name").and_then(toml::Value::as_str))
            .collect();
        assert_eq!(names, vec!["github"], "one entry per name");

        // One path, so the operator has one thing to fill in and no ambiguity
        // about which entry they are filling.
        let github_secrets: Vec<&str> = plan
            .required_secrets
            .iter()
            .filter(|path| path.starts_with("mcp.servers.github"))
            .map(String::as_str)
            .collect();
        assert_eq!(github_secrets, vec!["mcp.servers.github.env.GITHUB_TOKEN"]);

        // Resolution and masking agree on which entry won, so the shadowed
        // one contributes nothing to the bundle.
        let rendered = render_config_toml(&plan).unwrap();
        assert!(!rendered.contains("impostor"), "{rendered}");
        assert!(!rendered.contains("ghp_theotherone"), "{rendered}");
    }

    #[test]
    fn exclude_still_wins_inside_the_closure() {
        let mut config = fixture();
        // A second bundle that grants `search` outright — the first bundle's
        // `exclude` must still remove it, matching runtime resolution.
        config.mcp_bundles.insert(
            "extra".to_string(),
            McpBundleConfig {
                servers: vec!["search".to_string()],
                exclude: Vec::new(),
            },
        );
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.mcp_bundles.push("extra".to_string());
        }

        let plan = plan_export(&config, "researcher").unwrap();
        let rendered = render_config_toml(&plan).unwrap();
        assert!(!rendered.contains("search.example.com"), "{rendered}");
    }

    #[test]
    fn no_credential_survives_into_the_rendered_bundle() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let rendered = render_config_toml(&plan).unwrap();
        assert!(!rendered.contains("ghp_realsecretvalue"), "{rendered}");
        assert!(!rendered.contains("sk-ant-realkey"), "{rendered}");
        assert!(!rendered.contains("***MASKED***"), "{rendered}");
    }

    #[test]
    fn required_secrets_name_every_scrubbed_leaf_by_config_path() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        assert!(
            plan.required_secrets
                .contains(&"mcp.servers.github.env.GITHUB_TOKEN".to_string()),
            "{:?}",
            plan.required_secrets
        );
        assert!(
            plan.required_secrets
                .contains(&"providers.models.anthropic.main.api_key".to_string()),
            "{:?}",
            plan.required_secrets
        );
    }

    #[test]
    fn scrubbed_leaves_are_emitted_empty_not_masked() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let key = lookup(
            &plan.config,
            &["providers", "models", "anthropic", "main", "api_key"],
        )
        .and_then(toml::Value::as_str);
        assert_eq!(key, Some(""));
    }

    #[test]
    fn host_specific_and_relational_fields_are_dropped_and_reported() {
        let mut config = fixture();
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.workspace = AgentWorkspaceConfig {
                path: Some(PathBuf::from("/srv/custom")),
                access: [("beta".to_string().into(), AccessMode::Read)]
                    .into_iter()
                    .collect(),
                read_memory_from: vec!["beta".to_string().into()],
                unrestricted_filesystem: false,
            };
        }
        let plan = plan_export(&config, "researcher").unwrap();

        let agent = lookup(&plan.config, &["agents", "researcher"])
            .and_then(toml::Value::as_table)
            .expect("agent entry present");
        assert!(!agent.contains_key("channels"));
        let workspace = agent.get("workspace").and_then(toml::Value::as_table);
        if let Some(workspace) = workspace {
            assert!(!workspace.contains_key("path"));
            assert!(!workspace.contains_key("access"));
            assert!(!workspace.contains_key("read_memory_from"));
        }

        let paths: Vec<&str> = plan.dropped.iter().map(|d| d.path.as_str()).collect();
        for expected in [
            "agents.researcher.channels",
            "agents.researcher.workspace.path",
            "agents.researcher.workspace.access",
            "agents.researcher.workspace.read_memory_from",
        ] {
            assert!(paths.contains(&expected), "{paths:?} missing {expected}");
        }
    }

    #[test]
    fn drop_report_describes_this_agent_not_the_schema() {
        // The fixture agent has no delegates, cron jobs, or A2A publication,
        // so those must not appear in the report.
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let paths: Vec<&str> = plan.dropped.iter().map(|d| d.path.as_str()).collect();
        assert!(!paths.contains(&"agents.researcher.delegates"), "{paths:?}");
        assert!(!paths.contains(&"agents.researcher.cron_jobs"), "{paths:?}");
        assert!(!paths.contains(&"agents.researcher.a2a"), "{paths:?}");
    }

    /// Format 1 has no opt-in for memory: a live WAL database cannot be copied
    /// as files without risking a torn read, so neither half of the agent's
    /// memory travels, and both are reported.
    #[test]
    fn neither_half_of_memory_travels_and_both_are_reported() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let dropped: Vec<(&str, &str)> = plan
            .dropped
            .iter()
            .map(|d| (d.path.as_str(), d.reason.as_wire()))
            .collect();
        assert!(
            dropped.contains(&("workspace/memory", "not_yet_portable")),
            "{dropped:?}"
        );
        assert!(
            dropped.contains(&("workspace/MEMORY_SNAPSHOT.md", "private_data")),
            "{dropped:?}"
        );
    }

    #[test]
    fn memory_is_filtered_out_of_the_workspace_copy() {
        use std::path::Path;
        // The store, including the WAL sidecars beside the database.
        assert!(!workspace_entry_included(Path::new("memory")));
        assert!(!workspace_entry_included(Path::new("memory/brain.db")));
        assert!(!workspace_entry_included(Path::new("memory/brain.db-wal")));
        assert!(!workspace_entry_included(Path::new("memory/brain.db-shm")));
        // The snapshot the store re-hydrates from, at the workspace root.
        assert!(!workspace_entry_included(Path::new("MEMORY_SNAPSHOT.md")));

        // Ordinary workspace content, including paths that merely look like
        // memory: only the root-level snapshot is the store's own.
        assert!(workspace_entry_included(Path::new("IDENTITY.md")));
        assert!(workspace_entry_included(Path::new(
            "notes/memory/scratch.md"
        )));
        assert!(workspace_entry_included(Path::new(
            "notes/MEMORY_SNAPSHOT.md"
        )));
    }

    #[test]
    fn stdio_servers_are_flagged_as_process_spawn() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let flag = plan
            .risk_flags
            .iter()
            .find(|f| f.kind == RiskKind::ProcessSpawn)
            .expect("stdio server flagged");
        assert_eq!(flag.path, "mcp.servers.github.command");
        assert!(flag.detail.contains("npx"), "{}", flag.detail);
    }

    #[test]
    fn permissive_risk_profile_raises_every_matching_flag() {
        let mut config = fixture();
        config.risk_profiles.insert(
            "guarded".to_string(),
            RiskProfileConfig {
                level: AutonomyLevel::Full,
                workspace_only: false,
                sandbox_enabled: Some(false),
                block_high_risk_commands: false,
                require_approval_for_medium_risk: false,
                shell_env_passthrough: vec!["AWS_SECRET_ACCESS_KEY".to_string()],
                allowed_roots: vec!["/".to_string()],
                delegation_policy: crate::autonomy::DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                ..Default::default()
            },
        );
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.workspace.unrestricted_filesystem = true;
        }

        let plan = plan_export(&config, "researcher").unwrap();
        let kinds: BTreeSet<&'static str> = plan
            .risk_flags
            .iter()
            .map(|flag| flag.kind.as_wire())
            .collect();
        for expected in [
            "full_autonomy",
            "filesystem_escape",
            "sandbox_disabled",
            "approval_bypass",
            "env_passthrough",
            "extra_filesystem_roots",
            "delegation_enabled",
        ] {
            assert!(kinds.contains(expected), "{kinds:?} missing {expected}");
        }
    }

    /// The authored profile says the agent is workspace-confined; the runtime
    /// says otherwise, because full autonomy forces confinement off. The
    /// manifest has to disclose the authority the importer actually grants.
    #[test]
    fn full_autonomy_discloses_the_filesystem_escape_it_forces() {
        let mut config = fixture();
        config.risk_profiles.insert(
            "guarded".to_string(),
            RiskProfileConfig {
                level: AutonomyLevel::Full,
                // Left at the default: authored as confined.
                ..Default::default()
            },
        );
        assert!(
            config.risk_profiles["guarded"].workspace_only,
            "fixture must exercise the default, or the test proves nothing"
        );

        let plan = plan_export(&config, "researcher").unwrap();
        let flags: Vec<(&str, &str)> = plan
            .risk_flags
            .iter()
            .map(|f| (f.kind.as_wire(), f.path.as_str()))
            .collect();

        assert!(
            flags.contains(&("full_autonomy", "risk_profiles.guarded.level")),
            "{flags:?}"
        );
        // Bound to `level`, which is what grants it, not to the
        // `workspace_only` field that still reads `true`.
        assert!(
            flags.contains(&("filesystem_escape", "risk_profiles.guarded.level")),
            "{flags:?}"
        );

        // The effective policy the target builds agrees with the disclosure.
        let effective = crate::policy::SecurityPolicy::from_profiles(
            &config.risk_profiles["guarded"],
            None,
            std::path::Path::new("/tmp/workspace"),
        );
        assert!(!effective.workspace_only);
    }

    #[test]
    fn default_profile_raises_no_flags_beyond_transport() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let policy_flags: Vec<&str> = plan
            .risk_flags
            .iter()
            .filter(|flag| flag.kind != RiskKind::ProcessSpawn)
            .map(|flag| flag.kind.as_wire())
            .collect();
        assert!(policy_flags.is_empty(), "{policy_flags:?}");
    }

    #[test]
    fn pinned_resources_are_flagged_as_untrusted_startup_context() {
        let mut config = fixture();
        if let Some(server) = config.mcp.servers.iter_mut().find(|s| s.name == "github") {
            server.pinned_resources = vec!["repo://readme".to_string()];
        }
        let plan = plan_export(&config, "researcher").unwrap();
        assert!(
            plan.risk_flags
                .iter()
                .any(|f| f.kind == RiskKind::UntrustedStartupContext)
        );
    }

    #[test]
    fn every_provider_reference_the_agent_names_is_carried() {
        let mut config = fixture();
        config.providers.tts.openai.insert(
            "voice".to_string(),
            crate::schema::OpenAITtsProviderConfig::default(),
        );
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.tts_provider = "openai.voice".into();
        }
        let plan = plan_export(&config, "researcher").unwrap();
        assert!(
            lookup(&plan.config, &["providers", "tts", "openai", "voice"]).is_some(),
            "tts provider carried"
        );
        assert!(
            lookup(&plan.config, &["providers", "models", "anthropic", "main"]).is_some(),
            "model provider carried"
        );
    }

    #[test]
    fn rendered_bundle_round_trips_as_toml() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let config_text = render_config_toml(&plan).unwrap();
        let parsed: toml::Table = config_text.parse().expect("config fragment re-parses");
        assert!(parsed.contains_key("agents"));

        let provenance = Provenance {
            zeroclaw_version: "0.0.0-test".to_string(),
            exported_at: "2026-08-12T00:00:00Z".to_string(),
        };
        let manifest_text = render_manifest_toml(&plan, &provenance).unwrap();
        let manifest: toml::Table = manifest_text.parse().expect("manifest re-parses");
        assert_eq!(
            manifest.get("root_agent").and_then(toml::Value::as_str),
            Some("researcher")
        );
        assert_eq!(
            manifest
                .get("format_version")
                .and_then(toml::Value::as_integer),
            Some(i64::from(BUNDLE_FORMAT_VERSION))
        );
    }

    /// The fixture agent wired to every reference kind an export has to
    /// resolve: profiles (including one that names a provider of its own),
    /// bundles, the typed provider slots, a sibling agent, and host-specific
    /// paths.
    fn loaded_fixture() -> Config {
        let mut config = fixture();

        config.providers.models.anthropic.insert(
            "summarizer".to_string(),
            crate::schema::AnthropicModelProviderConfig::default(),
        );
        config.providers.models.anthropic.insert(
            "classifier".to_string(),
            crate::schema::AnthropicModelProviderConfig::default(),
        );
        config.providers.tts.openai.insert(
            "voice".to_string(),
            crate::schema::OpenAITtsProviderConfig::default(),
        );

        let mut profile = RuntimeProfileConfig::default();
        profile.context_compression.summary_provider = "anthropic.summarizer".into();
        config
            .runtime_profiles
            .insert("standard".to_string(), profile);

        config.skill_bundles.insert(
            "research_tools".to_string(),
            SkillBundleConfig {
                directory: Some("/srv/zeroclaw/shared/skills/pool".to_string()),
                ..Default::default()
            },
        );
        config
            .knowledge_bundles
            .insert("house_docs".to_string(), KnowledgeBundleConfig::default());

        // A sibling on the same risk profile: the peer that same-profile
        // delegation reach would silently connect on the target.
        config.agents.insert(
            "assistant".to_string(),
            AliasedAgentConfig {
                model_provider: "anthropic.main".into(),
                risk_profile: "guarded".into(),
                ..Default::default()
            },
        );

        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.runtime_profile = "standard".into();
            agent.classifier_provider = "anthropic.classifier".into();
            agent.tts_provider = "openai.voice".into();
            agent.skill_bundles = vec!["research_tools".to_string()];
            agent.knowledge_bundles = vec!["house_docs".to_string()];
            agent.delegates = vec![crate::schema::DelegateTargetConfig::bounded("assistant")];
            agent.identity.aieos_path = Some("/srv/identities/researcher.json".to_string());
        }

        config
    }

    /// The bundle's central promise: the closure is complete on its own. An
    /// import starts from a config that has none of the source install's
    /// entries, so every reference the closure carries has to resolve within
    /// the closure itself.
    #[test]
    fn closure_validates_on_an_otherwise_empty_install() {
        for config in [fixture(), loaded_fixture()] {
            let plan = plan_export(&config, "researcher").unwrap();
            let text = render_config_toml(&plan).unwrap();
            let imported: Config = toml::from_str(&text).expect("closure deserializes as a Config");

            // Nothing from the source install survives except the closure:
            // no sibling agents, no channel bindings, no delegate roster.
            assert_eq!(imported.agents.len(), 1, "{text}");
            let agent = &imported.agents["researcher"];
            assert!(agent.channels.is_empty(), "{text}");
            assert!(agent.delegates.is_empty(), "{text}");

            imported.validate().unwrap_or_else(|err| {
                panic!("closure must validate on an empty install: {err}\n{text}")
            });
        }
    }

    /// Every reference family the closure carries, made to dangle one at a
    /// time.
    ///
    /// Each of these is a config that *loads*: `Config::load_or_init` demotes
    /// validation errors to a startup warning so a broken reference never
    /// locks an operator out of the CLI that repairs it, and the exporter is
    /// one of the commands that stays reachable. A bundle built from one
    /// would fail on the importing install, where there is nothing else
    /// configured for the reference to hide behind.
    #[test]
    fn a_dangling_reference_in_any_carried_family_is_refused() {
        type Break = fn(&mut Config);
        let cases: &[(&str, Break, &str)] = &[
            (
                "model provider",
                |config| {
                    config.agents.get_mut("researcher").unwrap().model_provider =
                        "anthropic.gone".into();
                },
                "agents.researcher.model_provider",
            ),
            (
                "classifier provider",
                |config| {
                    config
                        .agents
                        .get_mut("researcher")
                        .unwrap()
                        .classifier_provider = "anthropic.gone".into();
                },
                "agents.researcher.classifier_provider",
            ),
            (
                "summary provider",
                |config| {
                    config
                        .agents
                        .get_mut("researcher")
                        .unwrap()
                        .summary_provider = "anthropic.gone".into();
                },
                "agents.researcher.summary_provider",
            ),
            (
                "tts provider",
                |config| {
                    config.agents.get_mut("researcher").unwrap().tts_provider =
                        "openai.gone".into();
                },
                "agents.researcher.tts_provider",
            ),
            (
                "transcription provider",
                |config| {
                    config
                        .agents
                        .get_mut("researcher")
                        .unwrap()
                        .transcription_provider = "openai.gone".into();
                },
                "agents.researcher.transcription_provider",
            ),
            (
                "risk profile",
                |config| {
                    config.agents.get_mut("researcher").unwrap().risk_profile = "gone".into();
                },
                "agents.researcher.risk_profile",
            ),
            (
                "runtime profile",
                |config| {
                    config.agents.get_mut("researcher").unwrap().runtime_profile = "gone".into();
                },
                "agents.researcher.runtime_profile",
            ),
            (
                "provider named by a carried runtime profile",
                |config| {
                    let mut profile = RuntimeProfileConfig::default();
                    profile.context_compression.summary_provider = "anthropic.gone".into();
                    config
                        .runtime_profiles
                        .insert("standard".to_string(), profile);
                    config.agents.get_mut("researcher").unwrap().runtime_profile =
                        "standard".into();
                },
                "runtime_profiles.standard.context_compression.summary_provider",
            ),
            (
                "skill bundle",
                |config| {
                    config.agents.get_mut("researcher").unwrap().skill_bundles =
                        vec!["gone".to_string()];
                },
                "agents.researcher.skill_bundles[0]",
            ),
            (
                "knowledge bundle",
                |config| {
                    config
                        .agents
                        .get_mut("researcher")
                        .unwrap()
                        .knowledge_bundles = vec!["gone".to_string()];
                },
                "agents.researcher.knowledge_bundles[0]",
            ),
            (
                "mcp bundle",
                |config| {
                    config.agents.get_mut("researcher").unwrap().mcp_bundles =
                        vec!["gone".to_string()];
                },
                "agents.researcher.mcp_bundles[0]",
            ),
        ];

        for (family, break_reference, expected) in cases {
            let mut config = fixture();
            break_reference(&mut config);
            // Planning is not where this is caught: the closure is not final
            // until every filesystem answer has been folded back into it.
            let plan = plan_export(&config, "researcher")
                .unwrap_or_else(|err| panic!("{family}: planning should still succeed: {err}"));

            let err = render_config_toml(&plan).expect_err(&format!(
                "{family}: a dangling reference must refuse the bundle"
            ));
            let text = err.to_string();
            assert!(
                matches!(err, ExportError::ClosureInvalid { .. }),
                "{family}: {text}"
            );
            assert!(text.contains(expected), "{family}: {text}");
        }
    }

    /// The reference that only exists on the carried profile, never on the
    /// agent: without it the closure imports a runtime profile pointing at a
    /// provider the target has never heard of.
    #[test]
    fn a_profile_owned_summary_provider_is_carried_and_validates() {
        let mut config = fixture();
        config.providers.models.anthropic.insert(
            "summarizer".to_string(),
            crate::schema::AnthropicModelProviderConfig::default(),
        );
        let mut profile = RuntimeProfileConfig::default();
        profile.context_compression.summary_provider = "anthropic.summarizer".into();
        config
            .runtime_profiles
            .insert("standard".to_string(), profile);
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.runtime_profile = "standard".into();
        }

        let plan = plan_export(&config, "researcher").unwrap();
        assert!(
            lookup(
                &plan.config,
                &["providers", "models", "anthropic", "summarizer"]
            )
            .is_some(),
            "the profile's own summarizer provider is carried"
        );

        let text = render_config_toml(&plan).unwrap();
        let imported: Config = toml::from_str(&text).unwrap();
        imported.validate().expect("closure validates");
    }

    #[test]
    fn same_profile_delegation_reach_is_closed_and_reported() {
        let plan = plan_export(&fixture(), "researcher").unwrap();

        let agent = lookup(&plan.config, &["agents", "researcher"])
            .and_then(toml::Value::as_table)
            .expect("agent entry present");
        assert_eq!(
            agent
                .get("delegate_same_risk_profile")
                .and_then(toml::Value::as_bool),
            Some(false),
            "the bundle must not arrive with same-profile delegation reach"
        );
        assert!(
            plan.dropped
                .iter()
                .any(|d| d.path == "agents.researcher.delegate_same_risk_profile"),
            "{:?}",
            plan.dropped
        );

        // The closed reach survives the round trip, rather than being pruned
        // back to the schema default of `true`.
        let text = render_config_toml(&plan).unwrap();
        let imported: Config = toml::from_str(&text).unwrap();
        assert!(!imported.agents["researcher"].delegate_same_risk_profile);
        assert!(
            imported
                .reachable_delegate_target_configs("researcher")
                .is_empty()
        );
    }

    /// The loader joins a relative `aieos_path` to the workspace without
    /// rejecting `..`, so a traversing path in a bundle would have the imported
    /// agent read outside its own workspace at startup. It never reaches the
    /// fragment.
    #[test]
    fn an_identity_path_that_escapes_the_workspace_is_dropped() {
        for escaping in [
            "../outside.json",
            "identity/../../outside.json",
            "..//outside.json",
        ] {
            let mut config = fixture();
            if let Some(agent) = config.agents.get_mut("researcher") {
                agent.identity.aieos_path = Some(escaping.to_string());
            }
            let plan = plan_export(&config, "researcher").unwrap();

            let identity = lookup(&plan.config, &["agents", "researcher", "identity"])
                .and_then(toml::Value::as_table);
            assert!(
                identity.is_none_or(|table| !table.contains_key("aieos_path")),
                "{escaping} survived: {identity:?}"
            );
            assert!(plan.identity_document.is_none(), "{escaping}");
            assert!(
                plan.dropped
                    .iter()
                    .any(|d| d.path == "agents.researcher.identity.aieos_path"),
                "{escaping} was dropped silently"
            );

            // What an importer would actually load: no path, so nothing to
            // resolve outside the workspace.
            let text = render_config_toml(&plan).unwrap();
            assert!(!text.contains("outside.json"), "{escaping}: {text}");
            let imported: Config = toml::from_str(&text).unwrap();
            assert_eq!(imported.agents["researcher"].identity.aieos_path, None);
        }
    }

    /// Inside the workspace lexically, but in the half the bundle does not
    /// carry, so the reference would dangle on the target.
    /// A Unix exporter cannot judge a path with the importing host's rules, so
    /// the format's own grammar has to reject anything another platform would
    /// read as a separator, a device, or a traversal.
    #[test]
    fn an_identity_path_that_only_traverses_on_another_platform_is_dropped() {
        for hostile in [
            r"..\outside.json",
            r"identity\..\..\outside.json",
            r"C:\Windows\System32\config\sam",
            "C:/Windows/System32/config/sam",
            r"\\server\share\outside.json",
            "identity/aieos.json:stream",
            "identity/\u{7}bell.json",
        ] {
            let mut config = fixture();
            if let Some(agent) = config.agents.get_mut("researcher") {
                agent.identity.aieos_path = Some(hostile.to_string());
            }
            let plan = plan_export(&config, "researcher").unwrap();

            assert!(plan.identity_document.is_none(), "{hostile:?} was retained");
            let identity = lookup(&plan.config, &["agents", "researcher", "identity"])
                .and_then(toml::Value::as_table);
            assert!(
                identity.is_none_or(|table| !table.contains_key("aieos_path")),
                "{hostile:?} survived: {identity:?}"
            );

            let text = render_config_toml(&plan).unwrap();
            let imported: Config = toml::from_str(&text).unwrap();
            assert_eq!(imported.agents["researcher"].identity.aieos_path, None);
        }
    }

    /// What a retained path is allowed to look like, checked as a grammar so
    /// the guarantee does not depend on the exporting platform.
    #[test]
    fn a_retained_identity_path_resolves_the_same_on_any_platform() {
        let mut config = fixture();
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.identity.aieos_path = Some("identity/aieos.json".to_string());
        }
        let plan = plan_export(&config, "researcher").unwrap();
        let kept = plan.identity_document.as_deref().expect("retained");

        assert!(!kept.contains('\\'), "{kept}");
        assert!(!kept.contains(':'), "{kept}");
        assert!(!kept.starts_with('/'), "{kept}");
        assert!(
            kept.split('/')
                .all(|s| !s.is_empty() && s != "." && s != ".."),
            "{kept}"
        );
        // Resolved with Windows separator rules, it still lands inside the
        // workspace the bundle recreates.
        let windows_joined = format!("workspace\\{}", kept.replace('/', "\\"));
        assert!(!windows_joined.contains("\\..\\"), "{windows_joined}");
    }

    /// The bundle's wire separator is `/` on every exporting platform, and the
    /// rendered config states the path in that grammar rather than as the
    /// operator spelled it. Asserted against literal strings, never through
    /// `Path::display`, so the expectation itself cannot vary by host.
    #[test]
    fn a_retained_identity_path_is_rendered_with_the_bundle_separator() {
        for spelled in ["identity/aieos.json", "./identity/aieos.json"] {
            let mut config = fixture();
            if let Some(agent) = config.agents.get_mut("researcher") {
                agent.identity.aieos_path = Some(spelled.to_string());
            }
            let plan = plan_export(&config, "researcher").unwrap();

            // One string names the file everywhere: the tracked document, the
            // carried config, and the rendered TOML.
            assert_eq!(
                plan.identity_document.as_deref(),
                Some("identity/aieos.json"),
                "{spelled}"
            );
            assert_eq!(
                lookup(
                    &plan.config,
                    &["agents", "researcher", "identity", "aieos_path"]
                )
                .and_then(toml::Value::as_str),
                Some("identity/aieos.json"),
                "{spelled}"
            );
            let text = render_config_toml(&plan).unwrap();
            assert!(
                text.contains(r#"aieos_path = "identity/aieos.json""#),
                "{spelled}: {text}"
            );
            assert!(!text.contains(r"identity\aieos.json"), "{spelled}: {text}");
        }
    }

    /// Edge whitespace is refused rather than trimmed: quietly exporting the
    /// trimmed spelling would have the imported agent load a different file
    /// than the source agent does.
    #[test]
    fn an_identity_path_with_edge_whitespace_is_dropped() {
        for hostile in [
            " identity/aieos.json",
            "identity/aieos.json ",
            "\tidentity/aieos.json",
        ] {
            let mut config = fixture();
            if let Some(agent) = config.agents.get_mut("researcher") {
                agent.identity.aieos_path = Some(hostile.to_string());
            }
            let plan = plan_export(&config, "researcher").unwrap();

            assert!(plan.identity_document.is_none(), "{hostile:?} was retained");
            assert!(
                plan.dropped
                    .iter()
                    .any(|d| d.path == "agents.researcher.identity.aieos_path"),
                "{hostile:?} was dropped silently"
            );
        }
    }

    /// A `..` segment is refused even when it lexically stays inside the
    /// workspace: the loader resolves the path through the filesystem, where
    /// a symlink before the `..` names a different file than the lexical
    /// form the bundle would carry.
    #[test]
    fn an_identity_path_with_an_inner_parent_segment_is_dropped() {
        for hostile in ["link/../identity.json", "identity/sub/../aieos.json"] {
            let mut config = fixture();
            if let Some(agent) = config.agents.get_mut("researcher") {
                agent.identity.aieos_path = Some(hostile.to_string());
            }
            let plan = plan_export(&config, "researcher").unwrap();

            assert!(plan.identity_document.is_none(), "{hostile} was retained");
            let identity = lookup(&plan.config, &["agents", "researcher", "identity"])
                .and_then(toml::Value::as_table);
            assert!(
                identity.is_none_or(|table| !table.contains_key("aieos_path")),
                "{hostile} survived: {identity:?}"
            );
            assert!(
                plan.dropped
                    .iter()
                    .any(|d| d.path == "agents.researcher.identity.aieos_path"),
                "{hostile} was dropped silently"
            );
        }
    }

    #[test]
    fn an_identity_path_inside_the_memory_store_is_dropped() {
        let mut config = fixture();
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.identity.aieos_path = Some("memory/aieos.json".to_string());
        }
        let plan = plan_export(&config, "researcher").unwrap();

        assert!(plan.identity_document.is_none());
        let identity = lookup(&plan.config, &["agents", "researcher", "identity"])
            .and_then(toml::Value::as_table);
        assert!(identity.is_none_or(|table| !table.contains_key("aieos_path")));
    }

    #[test]
    fn a_carried_identity_path_is_tracked_for_the_caller_to_verify() {
        let mut config = fixture();
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.identity.aieos_path = Some("identity/aieos.json".to_string());
        }
        let plan = plan_export(&config, "researcher").unwrap();
        assert_eq!(
            plan.identity_document.as_deref(),
            Some("identity/aieos.json")
        );

        // And dropping it after the copy takes the reference with it.
        let mut plan = plan;
        record_missing_identity_document(&mut plan);
        assert!(plan.identity_document.is_none());
        let identity = lookup(&plan.config, &["agents", "researcher", "identity"])
            .and_then(toml::Value::as_table);
        assert!(identity.is_none_or(|table| !table.contains_key("aieos_path")));
        assert!(
            plan.dropped
                .iter()
                .any(|d| d.reason == DropReason::SourceMissing
                    && d.path == "agents.researcher.identity.aieos_path")
        );
    }

    #[test]
    fn an_absolute_identity_path_is_dropped_and_a_workspace_relative_one_travels() {
        let mut config = fixture();
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.identity.aieos_path = Some("/srv/identities/researcher.json".to_string());
        }
        let plan = plan_export(&config, "researcher").unwrap();
        let identity = lookup(&plan.config, &["agents", "researcher", "identity"])
            .and_then(toml::Value::as_table);
        assert!(
            identity.is_none_or(|table| !table.contains_key("aieos_path")),
            "{identity:?}"
        );
        assert!(
            plan.dropped
                .iter()
                .any(|d| d.path == "agents.researcher.identity.aieos_path"),
            "{:?}",
            plan.dropped
        );

        // A workspace-relative path points into the tree the bundle carries.
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.identity.aieos_path = Some("identity/aieos.json".to_string());
        }
        let plan = plan_export(&config, "researcher").unwrap();
        assert_eq!(
            lookup(
                &plan.config,
                &["agents", "researcher", "identity", "aieos_path"]
            )
            .and_then(toml::Value::as_str),
            Some("identity/aieos.json")
        );
    }

    #[test]
    fn skill_bundle_content_is_planned_for_copy() {
        let mut config = fixture();
        config.skill_bundles.insert(
            "research_tools".to_string(),
            SkillBundleConfig {
                directory: None,
                include: Vec::new(),
                exclude: vec!["internal_only".to_string()],
            },
        );
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.skill_bundles = vec!["research_tools".to_string()];
        }

        let plan = plan_export(&config, "researcher").unwrap();
        assert_eq!(plan.skill_sources.len(), 1);
        let source = &plan.skill_sources[0];
        assert_eq!(source.alias, "research_tools");
        assert_eq!(
            source.source,
            crate::paths::normalize_lexical(
                &config
                    .install_root_dir()
                    .join("shared/skills/research_tools")
            )
        );
        // The copy walks this from the install root, component by component,
        // so it must be the shared-tree path and nothing looser.
        assert_eq!(
            source.relative,
            std::path::Path::new("shared/skills/research_tools")
        );
        assert!(source.filter.admits_skill("web_search"));
        assert!(!source.filter.admits_skill("internal_only"));

        assert!(
            plan.risk_flags
                .iter()
                .any(|f| f.kind == RiskKind::CarriedSkills
                    && f.path == "skill_bundles.research_tools"),
            "{:?}",
            plan.risk_flags
        );

        let provenance = Provenance {
            zeroclaw_version: "0.0.0-test".to_string(),
            exported_at: "2026-08-17T00:00:00Z".to_string(),
        };
        let manifest = render_manifest(&plan, &provenance);
        assert_eq!(
            manifest
                .get("skill_bundles")
                .and_then(toml::Value::as_array)
                .map(|aliases| aliases
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .collect::<Vec<_>>()),
            Some(vec!["research_tools"])
        );
    }

    /// The planner cannot know whether a referenced bundle has content, so the
    /// caller folds that back in before the manifest is rendered.
    #[test]
    fn recording_missing_skill_content_stops_the_manifest_advertising_it() {
        let mut config = fixture();
        config
            .skill_bundles
            .insert("research_tools".to_string(), SkillBundleConfig::default());
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.skill_bundles = vec!["research_tools".to_string()];
        }

        let mut plan = plan_export(&config, "researcher").unwrap();
        assert_eq!(plan.skill_sources.len(), 1);
        assert!(
            plan.risk_flags
                .iter()
                .any(|f| f.kind == RiskKind::CarriedSkills)
        );

        record_missing_skill_content(&mut plan, &["research_tools".to_string()]);

        assert!(plan.skill_sources.is_empty());
        assert!(
            !plan
                .risk_flags
                .iter()
                .any(|f| f.kind == RiskKind::CarriedSkills),
            "a grant for content that is not in the bundle"
        );
        let dropped = plan
            .dropped
            .iter()
            .find(|d| d.path == "skills/research_tools")
            .unwrap_or_else(|| panic!("{:?}", plan.dropped));
        assert_eq!(dropped.reason, DropReason::SourceMissing);

        let manifest = render_manifest(
            &plan,
            &Provenance {
                zeroclaw_version: "0.0.0-test".to_string(),
                exported_at: "2026-08-19T00:00:00Z".to_string(),
            },
        );
        assert_eq!(
            manifest
                .get("skill_bundles")
                .and_then(toml::Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    /// The alias is a config map key and becomes a directory name inside the
    /// bundle. The skill-bundle validator constrains where a bundle's directory
    /// may sit, not what its key may contain.
    #[test]
    fn a_skill_bundle_alias_that_is_not_one_component_is_not_planned() {
        for hostile in [
            "../../outside",
            "..",
            "nested/alias",
            r"..\outside",
            "/absolute",
            "C:evil",
            // Authored on any host, refused for a target this one cannot
            // see: Windows character devices, with and without an
            // extension, and names it would silently strip to something
            // else.
            "con",
            "NUL",
            "aux",
            "prn",
            "com1",
            "LPT9",
            "nul.md",
            "trailing.",
            "quo\"ted",
            "pipe|d",
            "star*",
        ] {
            let mut config = fixture();
            config
                .skill_bundles
                .insert(hostile.to_string(), SkillBundleConfig::default());
            if let Some(agent) = config.agents.get_mut("researcher") {
                agent.skill_bundles = vec![hostile.to_string()];
            }

            let plan = plan_export(&config, "researcher").unwrap();
            assert!(
                plan.skill_sources.is_empty(),
                "{hostile:?} was planned for materialization"
            );
            assert!(
                !plan
                    .risk_flags
                    .iter()
                    .any(|f| f.kind == RiskKind::CarriedSkills),
                "{hostile:?} raised a carried-skills grant"
            );
            assert!(
                plan.dropped
                    .iter()
                    .any(|d| d.path == format!("skill_bundles.{hostile}")),
                "{hostile:?} was dropped silently"
            );
        }
    }

    #[test]
    fn a_plain_skill_bundle_alias_is_planned() {
        assert!(is_safe_path_component("research_tools"));
        assert!(is_safe_path_component("core"));
        assert!(!is_safe_path_component(""));
        assert!(!is_safe_path_component(" spaced"));
        // Whitespace-only differences never reach the planner — the agent's
        // reference list is trimmed before the alias is resolved — so they are
        // pinned against the grammar directly.
        assert!(!is_safe_path_component("trailing "));
    }

    /// The device rule matches names, not substrings. Refusing everything
    /// that merely starts with `com` would cost the format ordinary words for
    /// no safety, so the boundary is pinned from both sides.
    #[test]
    fn only_the_reserved_windows_names_are_refused() {
        for reserved in [
            "con",
            "CON",
            "Con",
            "prn",
            "aux",
            "nul",
            "com0",
            "com1",
            "com9",
            "lpt1",
            "LPT9",
            "con.md",
            "nul.json",
            "com1.tar.gz",
        ] {
            assert!(
                !is_safe_path_component(reserved),
                "{reserved:?} is reserved on Windows"
            );
        }
        for ordinary in [
            "console",
            "com",
            "com10",
            "communication",
            "lpt",
            "auxiliary",
            "nullable",
            "conf.d",
            "my.con",
            "research.tools",
        ] {
            assert!(
                is_safe_path_component(ordinary),
                "{ordinary:?} is an ordinary name"
            );
        }
    }

    /// The same grammar governs the other name the format controls. A path
    /// whose component a target cannot open is dropped with its reason rather
    /// than written into a bundle that cannot be loaded there.
    #[test]
    fn an_identity_path_naming_a_windows_device_is_dropped() {
        for hostile in [
            "nul",
            "identity/con.json",
            "identity/lpt1.json",
            "identity/report.",
            "identity/report ",
            "identity/pipe|d.json",
        ] {
            let mut config = fixture();
            if let Some(agent) = config.agents.get_mut("researcher") {
                agent.identity.aieos_path = Some(hostile.to_string());
            }
            let plan = plan_export(&config, "researcher").unwrap();

            assert!(plan.identity_document.is_none(), "{hostile:?} was retained");
            let dropped = plan
                .dropped
                .iter()
                .find(|d| d.path == "agents.researcher.identity.aieos_path")
                .unwrap_or_else(|| panic!("{hostile:?} was dropped silently"));
            assert!(
                dropped
                    .detail
                    .contains("every supported target can materialize"),
                "{hostile:?}: {}",
                dropped.detail
            );
            // The reference does not survive into the closure either.
            assert!(
                lookup(
                    &plan.config,
                    &["agents", "researcher", "identity", "aieos_path"]
                )
                .is_none(),
                "{hostile:?} survived in the closure"
            );
        }
    }

    /// An absolute directory names this host twice over: the path cannot
    /// travel, and unless it sits inside this install's own shared tree the
    /// content is not the install's to export either.
    #[test]
    fn an_absolute_skill_bundle_directory_outside_shared_is_dropped_with_its_content() {
        let mut config = fixture();
        config.skill_bundles.insert(
            "research_tools".to_string(),
            SkillBundleConfig {
                directory: Some("/srv/zeroclaw/shared/skills/pool".to_string()),
                ..Default::default()
            },
        );
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.skill_bundles = vec!["research_tools".to_string()];
        }

        let plan = plan_export(&config, "researcher").unwrap();
        let bundle = lookup(&plan.config, &["skill_bundles", "research_tools"])
            .and_then(toml::Value::as_table)
            .expect("bundle carried");
        assert!(!bundle.contains_key("directory"), "{bundle:?}");
        assert!(
            plan.dropped
                .iter()
                .any(|d| d.path == "skill_bundles.research_tools.directory"),
            "{:?}",
            plan.dropped
        );

        // The directory sits outside this install's shared tree, so no copy
        // source is planned for it and the operator hears the content stayed.
        assert!(plan.skill_sources.is_empty(), "{:?}", plan.skill_sources);
        assert!(
            plan.dropped
                .iter()
                .any(|d| d.path == "skills/research_tools" && d.reason == DropReason::HostSpecific),
            "{:?}",
            plan.dropped
        );
        assert!(
            !plan
                .risk_flags
                .iter()
                .any(|f| f.kind == RiskKind::CarriedSkills),
            "a grant for content that is not in the bundle"
        );

        // And the target validates the bundle against its own install root.
        let text = render_config_toml(&plan).unwrap();
        let imported: Config = toml::from_str(&text).unwrap();
        imported
            .validate()
            .expect("a bundle directory the target owns validates");
    }

    /// A relative directory that lexically escapes `<install>/shared/` is the
    /// in-install redirect shape: it stays a valid-looking config entry while
    /// naming a tree the skill-bundle contract does not cover. A `..` that
    /// lexically stays inside `shared/` is refused with it: the runtime
    /// resolves the configured path through the filesystem, where a symlink
    /// before the `..` lands somewhere the lexical form does not name.
    #[test]
    fn a_skill_bundle_directory_escaping_shared_is_dropped_with_its_content() {
        for escaping in [
            "shared/../private-skills",
            "agents/researcher/workspace",
            "shared/missing/../pool",
            "shared/skills/staging/../pool",
        ] {
            let mut config = fixture();
            config.skill_bundles.insert(
                "research_tools".to_string(),
                SkillBundleConfig {
                    directory: Some(escaping.to_string()),
                    ..Default::default()
                },
            );
            if let Some(agent) = config.agents.get_mut("researcher") {
                agent.skill_bundles = vec!["research_tools".to_string()];
            }

            let plan = plan_export(&config, "researcher").unwrap();
            assert!(
                plan.skill_sources.is_empty(),
                "{escaping}: {:?}",
                plan.skill_sources
            );
            let bundle = lookup(&plan.config, &["skill_bundles", "research_tools"])
                .and_then(toml::Value::as_table)
                .expect("bundle carried");
            assert!(!bundle.contains_key("directory"), "{escaping}: {bundle:?}");
            assert!(
                plan.dropped
                    .iter()
                    .any(|d| d.path == "skills/research_tools"),
                "{escaping} was dropped silently: {:?}",
                plan.dropped
            );
            assert!(
                !plan
                    .risk_flags
                    .iter()
                    .any(|f| f.kind == RiskKind::CarriedSkills),
                "{escaping} raised a carried-skills grant"
            );

            // The carried config must validate on the target without the
            // escaping path.
            let text = render_config_toml(&plan).unwrap();
            let imported: Config = toml::from_str(&text).unwrap();
            imported
                .validate()
                .expect("a bundle without the escaping directory validates");
        }
    }

    /// A relative directory deeper inside the shared tree is the operator
    /// arranging their install, not an escape: the content travels, and the
    /// copy walks the configured components.
    #[test]
    fn a_relative_skill_bundle_directory_inside_shared_is_planned() {
        let mut config = fixture();
        config.skill_bundles.insert(
            "research_tools".to_string(),
            SkillBundleConfig {
                directory: Some("shared/skills/pool/research".to_string()),
                ..Default::default()
            },
        );
        if let Some(agent) = config.agents.get_mut("researcher") {
            agent.skill_bundles = vec!["research_tools".to_string()];
        }

        let plan = plan_export(&config, "researcher").unwrap();
        assert_eq!(plan.skill_sources.len(), 1);
        assert_eq!(
            plan.skill_sources[0].relative,
            std::path::Path::new("shared/skills/pool/research")
        );
        // The relative directory is portable, so it is carried as configured.
        assert_eq!(
            lookup(
                &plan.config,
                &["skill_bundles", "research_tools", "directory"]
            )
            .and_then(toml::Value::as_str),
            Some("shared/skills/pool/research")
        );
    }

    #[test]
    fn closure_parses_back_into_a_config_that_names_the_agent() {
        let plan = plan_export(&fixture(), "researcher").unwrap();
        let text = render_config_toml(&plan).unwrap();
        let reparsed: Config = toml::from_str(&text).expect("closure deserializes as a Config");
        let agent = reparsed.agents.get("researcher").expect("agent present");
        assert_eq!(agent.model_provider.as_str(), "anthropic.main");
        assert_eq!(agent.risk_profile.as_str(), "guarded");
        assert!(agent.channels.is_empty(), "channels were dropped");
        assert!(reparsed.risk_profiles.contains_key("guarded"));
        assert!(reparsed.mcp_bundles.contains_key("research"));
    }

    #[test]
    fn ciphertext_that_escapes_scrubbing_aborts_the_export() {
        let mut root = toml::Value::Table(toml::Table::new());
        if let toml::Value::Table(table) = &mut root {
            let mut inner = toml::Table::new();
            inner.insert(
                "api_key".to_string(),
                toml::Value::String("enc2:deadbeef".to_string()),
            );
            table.insert("providers".to_string(), toml::Value::Table(inner));
        }
        assert_eq!(
            find_ciphertext(&root, "").as_deref(),
            Some("providers.api_key")
        );
    }

    #[test]
    fn bundle_aliases_are_deduplicated() {
        assert_eq!(
            dedup(&[
                "a".to_string(),
                " a ".to_string(),
                String::new(),
                "b".to_string()
            ]),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
