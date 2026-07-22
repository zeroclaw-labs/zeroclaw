pub mod skill_http;
pub mod skill_tool;
use anyhow::{Context, Result};
use directories::UserDirs;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

pub mod audit;
pub mod bundle;
pub mod cache;
pub mod constants;
pub mod creator;
pub mod document;
pub mod frontmatter;
pub mod improver;
pub mod reference;
pub mod review;
pub mod scaffold;
pub mod service;
mod suggestions;
pub mod testing;

pub use bundle::{BundleError, BundleSummary};
pub use document::{DocumentParseError, SkillDocument};
pub use frontmatter::SkillFrontmatter;
pub use reference::{SkillRef, SkillRefError};
pub use scaffold::{ScaffoldError, ScaffoldOptions};
pub use service::{
    EffectiveSkill, EffectiveSkillSet, RemoveMode, ServiceError, SkillOrigin, SkillSummary,
    SkillsService,
};
pub(crate) use suggestions::render_missing_skill_install_suggestion;

const OPEN_SKILLS_REPO_URL: &str = "https://github.com/besoeasy/open-skills";
const OPEN_SKILLS_SYNC_MARKER: &str = ".zeroclaw-open-skills-sync";
const OPEN_SKILLS_SYNC_INTERVAL_SECS: u64 = 60 * 60 * 24 * 7;

// ─── Skills registry (zeroclaw-skills) ────────────────────────────────────────
const SKILLS_REGISTRY_REPO_URL: &str = "https://github.com/zeroclaw-labs/zeroclaw-skills";
const SKILLS_REGISTRY_DIR_NAME: &str = "skills-registry";
const SKILLS_REGISTRY_SYNC_MARKER: &str = ".zeroclaw-skills-registry-sync";
const SKILLS_REGISTRY_SYNC_INTERVAL_SECS: u64 = 60 * 60 * 24;

// ─── Extra (user-configured) registries ──────────────────────────────────────
/// Each `[[skills.extra_registries]]` entry is cloned to its own
/// `<workspace>/extra-registry-<name>/` directory, reusing the same git
/// clone/pull/sync machinery as the default skills registry.
const EXTRA_REGISTRY_DIR_PREFIX: &str = "extra-registry-";

/// A skill is a user-defined or community-built capability.
/// Skills live in `~/.zeroclaw/workspace/skills/<name>/SKILL.md`
/// and can include tool definitions, prompts, and automation scripts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Per-locale translations of `description`, keyed by Discord locale code
    /// (e.g. `fr`, `es-ES`, `ja`). Consumed by slash-capable channels to
    /// localize the command description; empty for unlocalized skills. Declared
    /// in SKILL.toml under `[skill]` as `description_localizations`.
    #[serde(default)]
    pub description_localizations: BTreeMap<String, String>,
    pub version: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tools: Vec<SkillTool>,
    #[serde(default)]
    pub prompts: Vec<String>,
    /// Typed slash-command options a `slash`-tagged skill exposes (e.g. on
    /// Discord). Empty for skills that take no structured input — slash channels
    /// then fall back to a single free-text option. See [`SkillSlashOption`].
    #[serde(default)]
    pub slash_options: Vec<SkillSlashOption>,
    #[serde(skip)]
    pub location: Option<PathBuf>,
}

/// Why the audited resolver dropped a candidate skill directory/file.
/// Carries the human-readable detail the loader already logs, so the
/// dashboard can show the same reason without re-running the audit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillDropReason {
    /// `audit_*` returned Ok(report) with findings. `summary` = report.summary();
    /// `scripts_blocked` is true when the secure-default script policy is the
    /// blocker, so consumers can offer the `skills.allow_scripts = true` hint
    /// without re-parsing the human-readable summary.
    AuditFindings {
        summary: String,
        scripts_blocked: bool,
    },
    /// `audit_*` returned Err (unauditable); String = error message.
    AuditError(String),
    ManifestParseError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DroppedSkill {
    pub name: String,
    /// `"workspace"` | `"open-skills"` | `"plugin"` | `"bundle"`.
    pub origin_hint: String,
    pub reason: SkillDropReason,
    pub location: Option<PathBuf>,
}

/// One lower-precedence skill that lost its name to an earlier (higher-priority)
/// source during the agent's effective-skill dedup. Recorded for the dashboard
/// so operators can see why an assigned bundle skill is being overridden.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowedSkill {
    /// The name shared with (and won by) the higher-precedence skill.
    pub name: String,
    /// Origin of the LOSER: `"open-skills"` | `"plugin"` | `"bundle"`.
    pub origin_hint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlashOptionKind {
    String,
    Integer,
    Number,
    Boolean,
    User,
    Channel,
    Role,
    Mentionable,
}

impl SlashOptionKind {
    /// Every kind, in the order surfaces should offer them. Walked (not
    /// restated) by every registry consumer.
    pub const ALL: [Self; 8] = [
        Self::String,
        Self::Integer,
        Self::Number,
        Self::Boolean,
        Self::User,
        Self::Channel,
        Self::Role,
        Self::Mentionable,
    ];

    /// The canonical `type` token written in frontmatter.
    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::User => "user",
            Self::Channel => "channel",
            Self::Role => "role",
            Self::Mentionable => "mentionable",
        }
    }

    /// Predefined `choices` apply only to string/integer/number options.
    pub fn supports_choices(self) -> bool {
        match self {
            Self::String | Self::Integer | Self::Number => true,
            Self::Boolean | Self::User | Self::Channel | Self::Role | Self::Mentionable => false,
        }
    }

    /// `min`/`max` numeric bounds apply only to integer/number options.
    pub fn supports_numeric_bounds(self) -> bool {
        match self {
            Self::Integer | Self::Number => true,
            Self::String
            | Self::Boolean
            | Self::User
            | Self::Channel
            | Self::Role
            | Self::Mentionable => false,
        }
    }

    /// `min_length`/`max_length` bounds apply only to string options.
    pub fn supports_length_bounds(self) -> bool {
        match self {
            Self::String => true,
            Self::Integer
            | Self::Number
            | Self::Boolean
            | Self::User
            | Self::Channel
            | Self::Role
            | Self::Mentionable => false,
        }
    }

    /// The wire-facing capability row for this kind, consumed by API surfaces.
    pub fn descriptor(self) -> SlashOptionKindDescriptor {
        SlashOptionKindDescriptor {
            manifest_name: self.manifest_name().to_string(),
            supports_choices: self.supports_choices(),
            supports_numeric_bounds: self.supports_numeric_bounds(),
            supports_length_bounds: self.supports_length_bounds(),
        }
    }
}

/// Serialized capability row for one [`SlashOptionKind`], as published to
/// surfaces (the web dashboard mirrors this shape). Built by walking
/// [`SlashOptionKind::ALL`]; never hand-authored.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SlashOptionKindDescriptor {
    pub manifest_name: String,
    pub supports_choices: bool,
    pub supports_numeric_bounds: bool,
    pub supports_length_bounds: bool,
}

/// The full registry, produced by exhaustively walking [`SlashOptionKind::ALL`].
pub fn slash_option_kinds() -> Vec<SlashOptionKindDescriptor> {
    SlashOptionKind::ALL
        .into_iter()
        .map(SlashOptionKind::descriptor)
        .collect()
}

/// A typed option a `slash`-tagged skill exposes on its slash command. Shaped
/// after the Discord Application Command Option model but channel-agnostic; a
/// slash-capable channel maps `kind` to its wire option type. Declared in
/// SKILL.toml under `[[skill.slash_options]]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillSlashOption {
    pub name: String,
    pub description: String,
    /// Per-locale translations of `description`, keyed by Discord locale code.
    /// Empty for unlocalized options. Declared under
    /// `[[skill.slash_options]]` as `description_localizations`.
    #[serde(default)]
    pub description_localizations: BTreeMap<String, String>,
    /// `string` | `integer` | `number` | `boolean` | `user` | `channel` |
    /// `role` | `mentionable`. Unknown values are dropped by the channel.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    /// Predefined choices (string/integer/number options only). The `value` is
    /// kept as text and coerced to the option's type by the channel.
    #[serde(default)]
    pub choices: Vec<SkillSlashChoice>,
    /// Inclusive bounds for integer/number options.
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    /// Length bounds for string options.
    #[serde(default)]
    pub min_length: Option<u32>,
    #[serde(default)]
    pub max_length: Option<u32>,
}

/// A predefined choice for a typed slash option.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillSlashChoice {
    pub name: String,
    pub value: String,
}

impl ::zeroclaw_api::attribution::Attributable for Skill {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Skill
    }
    fn alias(&self) -> &str {
        &self.name
    }
}

/// A tool defined by a skill (shell command, HTTP call, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTool {
    pub name: String,
    pub description: String,
    /// "shell", "http", "script", "builtin", "mcp"
    pub kind: String,
    /// The command/URL/script to execute (unused for builtin/mcp kinds)
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: HashMap<String, String>,
    /// For `kind = "builtin"`: the name of the built-in tool to delegate to.
    /// For `kind = "mcp"`: the prefixed MCP tool name `{server}__{tool}`
    /// (e.g. `images__generate`).
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default, alias = "default_args")]
    pub locked_args: HashMap<String, String>,
    /// For `kind = "shell"` / `kind = "script"`: maximum execution time in
    /// seconds before the command is killed. Unset falls back to the built-in
    /// `SKILL_SHELL_TIMEOUT_SECS` (60s) default; long-running skills (e.g. a
    /// build pipeline) raise it via `timeout_secs` in SKILL.toml.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Skill manifest parsed from SKILL.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillManifest {
    skill: SkillMeta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forge: Option<ForgeMetadata>,
    #[serde(default)]
    tools: Vec<SkillTool>,
    #[serde(default)]
    prompts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillMeta {
    name: String,
    description: String,
    #[serde(default)]
    description_localizations: BTreeMap<String, String>,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    prompts: Vec<String>,
    #[serde(default)]
    slash_options: Vec<SkillSlashOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgeMetadata {
    /// Upstream URL the skill was integrated from.
    #[serde(default)]
    source: Option<String>,
    /// Upstream owner (GitHub user / org).
    #[serde(default)]
    owner: Option<String>,
    /// Primary language reported by the source (or `"unknown"`).
    #[serde(default)]
    language: Option<String>,
    /// `true` if the upstream repo carries a license file.
    #[serde(default)]
    license: Option<bool>,
    /// Upstream star count at integration time.
    #[serde(default)]
    stars: Option<u64>,
    /// Upstream `updated_at` timestamp formatted `YYYY-MM-DD`, or
    /// `"unknown"` if the integrator could not resolve one.
    #[serde(default)]
    updated_at: Option<String>,
    /// Runtime/version requirements declared by the integrator.
    #[serde(default)]
    requirements: BTreeMap<String, toml::Value>,
    #[serde(default)]
    metadata: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Default)]
struct SkillMarkdownMeta {
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
    author: Option<String>,
    tags: Vec<String>,
    /// Typed slash-command options from the nested `slash_options:` frontmatter
    /// block. Parsed by the shared helper in `document` (not the flat scanner)
    /// so a SKILL.md skill can drive native Discord slash commands — parity with
    /// SKILL.toml's `[[skill.slash_options]]`.
    slash_options: Vec<SkillSlashOption>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillTier {
    Official,
    Community,
    Featured,
    Unknown,
}

#[derive(Debug, Deserialize)]
struct RegistryIndex {
    #[serde(default)]
    skills: Vec<RegistryEntry>,
}

#[derive(Debug, Deserialize)]
struct RegistryEntry {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

fn tier_from_tags(tags: &[String]) -> SkillTier {
    let has = |needle: &str| tags.iter().any(|t| t.eq_ignore_ascii_case(needle));
    if has("Official") {
        SkillTier::Official
    } else if has("Community") {
        SkillTier::Community
    } else if has("Featured") {
        SkillTier::Featured
    } else {
        SkillTier::Unknown
    }
}

/// Look up a skill in `<registry_dir>/registry.json` and return its trust tier
/// and version. Returns `(SkillTier::Unknown, None)` if the index file is
/// missing, malformed, or does not list the skill.
pub fn lookup_registry_skill_tier(registry_dir: &Path, name: &str) -> (SkillTier, Option<String>) {
    let path = registry_dir.join("registry.json");
    let Ok(data) = std::fs::read_to_string(&path) else {
        return (SkillTier::Unknown, None);
    };
    let Ok(index) = serde_json::from_str::<RegistryIndex>(&data) else {
        return (SkillTier::Unknown, None);
    };
    let Some(entry) = index.skills.into_iter().find(|e| e.name == name) else {
        return (SkillTier::Unknown, None);
    };
    (tier_from_tags(&entry.tags), entry.version)
}

fn install_tier_banner_key(tier: SkillTier) -> &'static str {
    match tier {
        SkillTier::Official => "cli-skills-install-tier-official",
        SkillTier::Community | SkillTier::Featured | SkillTier::Unknown => {
            "cli-skills-install-tier-community"
        }
    }
}

pub fn build_install_tier_banner(name: &str, version: Option<&str>, tier: SkillTier) -> String {
    let version_label = version.unwrap_or("?");
    let args = [("name", name), ("version", version_label)];
    let key = install_tier_banner_key(tier);
    let mut banner = crate::i18n::get_required_cli_string_with_args(key, &args);
    if !banner.ends_with('\n') {
        banner.push('\n');
    }
    banner
}

/// Print the install-time tier banner to stdout.
pub fn print_install_tier_banner(name: &str, version: Option<&str>, tier: SkillTier) {
    print!("{}", build_install_tier_banner(name, version, tier));
}

/// Emit a user-visible warning when a skill directory is skipped due to audit
/// findings. When `scripts_blocked` is set and `allow_scripts` is `false`, the
/// message includes actionable remediation guidance so users know how to enable
/// their skill.
fn warn_skipped_skill(path: &Path, summary: &str, scripts_blocked: bool, allow_scripts: bool) {
    if scripts_blocked && !allow_scripts {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "skipping skill directory {}: {summary}. \
             To allow script files in skills, set `skills.allow_scripts = true` in your config.",
                path.display().to_string()
            )
        );
        eprintln!(
            "warning: skill '{}' was skipped because it contains script files. \
             Set `skills.allow_scripts = true` in your zeroclaw config to enable it.",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        );
    } else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "skipping insecure skill directory {}: {summary}",
                path.display().to_string()
            )
        );
    }
}

fn warn_metadata_drift(skill_dir: &Path, toml_skill: &Skill, md_path: &Path) {
    if !md_path.exists() {
        return;
    }
    let Ok(md_content) = std::fs::read_to_string(md_path) else {
        return;
    };
    let parsed = parse_skill_markdown(&md_content);
    let dir_name = skill_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");

    if let Some(ref md_name) = parsed.meta.name
        && md_name != &toml_skill.name
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "skill '{}': name mismatch between TOML ('{}') and SKILL.md ('{}')",
                dir_name, toml_skill.name, md_name
            )
        );
    }
    if let Some(ref md_desc) = parsed.meta.description {
        let md_desc = md_desc.trim();
        if !md_desc.is_empty() && md_desc != ">-" && md_desc != toml_skill.description.trim() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "skill '{}': description mismatch between TOML and SKILL.md — TOML takes precedence",
                    dir_name
                )
            );
        }
    }
}

/// Infer the directory/file stem a dropped/loaded skill is named after when its
/// manifest can't be (or wasn't) read.
fn dir_stem(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Load all skills from the workspace skills directory
pub fn load_skills(workspace_dir: &Path) -> Vec<Skill> {
    load_skills_with_open_skills_config(workspace_dir, None, None, None).0
}

/// Load skills using runtime config values (preferred at runtime).
pub fn load_skills_with_config(
    workspace_dir: &Path,
    config: &zeroclaw_config::schema::Config,
) -> Vec<Skill> {
    load_skills_with_config_audited(workspace_dir, config).0
}

/// Like [`load_skills_with_config`] but also returns the audit-dropped
/// candidates the resolver skipped, so the dashboard can surface them
pub fn load_skills_with_config_audited(
    workspace_dir: &Path,
    config: &zeroclaw_config::schema::Config,
) -> (Vec<Skill>, Vec<DroppedSkill>) {
    #[allow(unused_mut)]
    let (mut skills, mut dropped) = load_skills_with_open_skills_config(
        workspace_dir,
        Some(config.skills.open_skills_enabled),
        config.skills.open_skills_dir.as_deref(),
        Some(config.skills.allow_scripts),
    );

    #[cfg(feature = "plugins-wasm")]
    {
        let (plugin_skills, plugin_dropped) = load_plugin_skills_from_config(config);
        skills.extend(plugin_skills);
        dropped.extend(plugin_dropped);
    }

    (skills, dropped)
}

pub fn load_skills_for_agent(
    workspace_dir: &Path,
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> Vec<Skill> {
    load_skills_for_agent_audited(workspace_dir, config, agent_alias).0
}

fn origin_hint_of(skill: &Skill) -> &'static str {
    if skill.tags.iter().any(|t| t == "open-skills") {
        "open-skills"
    } else if skill.name.starts_with("plugin:")
        || skill.tags.iter().any(|t| t.starts_with("plugin:"))
    {
        "plugin"
    } else {
        "workspace"
    }
}

/// [`load_skills_for_agent`] plus the audit-dropped and shadowed candidates the
/// resolver skipped, so the dashboard can surface them without re-auditing or
/// re-walking
pub fn load_skills_for_agent_audited(
    workspace_dir: &Path,
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> (Vec<Skill>, Vec<DroppedSkill>, Vec<ShadowedSkill>) {
    let (mut skills, mut dropped) = load_skills_with_config_audited(workspace_dir, config);
    let mut shadows: Vec<ShadowedSkill> = Vec::new();
    let Some(agent) = config.agent(agent_alias) else {
        return (skills, dropped, shadows);
    };
    if agent.skill_bundles.is_empty() {
        return (skills, dropped, shadows);
    }
    let install_root = config.install_root_dir();
    let allow_scripts = config.skills.allow_scripts;
    // name → origin_hint of the winner already in `skills`, so a shadowed
    // bundle skill can be attributed to the source that beat it.
    let mut seen: std::collections::HashMap<String, &'static str> = skills
        .iter()
        .map(|s| (s.name.clone(), origin_hint_of(s)))
        .collect();
    for bundle_alias in &agent.skill_bundles {
        let bundle = match config.skill_bundles.get(bundle_alias) {
            Some(b) => b,
            None => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": agent_alias, "bundle": bundle_alias, "bundle_alias": bundle_alias})), "skipping skill bundle: [skill_bundles.] is not configured");
                continue;
            }
        };
        let dir = match zeroclaw_config::skill_bundles::resolve_directory(
            config,
            &install_root,
            bundle_alias,
        ) {
            Ok(d) => d,
            Err(e) => {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": agent_alias, "bundle": bundle_alias, "e": e.to_string()})), "skipping skill bundle: ");
                continue;
            }
        };
        let (bundle_skills, bundle_dropped) = load_skills_from_directory(&dir, allow_scripts);
        dropped.extend(bundle_dropped.into_iter().map(|mut d| {
            d.origin_hint = "bundle".into();
            d
        }));
        for skill in bundle_skills {
            if !bundle.admits_skill(&skill.name) {
                continue;
            }
            // First-write wins so workspace skills override bundle skills
            // with the same name (legacy agents who edited a workspace
            // copy keep their override after a bundle is assigned).
            if seen.contains_key(&skill.name) {
                // This bundle skill lost the name to an earlier source.
                // Record the loser keyed to the winner's name so the
                // dashboard can badge the winning skill.
                shadows.push(ShadowedSkill {
                    name: skill.name.clone(),
                    origin_hint: "bundle".into(),
                });
            } else {
                seen.insert(skill.name.clone(), "bundle");
                skills.push(skill);
            }
        }
    }
    (skills, dropped, shadows)
}

pub fn load_skills_for_agent_from_config(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> Vec<Skill> {
    load_skills_for_agent_from_config_audited(config, agent_alias).0
}

/// [`load_skills_for_agent_from_config`] plus the audit-dropped and shadowed
/// candidates the resolver skipped — the dashboard's source for the
/// skipped-audit banner and shadow badges
pub fn load_skills_for_agent_from_config_audited(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> (Vec<Skill>, Vec<DroppedSkill>, Vec<ShadowedSkill>) {
    load_skills_for_agent_audited(
        &config.agent_workspace_dir(agent_alias),
        config,
        agent_alias,
    )
}

/// Load skills using explicit open-skills settings.
pub fn load_skills_with_open_skills_settings(
    workspace_dir: &Path,
    open_skills_enabled: bool,
    open_skills_dir: Option<&str>,
    allow_scripts: bool,
) -> Vec<Skill> {
    load_skills_with_open_skills_config(
        workspace_dir,
        Some(open_skills_enabled),
        open_skills_dir,
        Some(allow_scripts),
    )
    .0
}

fn load_skills_with_open_skills_config(
    workspace_dir: &Path,
    config_open_skills_enabled: Option<bool>,
    config_open_skills_dir: Option<&str>,
    config_allow_scripts: Option<bool>,
) -> (Vec<Skill>, Vec<DroppedSkill>) {
    let mut skills = Vec::new();
    let mut dropped = Vec::new();
    let allow_scripts = config_allow_scripts.unwrap_or(false);

    if let Some(open_skills_dir) =
        ensure_open_skills_repo(config_open_skills_enabled, config_open_skills_dir)
    {
        let (os_skills, os_dropped) = load_open_skills(&open_skills_dir, allow_scripts);
        skills.extend(os_skills);
        dropped.extend(os_dropped);
    }

    let (ws_skills, ws_dropped) = load_workspace_skills(workspace_dir, allow_scripts);
    skills.extend(ws_skills);
    dropped.extend(ws_dropped);
    (skills, dropped)
}

fn load_workspace_skills(
    workspace_dir: &Path,
    allow_scripts: bool,
) -> (Vec<Skill>, Vec<DroppedSkill>) {
    let skills_dir = workspace_dir.join("skills");
    load_skills_from_directory(&skills_dir, allow_scripts)
}

pub fn load_skills_from_directory(
    skills_dir: &Path,
    allow_scripts: bool,
) -> (Vec<Skill>, Vec<DroppedSkill>) {
    let out = cache::cached_load(skills_dir, allow_scripts, "workspace", || {
        let (skills, dropped) = load_skills_from_directory_uncached(skills_dir, allow_scripts);
        cache::LoadOutput { skills, dropped }
    });
    (out.skills, out.dropped)
}

fn load_skills_from_directory_uncached(
    skills_dir: &Path,
    allow_scripts: bool,
) -> (Vec<Skill>, Vec<DroppedSkill>) {
    let mut skills = Vec::new();
    let mut dropped = Vec::new();
    if !skills_dir.exists() {
        return (skills, dropped);
    }

    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return (skills, dropped);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        match audit::audit_skill_directory_with_options(
            &path,
            audit::SkillAuditOptions { allow_scripts },
        ) {
            Ok(report) if report.is_clean() => {}
            Ok(report) => {
                let summary = report.summary();
                let scripts_blocked = report.scripts_blocked;
                warn_skipped_skill(&path, &summary, scripts_blocked, allow_scripts);
                dropped.push(DroppedSkill {
                    name: dir_stem(&path),
                    origin_hint: "workspace".into(),
                    reason: SkillDropReason::AuditFindings {
                        summary,
                        scripts_blocked,
                    },
                    location: Some(path.clone()),
                });
                continue;
            }
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "skipping unauditable skill directory {}: {err}",
                        path.display().to_string()
                    )
                );
                dropped.push(DroppedSkill {
                    name: dir_stem(&path),
                    origin_hint: "workspace".into(),
                    reason: SkillDropReason::AuditError(err.to_string()),
                    location: Some(path.clone()),
                });
                continue;
            }
        }

        // Try SKILL.toml first, then manifest.toml (registry format), then SKILL.md
        let skill_toml_path = path.join("SKILL.toml");
        let manifest_toml_path = path.join("manifest.toml");
        let md_path = path.join("SKILL.md");

        let toml_path = if skill_toml_path.exists() {
            Some(skill_toml_path)
        } else if manifest_toml_path.exists() {
            Some(manifest_toml_path)
        } else {
            None
        };

        if let Some(toml_path) = toml_path {
            match load_skill_toml(&toml_path) {
                Ok(skill) => {
                    warn_metadata_drift(&path, &skill, &md_path);
                    skills.push(skill);
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "path": toml_path.display().to_string(),
                                "error": format!("{}", e),
                            })),
                        "failed to load SKILL.toml — skill directory skipped"
                    );
                    dropped.push(DroppedSkill {
                        name: dir_stem(&path),
                        origin_hint: "workspace".into(),
                        reason: SkillDropReason::ManifestParseError(format!("{e}")),
                        location: Some(path.clone()),
                    });
                }
            }
        } else if md_path.exists()
            && let Ok(skill) = load_skill_md(&md_path, &path)
        {
            skills.push(skill);
        }
    }

    (skills, dropped)
}

fn finalize_open_skill(mut skill: Skill) -> Skill {
    if !skill.tags.iter().any(|tag| tag == "open-skills") {
        skill.tags.push("open-skills".to_string());
    }
    if skill.author.is_none() {
        skill.author = Some("besoeasy/open-skills".to_string());
    }
    skill
}

fn load_open_skills_from_directory(
    skills_dir: &Path,
    allow_scripts: bool,
) -> (Vec<Skill>, Vec<DroppedSkill>) {
    let out = cache::cached_load(skills_dir, allow_scripts, "open-skills", || {
        let (skills, dropped) = load_open_skills_from_directory_uncached(skills_dir, allow_scripts);
        cache::LoadOutput { skills, dropped }
    });
    (out.skills, out.dropped)
}

fn load_open_skills_from_directory_uncached(
    skills_dir: &Path,
    allow_scripts: bool,
) -> (Vec<Skill>, Vec<DroppedSkill>) {
    let mut skills = Vec::new();
    let mut dropped = Vec::new();
    if !skills_dir.exists() {
        return (skills, dropped);
    }

    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return (skills, dropped);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        match audit::audit_skill_directory_with_options(
            &path,
            audit::SkillAuditOptions { allow_scripts },
        ) {
            Ok(report) if report.is_clean() => {}
            Ok(report) => {
                let summary = report.summary();
                let scripts_blocked = report.scripts_blocked;
                warn_skipped_skill(&path, &summary, scripts_blocked, allow_scripts);
                dropped.push(DroppedSkill {
                    name: dir_stem(&path),
                    origin_hint: "open-skills".into(),
                    reason: SkillDropReason::AuditFindings {
                        summary,
                        scripts_blocked,
                    },
                    location: Some(path.clone()),
                });
                continue;
            }
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "skipping unauditable open-skill directory {}: {err}",
                        path.display().to_string()
                    )
                );
                dropped.push(DroppedSkill {
                    name: dir_stem(&path),
                    origin_hint: "open-skills".into(),
                    reason: SkillDropReason::AuditError(err.to_string()),
                    location: Some(path.clone()),
                });
                continue;
            }
        }

        let skill_toml_path = path.join("SKILL.toml");
        let manifest_toml_path = path.join("manifest.toml");
        let md_path = path.join("SKILL.md");

        let toml_path = if skill_toml_path.exists() {
            Some(skill_toml_path)
        } else if manifest_toml_path.exists() {
            Some(manifest_toml_path)
        } else {
            None
        };

        if let Some(toml_path) = toml_path {
            match load_skill_toml(&toml_path) {
                Ok(skill) => {
                    warn_metadata_drift(&path, &skill, &md_path);
                    skills.push(finalize_open_skill(skill));
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "path": toml_path.display().to_string(),
                                "error": format!("{}", e),
                            })),
                        "failed to load SKILL.toml — skill directory skipped"
                    );
                    dropped.push(DroppedSkill {
                        name: dir_stem(&path),
                        origin_hint: "open-skills".into(),
                        reason: SkillDropReason::ManifestParseError(format!("{e}")),
                        location: Some(path.clone()),
                    });
                }
            }
        } else if md_path.exists()
            && let Ok(skill) = load_open_skill_md(&md_path)
        {
            skills.push(skill);
        }
    }

    (skills, dropped)
}

fn load_open_skills(repo_dir: &Path, allow_scripts: bool) -> (Vec<Skill>, Vec<DroppedSkill>) {
    // Modern open-skills layout stores skill packages in `skills/<name>/SKILL.md`.
    // Prefer that structure to avoid treating repository docs (e.g. CONTRIBUTING.md)
    // as executable skills.
    let nested_skills_dir = repo_dir.join("skills");
    if nested_skills_dir.is_dir() {
        return load_open_skills_from_directory(&nested_skills_dir, allow_scripts);
    }

    let mut skills = Vec::new();
    let mut dropped = Vec::new();

    let Ok(entries) = std::fs::read_dir(repo_dir) else {
        return (skills, dropped);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let is_markdown = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
        if !is_markdown {
            continue;
        }

        let is_readme = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("README.md"));
        if is_readme {
            continue;
        }

        match audit::audit_open_skill_markdown(&path, repo_dir) {
            Ok(report) if report.is_clean() => {}
            Ok(report) => {
                let summary = report.summary();
                let scripts_blocked = report.scripts_blocked;
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "skipping insecure open-skill file {}: {}",
                        path.display().to_string(),
                        summary
                    )
                );
                dropped.push(DroppedSkill {
                    name: dir_stem(&path),
                    origin_hint: "open-skills".into(),
                    reason: SkillDropReason::AuditFindings {
                        summary,
                        scripts_blocked,
                    },
                    location: Some(path.clone()),
                });
                continue;
            }
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "skipping unauditable open-skill file {}: {err}",
                        path.display().to_string()
                    )
                );
                dropped.push(DroppedSkill {
                    name: dir_stem(&path),
                    origin_hint: "open-skills".into(),
                    reason: SkillDropReason::AuditError(err.to_string()),
                    location: Some(path.clone()),
                });
                continue;
            }
        }

        if let Ok(skill) = load_open_skill_md(&path) {
            skills.push(skill);
        }
    }

    (skills, dropped)
}

fn parse_open_skills_enabled(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn open_skills_enabled_from_sources(
    config_open_skills_enabled: Option<bool>,
    env_override: Option<&str>,
) -> bool {
    if let Some(raw) = env_override {
        if let Some(enabled) = parse_open_skills_enabled(raw) {
            return enabled;
        }
        if !raw.trim().is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Ignoring invalid ZEROCLAW_OPEN_SKILLS_ENABLED (valid: 1|0|true|false|yes|no|on|off)"
            );
        }
    }

    config_open_skills_enabled.unwrap_or(false)
}

fn open_skills_enabled(config_open_skills_enabled: Option<bool>) -> bool {
    let env_override = std::env::var("ZEROCLAW_OPEN_SKILLS_ENABLED").ok();
    open_skills_enabled_from_sources(config_open_skills_enabled, env_override.as_deref())
}

fn resolve_open_skills_dir_from_sources(
    env_dir: Option<&str>,
    config_dir: Option<&str>,
    home_dir: Option<&Path>,
) -> Option<PathBuf> {
    let parse_dir = |raw: &str| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    };

    if let Some(env_dir) = env_dir.and_then(parse_dir) {
        return Some(env_dir);
    }
    if let Some(config_dir) = config_dir.and_then(parse_dir) {
        return Some(config_dir);
    }
    home_dir.map(|home| home.join("open-skills"))
}

fn resolve_open_skills_dir(config_open_skills_dir: Option<&str>) -> Option<PathBuf> {
    let env_dir = std::env::var("ZEROCLAW_OPEN_SKILLS_DIR").ok();
    let home_dir = UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    resolve_open_skills_dir_from_sources(
        env_dir.as_deref(),
        config_open_skills_dir,
        home_dir.as_deref(),
    )
}

fn ensure_open_skills_repo(
    config_open_skills_enabled: Option<bool>,
    config_open_skills_dir: Option<&str>,
) -> Option<PathBuf> {
    if !open_skills_enabled(config_open_skills_enabled) {
        return None;
    }

    let repo_dir = resolve_open_skills_dir(config_open_skills_dir)?;

    if !repo_dir.exists() {
        if !clone_open_skills_repo(&repo_dir) {
            return None;
        }
        let _ = mark_open_skills_synced(&repo_dir);
        return Some(repo_dir);
    }

    if should_sync_open_skills(&repo_dir) {
        if pull_open_skills_repo(&repo_dir) {
            let _ = mark_open_skills_synced(&repo_dir);
        } else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "open-skills update failed; using local copy from {}",
                    repo_dir.display().to_string()
                )
            );
        }
    }

    Some(repo_dir)
}

fn clone_open_skills_repo(repo_dir: &Path) -> bool {
    if let Some(parent) = repo_dir.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "failed to create open-skills parent directory {}: {err}",
                parent.display().to_string()
            )
        );
        return false;
    }

    let output = Command::new("git")
        .args(["clone", "--depth", "1", OPEN_SKILLS_REPO_URL])
        .arg(repo_dir)
        .output();

    match output {
        Ok(result) if result.status.success() => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "initialized open-skills at {}",
                    repo_dir.display().to_string()
                )
            );
            true
        }
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"stderr": stderr})),
                "failed to clone open-skills: "
            );
            false
        }
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                "failed to run git clone for open-skills"
            );
            false
        }
    }
}

fn pull_open_skills_repo(repo_dir: &Path) -> bool {
    // If user points to a non-git directory via env var, keep using it without pulling.
    if !repo_dir.join(".git").exists() {
        return true;
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["pull", "--ff-only"])
        .output();

    match output {
        Ok(result) if result.status.success() => true,
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"stderr": stderr})),
                "failed to pull open-skills updates: "
            );
            false
        }
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                "failed to run git pull for open-skills"
            );
            false
        }
    }
}

fn should_sync_open_skills(repo_dir: &Path) -> bool {
    let marker = repo_dir.join(OPEN_SKILLS_SYNC_MARKER);
    let Ok(metadata) = std::fs::metadata(marker) else {
        return true;
    };
    let Ok(modified_at) = metadata.modified() else {
        return true;
    };
    let Ok(age) = SystemTime::now().duration_since(modified_at) else {
        return true;
    };

    age >= Duration::from_secs(OPEN_SKILLS_SYNC_INTERVAL_SECS)
}

fn mark_open_skills_synced(repo_dir: &Path) -> Result<()> {
    std::fs::write(repo_dir.join(OPEN_SKILLS_SYNC_MARKER), b"synced")?;
    Ok(())
}

/// Load a skill from a SKILL.toml manifest
fn load_skill_toml(path: &Path) -> Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    let manifest: SkillManifest = toml::from_str(&content)?;

    // Merge prompts from both locations: inside the [skill] table (natural
    // location for per-skill prompts) and at the manifest root (historical
    // location). Previously, prompts placed inside [skill] were silently
    // dropped because SkillMeta had no `prompts` field.
    let mut prompts = manifest.skill.prompts;
    prompts.extend(manifest.prompts);

    Ok(Skill {
        name: manifest.skill.name,
        description: manifest.skill.description,
        description_localizations: manifest.skill.description_localizations,
        version: manifest.skill.version,
        author: manifest.skill.author,
        tags: manifest.skill.tags,
        tools: manifest.tools,
        prompts,
        slash_options: manifest.skill.slash_options,
        location: Some(path.to_path_buf()),
    })
}

/// Load a skill from a SKILL.md file (simpler format)
fn load_skill_md(path: &Path, dir: &Path) -> Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    let parsed = parse_skill_markdown(&content);
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(Skill {
        name: parsed.meta.name.unwrap_or(name),
        description: parsed
            .meta
            .description
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| extract_description(&parsed.body)),
        // SKILL.md frontmatter carries no localizations.
        description_localizations: Default::default(),
        version: parsed.meta.version.unwrap_or_else(default_version),
        author: parsed.meta.author,
        tags: parsed.meta.tags,
        tools: Vec::new(),
        prompts: vec![parsed.body],
        slash_options: parsed.meta.slash_options,
        location: Some(path.to_path_buf()),
    })
}

fn load_open_skill_md(path: &Path) -> Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    let parsed = parse_skill_markdown(&content);
    let file_stem = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("open-skill")
        .to_string();
    let name = if file_stem.eq_ignore_ascii_case("skill") {
        path.parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or(&file_stem)
            .to_string()
    } else {
        file_stem
    };
    Ok(finalize_open_skill(Skill {
        name: parsed.meta.name.unwrap_or(name),
        description: parsed
            .meta
            .description
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| extract_description(&parsed.body)),
        // SKILL.md frontmatter carries no localizations.
        description_localizations: Default::default(),
        version: parsed
            .meta
            .version
            .unwrap_or_else(|| "open-skills".to_string()),
        author: parsed
            .meta
            .author
            .or_else(|| Some("besoeasy/open-skills".to_string())),
        tags: parsed.meta.tags,
        tools: Vec::new(),
        prompts: vec![parsed.body],
        slash_options: parsed.meta.slash_options,
        location: Some(path.to_path_buf()),
    }))
}

struct ParsedSkillMarkdown {
    meta: SkillMarkdownMeta,
    body: String,
}

fn parse_skill_markdown(content: &str) -> ParsedSkillMarkdown {
    if let Some((frontmatter, body)) = split_skill_frontmatter(content) {
        let meta = parse_simple_frontmatter(&frontmatter);
        return ParsedSkillMarkdown { meta, body };
    }

    ParsedSkillMarkdown {
        meta: SkillMarkdownMeta::default(),
        body: content.to_string(),
    }
}

/// Lightweight YAML-like frontmatter parser for simple `key: value` pairs.
/// Replaces `serde_yaml` to avoid pulling in the full YAML parser (~30KB)
/// for a struct with only 5 optional string fields.
fn parse_simple_frontmatter(s: &str) -> SkillMarkdownMeta {
    let mut meta = SkillMarkdownMeta::default();
    let mut collecting_tags = false;
    let mut collecting_multiline: Option<String> = None;
    let mut multiline_parts: Vec<String> = Vec::new();

    let flush_multiline = |key: &str, parts: &[String], meta: &mut SkillMarkdownMeta| {
        let joined = parts.join(" ");
        let val = joined.trim();
        if !val.is_empty() {
            match key {
                "description" => meta.description = Some(val.to_string()),
                "name" => meta.name = Some(val.to_string()),
                _ => {}
            }
        }
    };

    for line in s.lines() {
        // Collect indented continuation lines for YAML block scalars (>- or |)
        if let Some(ref key) = collecting_multiline {
            // A blank/whitespace-only line is a paragraph break *inside* the
            // block scalar, not a terminator — keep collecting. Only a
            // non-indented, non-empty line (a real next key) ends the scalar.
            if line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty() {
                multiline_parts.push(line.trim().to_string());
                continue;
            }
            flush_multiline(key, &multiline_parts, &mut meta);
            collecting_multiline = None;
            multiline_parts.clear();
        }

        // Handle YAML list items under `tags:` (e.g. "  - parser")
        if collecting_tags {
            let trimmed = line.trim();
            if let Some(item) = trimmed.strip_prefix("- ") {
                let tag = item.trim().trim_matches('"').trim_matches('\'');
                if !tag.is_empty() {
                    meta.tags.push(tag.to_string());
                }
                continue;
            }
            // Non-list-item line → stop collecting tags
            collecting_tags = false;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim().trim_matches('"').trim_matches('\'');
        // YAML block scalar indicators — collect continuation lines
        if val == ">-" || val == ">" || val == "|" || val == "|-" {
            collecting_multiline = Some(key.to_string());
            multiline_parts.clear();
            continue;
        }
        match key {
            "name" => meta.name = Some(val.to_string()),
            "description" => meta.description = Some(val.to_string()),
            "version" => meta.version = Some(val.to_string()),
            "author" => meta.author = Some(val.to_string()),
            "tags" => {
                if val.is_empty() {
                    // YAML block list follows on subsequent lines
                    collecting_tags = true;
                } else {
                    // Inline: [a, b, c] or comma-separated
                    let val = val.trim_start_matches('[').trim_end_matches(']');
                    meta.tags = val
                        .split(',')
                        .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                }
            }
            _ => {}
        }
    }
    if let Some(ref key) = collecting_multiline {
        flush_multiline(key, &multiline_parts, &mut meta);
    }
    // The one nested field. Parsed by the shared helper so the loader and the
    // service (`SkillDocument`) read `slash_options` identically — no second
    // nested parser to drift.
    meta.slash_options = document::parse_slash_options(s);
    meta
}

fn split_skill_frontmatter(content: &str) -> Option<(String, String)> {
    let normalized = content.replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n")?;
    if let Some(idx) = rest.find("\n---\n") {
        let frontmatter = rest[..idx].to_string();
        let body = rest[idx + 5..].to_string();
        return Some((frontmatter, body));
    }
    if let Some(frontmatter) = rest.strip_suffix("\n---") {
        return Some((frontmatter.to_string(), String::new()));
    }
    None
}

fn extract_description(content: &str) -> String {
    content
        .lines()
        .find(|line| !line.starts_with('#') && !line.trim().is_empty())
        .unwrap_or("No description")
        .trim()
        .to_string()
}

fn append_xml_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
}

fn write_xml_text_element(out: &mut String, indent: usize, tag: &str, value: &str) {
    for _ in 0..indent {
        out.push(' ');
    }
    out.push('<');
    out.push_str(tag);
    out.push('>');
    append_xml_escaped(out, value);
    out.push_str("</");
    out.push_str(tag);
    out.push_str(">\n");
}

fn resolve_skill_location(skill: &Skill, workspace_dir: &Path) -> PathBuf {
    skill.location.clone().unwrap_or_else(|| {
        workspace_dir
            .join("skills")
            .join(&skill.name)
            .join("SKILL.md")
    })
}

fn render_skill_location(skill: &Skill, workspace_dir: &Path, prefer_relative: bool) -> String {
    let location = resolve_skill_location(skill, workspace_dir);
    if prefer_relative && let Ok(relative) = location.strip_prefix(workspace_dir) {
        return display_skill_location(relative);
    }
    display_skill_location(&location)
}

fn display_skill_location(path: &Path) -> String {
    let rendered = path.display().to_string();
    #[cfg(target_os = "windows")]
    {
        rendered.replace('\\', "/")
    }
    #[cfg(not(target_os = "windows"))]
    {
        rendered
    }
}

/// Build the "Available Skills" system prompt section with full skill instructions.
pub fn skills_to_prompt(skills: &[Skill], workspace_dir: &Path) -> String {
    skills_to_prompt_with_mode(
        skills,
        workspace_dir,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
    )
}

fn is_registered_skill_tool_kind(kind: &str) -> bool {
    matches!(kind, "shell" | "script" | "http" | "builtin" | "mcp")
}

fn skill_tool_is_prompt_callable(tool: &SkillTool) -> bool {
    if !is_registered_skill_tool_kind(tool.kind.as_str()) {
        return false;
    }
    match tool.kind.as_str() {
        "builtin" | "mcp" => tool.target.as_deref().is_some_and(|t| !t.trim().is_empty()),
        _ => true,
    }
}

/// Build the "Available Skills" system prompt section with configurable verbosity.
pub fn skills_to_prompt_with_mode(
    skills: &[Skill],
    workspace_dir: &Path,
    mode: zeroclaw_config::schema::SkillsPromptInjectionMode,
) -> String {
    use std::fmt::Write;

    if skills.is_empty() {
        return String::new();
    }

    let mut prompt = match mode {
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full => String::from(
            "## Available Skills\n\n\
             Skill instructions and tool metadata are preloaded below.\n\
             Follow these instructions directly; do not read skill files at runtime unless the user asks.\n\n\
             <available_skills>\n",
        ),
        zeroclaw_config::schema::SkillsPromptInjectionMode::Compact => String::from(
            "## Available Skills\n\n\
             Skill summaries are preloaded below to keep context compact.\n\
             Skill instructions are loaded on demand: call `read_skill(name)` with the skill's `<name>` when you need the full skill file.\n\
             The `location` field is included for reference.\n\n\
             <available_skills>\n",
        ),
    };

    for skill in skills {
        let _ = writeln!(prompt, "  <skill>");
        write_xml_text_element(&mut prompt, 4, "name", &skill.name);
        write_xml_text_element(&mut prompt, 4, "description", &skill.description);
        let location = render_skill_location(
            skill,
            workspace_dir,
            matches!(
                mode,
                zeroclaw_config::schema::SkillsPromptInjectionMode::Compact
            ),
        );
        write_xml_text_element(&mut prompt, 4, "location", &location);

        // In Full mode, inline both instructions and tools.
        // In Compact mode, skip instructions (loaded on demand) but keep tools
        // so the LLM knows which skill tools are available.
        if matches!(
            mode,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full
        ) && !skill.prompts.is_empty()
        {
            let _ = writeln!(prompt, "    <instructions>");
            for instruction in &skill.prompts {
                write_xml_text_element(&mut prompt, 6, "instruction", instruction);
            }
            let _ = writeln!(prompt, "    </instructions>");
        }

        if !skill.tools.is_empty() {
            let registered: Vec<_> = skill
                .tools
                .iter()
                .filter(|t| skill_tool_is_prompt_callable(t))
                .collect();
            let unregistered: Vec<_> = skill
                .tools
                .iter()
                .filter(|t| !skill_tool_is_prompt_callable(t))
                .collect();

            if !registered.is_empty() {
                let _ = writeln!(
                    prompt,
                    "    <callable_tools hint=\"These are registered as callable tool specs. Invoke them directly by name ({{}}__{{}}) instead of using shell.\">"
                );
                for tool in &registered {
                    let _ = writeln!(prompt, "      <tool>");
                    write_xml_text_element(
                        &mut prompt,
                        8,
                        "name",
                        // Must match the registered tool spec's name exactly
                        // (same sanitizer), or the model is told to call a name
                        // that no tool exposes
                        &crate::tools::skill_tool::composed_tool_name(&skill.name, &tool.name),
                    );
                    write_xml_text_element(&mut prompt, 8, "description", &tool.description);
                    let _ = writeln!(prompt, "      </tool>");
                }
                let _ = writeln!(prompt, "    </callable_tools>");
            }

            if !unregistered.is_empty() {
                let _ = writeln!(prompt, "    <tools>");
                for tool in &unregistered {
                    let _ = writeln!(prompt, "      <tool>");
                    write_xml_text_element(&mut prompt, 8, "name", &tool.name);
                    write_xml_text_element(&mut prompt, 8, "description", &tool.description);
                    write_xml_text_element(&mut prompt, 8, "kind", &tool.kind);
                    let _ = writeln!(prompt, "      </tool>");
                }
                let _ = writeln!(prompt, "    </tools>");
            }
        }

        let _ = writeln!(prompt, "  </skill>");
    }

    prompt.push_str("</available_skills>");
    prompt
}

pub fn skills_to_tools(
    skills: &[Skill],
    security: std::sync::Arc<crate::security::SecurityPolicy>,
) -> Vec<Box<dyn zeroclaw_api::tool::Tool>> {
    skills_to_tools_with_context(skills, security, &[])
}

fn resolve_elevated_tool(
    skill_name: &str,
    tool: &SkillTool,
    kind_label: &str,
    resolution_registry: &[std::sync::Arc<dyn zeroclaw_api::tool::Tool>],
) -> Option<Box<dyn zeroclaw_api::tool::Tool>> {
    let Some(target_name) = tool.target.as_deref() else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "Skill tool {}.{} has kind='{}' but no 'target' field, skipping",
                skill_name, tool.name, kind_label
            )
        );
        return None;
    };
    match resolution_registry.iter().find(|t| t.name() == target_name) {
        Some(target) => Some(Box::new(crate::skills::skill_tool::SkillBuiltinTool::new(
            skill_name,
            tool,
            std::sync::Arc::clone(target),
            tool.locked_args.clone(),
        ))),
        None => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "Skill tool {}.{} targets {} '{}' which was not found in the \
                     resolution registry (for MCP, use the prefixed name \
                     '{{server}}__{{tool}}' and ensure the server is connected), skipping",
                    skill_name, tool.name, kind_label, target_name
                )
            );
            None
        }
    }
}

pub fn skills_to_tools_with_context(
    skills: &[Skill],
    security: std::sync::Arc<crate::security::SecurityPolicy>,
    unfiltered_registry: &[std::sync::Arc<dyn zeroclaw_api::tool::Tool>],
) -> Vec<Box<dyn zeroclaw_api::tool::Tool>> {
    skills_to_tools_with_context_and_runtime(
        skills,
        security,
        unfiltered_registry,
        std::sync::Arc::new(crate::platform::NativeRuntime::new()),
    )
}

pub fn skills_to_tools_with_context_and_runtime(
    skills: &[Skill],
    security: std::sync::Arc<crate::security::SecurityPolicy>,
    unfiltered_registry: &[std::sync::Arc<dyn zeroclaw_api::tool::Tool>],
    runtime: std::sync::Arc<dyn crate::platform::RuntimeAdapter>,
) -> Vec<Box<dyn zeroclaw_api::tool::Tool>> {
    let mut tools: Vec<Box<dyn zeroclaw_api::tool::Tool>> = Vec::new();
    for skill in skills {
        for tool in &skill.tools {
            if !is_registered_skill_tool_kind(tool.kind.as_str()) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "Unknown skill tool kind '{}' for {}.{}, skipping",
                        tool.kind, skill.name, tool.name
                    )
                );
                continue;
            }
            match tool.kind.as_str() {
                "shell" | "script" => {
                    let inner = crate::skills::skill_tool::SkillShellTool::new_with_runtime(
                        &skill.name,
                        tool,
                        security.clone(),
                        runtime.clone(),
                    );
                    tools.push(Box::new(zeroclaw_tools::wrappers::RateLimitedTool::new(
                        inner,
                        security.clone(),
                    )));
                }
                "http" => {
                    tools.push(Box::new(crate::skills::skill_http::SkillHttpTool::new(
                        &skill.name,
                        tool,
                    )));
                }
                "builtin" => {
                    if let Some(t) =
                        resolve_elevated_tool(&skill.name, tool, "builtin", unfiltered_registry)
                    {
                        tools.push(t);
                    }
                }
                "mcp" => {
                    if let Some(t) =
                        resolve_elevated_tool(&skill.name, tool, "MCP", unfiltered_registry)
                    {
                        tools.push(t);
                    }
                }
                // `is_registered_skill_tool_kind` above admits only the kinds
                // dispatched here, so any other kind was already skipped.
                other => unreachable!("registered skill kind '{other}' not dispatched"),
            }
        }
    }
    tools
}

/// Get the skills directory path
pub fn skills_dir(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("skills")
}

/// Initialize the skills directory with a README
pub fn init_skills_dir(workspace_dir: &Path) -> Result<()> {
    let dir = skills_dir(workspace_dir);
    std::fs::create_dir_all(&dir)?;

    let readme = dir.join("README.md");
    if !readme.exists() {
        std::fs::write(
            &readme,
            "# ZeroClaw Skills\n\n\
             Each subdirectory is a skill. Create a `SKILL.toml` or `SKILL.md` file inside.\n\n\
             ## SKILL.toml format\n\n\
             ```toml\n\
             [skill]\n\
             name = \"my-skill\"\n\
             description = \"What this skill does\"\n\
             version = \"0.1.0\"\n\
             author = \"your-name\"\n\
             tags = [\"productivity\", \"automation\"]\n\n\
             [[tools]]\n\
             name = \"my_tool\"\n\
             description = \"What this tool does\"\n\
             kind = \"shell\"\n\
             command = \"echo hello\"\n\
             ```\n\n\
             ## SKILL.md format (simpler)\n\n\
             Just write a markdown file with instructions for the agent.\n\
             Optional YAML frontmatter is supported for `name`, `description`, `version`, `author`, and `tags`.\n\
             The agent will read it and follow the instructions.\n\n\
             ## Installing community skills\n\n\
             ```bash\n\
             zeroclaw skills install <source>\n\
             zeroclaw skills list\n\
             ```\n",
        )?;
    }

    Ok(())
}

pub fn is_git_source(source: &str) -> bool {
    is_git_scheme_source(source, "https://")
        || is_git_scheme_source(source, "http://")
        || is_git_scheme_source(source, "ssh://")
        || is_git_scheme_source(source, "git://")
        || is_git_scp_source(source)
}

fn is_git_scheme_source(source: &str, scheme: &str) -> bool {
    let Some(rest) = source.strip_prefix(scheme) else {
        return false;
    };
    if rest.is_empty() || rest.starts_with('/') {
        return false;
    }

    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !host.is_empty()
}

fn is_git_scp_source(source: &str) -> bool {
    // SCP-like syntax accepted by git, e.g. git@host:owner/repo.git
    // Keep this strict enough to avoid treating local paths as git remotes.
    let Some((user_host, remote_path)) = source.split_once(':') else {
        return false;
    };
    if remote_path.is_empty() {
        return false;
    }
    if source.contains("://") {
        return false;
    }

    let Some((user, host)) = user_host.split_once('@') else {
        return false;
    };
    !user.is_empty()
        && !host.is_empty()
        && !user.contains('/')
        && !user.contains('\\')
        && !host.contains('/')
        && !host.contains('\\')
}

fn snapshot_skill_children(skills_path: &Path) -> Result<HashSet<PathBuf>> {
    let mut paths = HashSet::new();
    for entry in std::fs::read_dir(skills_path)? {
        let entry = entry?;
        paths.insert(entry.path());
    }
    Ok(paths)
}

fn detect_newly_installed_directory(
    skills_path: &Path,
    before: &HashSet<PathBuf>,
) -> Result<PathBuf> {
    let mut created = Vec::new();
    for entry in std::fs::read_dir(skills_path)? {
        let entry = entry?;
        let path = entry.path();
        if !before.contains(&path) && path.is_dir() {
            created.push(path);
        }
    }

    match created.len() {
        1 => Ok(created.remove(0)),
        0 => anyhow::bail!(
            "Unable to determine installed skill directory after clone (no new directory found)"
        ),
        _ => anyhow::bail!(
            "Unable to determine installed skill directory after clone (multiple new directories found)"
        ),
    }
}

fn enforce_skill_security_audit(
    skill_path: &Path,
    allow_scripts: bool,
) -> Result<audit::SkillAuditReport> {
    let report = audit::audit_skill_directory_with_options(
        skill_path,
        audit::SkillAuditOptions { allow_scripts },
    )?;
    if report.is_clean() {
        return Ok(report);
    }

    anyhow::bail!("Skill security audit failed: {}", report.summary());
}

fn remove_git_metadata(skill_path: &Path) -> Result<()> {
    let git_dir = skill_path.join(".git");
    if git_dir.exists() {
        std::fs::remove_dir_all(&git_dir)
            .with_context(|| format!("failed to remove {}", git_dir.display().to_string()))?;
    }
    Ok(())
}

fn copy_dir_recursive_secure(src: &Path, dest: &Path) -> Result<()> {
    let src_meta = std::fs::symlink_metadata(src)
        .with_context(|| format!("failed to read metadata for {}", src.display().to_string()))?;
    if src_meta.file_type().is_symlink() {
        anyhow::bail!(
            "Refusing to copy symlinked skill source path: {}",
            src.display()
        );
    }
    if !src_meta.is_dir() {
        anyhow::bail!(
            "Skill source must be a directory: {}",
            src.display().to_string()
        );
    }

    std::fs::create_dir_all(dest).with_context(|| {
        format!(
            "failed to create destination {}",
            dest.display().to_string()
        )
    })?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&src_path).with_context(|| {
            format!(
                "failed to read metadata for {}",
                src_path.display().to_string()
            )
        })?;

        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "Refusing to copy symlink within skill source: {}",
                src_path.display()
            );
        }

        if metadata.is_dir() {
            copy_dir_recursive_secure(&src_path, &dest_path)?;
        } else if metadata.is_file() {
            std::fs::copy(&src_path, &dest_path).with_context(|| {
                format!(
                    "failed to copy skill file from {} to {}",
                    src_path.display().to_string(),
                    dest_path.display()
                )
            })?;
        }
    }

    Ok(())
}

pub fn install_local_skill_source(
    source: &str,
    skills_path: &Path,
    allow_scripts: bool,
) -> Result<(PathBuf, usize)> {
    let source_path = PathBuf::from(source);
    if !source_path.exists() {
        anyhow::bail!("Source path does not exist: {source}");
    }

    let source_path = source_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize source path {source}"))?;
    let _ = enforce_skill_security_audit(&source_path, allow_scripts)?;

    let name = source_path
        .file_name()
        .context("Source path must include a directory name")?;
    let dest = skills_path.join(name);
    if dest.exists() {
        anyhow::bail!(
            "Destination skill already exists: {}",
            dest.display().to_string()
        );
    }

    if let Err(err) = copy_dir_recursive_secure(&source_path, &dest) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(err);
    }

    match enforce_skill_security_audit(&dest, allow_scripts) {
        Ok(report) => Ok((dest, report.files_scanned)),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&dest);
            Err(err)
        }
    }
}

pub fn install_git_skill_source(
    source: &str,
    skills_path: &Path,
    allow_scripts: bool,
) -> Result<(PathBuf, usize)> {
    let before = snapshot_skill_children(skills_path)?;
    let output = std::process::Command::new("git")
        .args(["clone", "--depth", "1", source])
        .current_dir(skills_path)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git clone failed: {stderr}");
    }

    let installed_dir = detect_newly_installed_directory(skills_path, &before)?;
    remove_git_metadata(&installed_dir)?;
    match enforce_skill_security_audit(&installed_dir, allow_scripts) {
        Ok(report) => Ok((installed_dir, report.files_scanned)),
        Err(err) => {
            let _ = std::fs::remove_dir_all(&installed_dir);
            Err(err)
        }
    }
}

// ─── Skills registry resolution ───────────────────────────────────────────────

pub fn is_registry_source(source: &str) -> bool {
    if source.is_empty() {
        return false;
    }
    if source.contains('/') || source.contains('\\') || source.contains("..") {
        return false;
    }
    if source.contains("://") || source.contains(':') {
        return false;
    }
    if source.starts_with('.') || source.starts_with('~') {
        return false;
    }
    source
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// True when `source` is an extra-registry spec `registry:<name>/<skill>`
/// with both segments being bare registry-safe identifiers.
pub fn is_extra_registry_source(source: &str) -> bool {
    parse_extra_registry_source(source).is_some()
}

/// Parse `registry:<name>/<skill>` into `(registry_name, skill_name)`.
/// Returns `None` unless it is exactly one registry name and one skill name,
/// both matching their install-spec identifiers.
pub fn parse_extra_registry_source(source: &str) -> Option<(String, String)> {
    let rest = source.strip_prefix("registry:")?;
    let (name, skill) = rest.split_once('/')?;
    if !zeroclaw_config::schema::ExternalRegistry::is_valid_name(name) || !is_registry_source(skill)
    {
        return None;
    }
    Some((name.to_string(), skill.to_string()))
}

fn clone_skills_repository(target_dir: &Path, repo_url: &str) -> Result<()> {
    if let Some(parent) = target_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create registry parent: {}",
                parent.display().to_string()
            )
        })?;
    }

    let output = Command::new("git")
        .args(["clone", "--depth", "1", repo_url])
        .arg(target_dir)
        .output()
        .context("failed to run git clone for skills registry")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("failed to clone skills registry: {stderr}");
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        &format!(
            "cloned skills registry to {}",
            target_dir.display().to_string()
        )
    );
    Ok(())
}

fn clone_skills_registry(registry_dir: &Path, repo_url: &str) -> Result<()> {
    clone_skills_repository(registry_dir, repo_url)?;
    mark_skills_registry_synced(registry_dir)?;
    Ok(())
}

fn pull_skills_registry(registry_dir: &Path) -> bool {
    if !registry_dir.join(".git").exists() {
        return true;
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(registry_dir)
        .args(["pull", "--ff-only"])
        .output();

    match output {
        Ok(result) if result.status.success() => true,
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"stderr": stderr})),
                "failed to pull skills registry updates: "
            );
            false
        }
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                "failed to run git pull for skills registry"
            );
            false
        }
    }
}

fn should_sync_skills_registry(registry_dir: &Path) -> bool {
    let marker = registry_dir.join(SKILLS_REGISTRY_SYNC_MARKER);
    let Ok(metadata) = std::fs::metadata(marker) else {
        return true;
    };
    let Ok(modified_at) = metadata.modified() else {
        return true;
    };
    let Ok(age) = SystemTime::now().duration_since(modified_at) else {
        return true;
    };
    age >= Duration::from_secs(SKILLS_REGISTRY_SYNC_INTERVAL_SECS)
}

fn mark_skills_registry_synced(registry_dir: &Path) -> Result<()> {
    std::fs::write(registry_dir.join(SKILLS_REGISTRY_SYNC_MARKER), b"synced")?;
    Ok(())
}

fn ensure_skills_registry(workspace_dir: &Path, registry_url: Option<&str>) -> Result<PathBuf> {
    let registry_dir = workspace_dir.join(SKILLS_REGISTRY_DIR_NAME);
    let repo_url = registry_url.unwrap_or(SKILLS_REGISTRY_REPO_URL);

    if !registry_dir.exists() {
        clone_skills_registry(&registry_dir, repo_url)?;
        return Ok(registry_dir);
    }

    if should_sync_skills_registry(&registry_dir) {
        if pull_skills_registry(&registry_dir) {
            let _ = mark_skills_registry_synced(&registry_dir);
        } else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "skills registry update failed; using local copy from {}",
                    registry_dir.display().to_string()
                )
            );
        }
    }

    Ok(registry_dir)
}

fn list_registry_skill_names(registry_dir: &Path) -> Vec<String> {
    let skills_parent = registry_dir.join("skills");
    let Ok(entries) = std::fs::read_dir(&skills_parent) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// List real directory entries under an already-contained catalog `skills/`
/// root without following entry symlinks.
fn list_contained_catalog_skill_names(skills_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(skills_root) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Install a single skill by name from a git catalog repository.
///
/// Clones `url` into a throwaway directory, resolves `skills/<skill_name>/`
/// (the same `<repo>/skills/<name>/` layout as the default and extra
/// registries), and installs it through the shared local-copy path (which
/// runs the security audit). No archive handling — pure `git clone`.
pub fn install_git_catalog_skill_source(
    url: &str,
    skill_name: &str,
    skills_path: &Path,
    allow_scripts: bool,
    workspace_dir: &Path,
) -> Result<(PathBuf, usize)> {
    if !is_registry_source(skill_name) {
        anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
            "cli-skills-install-invalid-skill-name",
            &[("skill", skill_name)]
        ));
    }

    std::fs::create_dir_all(workspace_dir).with_context(|| {
        crate::i18n::get_required_cli_string_with_args(
            "cli-skills-install-catalog-clone-failed",
            &[("url", url)],
        )
    })?;
    let clone_tempdir = tempfile::Builder::new()
        .prefix(".skill-catalog-")
        .tempdir_in(workspace_dir)
        .with_context(|| {
            crate::i18n::get_required_cli_string_with_args(
                "cli-skills-install-catalog-clone-failed",
                &[("url", url)],
            )
        })?;
    let clone_dir = clone_tempdir.path();

    // A transient catalog has no sync lifecycle, so clone it without writing
    // the persistent registry marker into the untrusted checkout. Besides
    // avoiding unnecessary state, this prevents a catalog-committed marker
    // symlink from redirecting that write to an arbitrary host file.
    clone_skills_repository(clone_dir, url).with_context(|| {
        crate::i18n::get_required_cli_string_with_args(
            "cli-skills-install-catalog-clone-failed",
            &[("url", url)],
        )
    })?;

    (|| {
        // Establish the catalog trust boundary before looking up a selected
        // name or enumerating available names. A catalog controls `skills/`,
        // so following it before this check could inspect an arbitrary host
        // directory even when the requested skill does not exist.
        let clone_root = clone_dir.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize catalog clone {}",
                clone_dir.display()
            )
        })?;
        let skills_dir = clone_dir.join("skills");
        let skills_meta = match std::fs::symlink_metadata(&skills_dir) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                    "cli-skills-install-skill-not-in-catalog-empty",
                    &[("skill", skill_name), ("url", url)]
                ));
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to read metadata for catalog skills root {}",
                        skills_dir.display()
                    )
                });
            }
        };
        if skills_meta.file_type().is_symlink() {
            anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                "cli-skills-install-catalog-root-symlink",
                &[("url", url)]
            ));
        }
        let skills_root = skills_dir.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize catalog skills root {}",
                skills_dir.display()
            )
        })?;
        if !skills_root.starts_with(&clone_root) {
            anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                "cli-skills-install-catalog-root-escapes",
                &[("url", url)]
            ));
        }
        if !skills_root.is_dir() {
            anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                "cli-skills-install-skill-not-in-catalog-empty",
                &[("skill", skill_name), ("url", url)]
            ));
        }

        let skill_dir = skills_root.join(skill_name);
        let entry_meta = match std::fs::symlink_metadata(&skill_dir) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let available = list_contained_catalog_skill_names(&skills_root);
                if available.is_empty() {
                    anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                        "cli-skills-install-skill-not-in-catalog-empty",
                        &[("skill", skill_name), ("url", url)]
                    ));
                }
                anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                    "cli-skills-install-skill-not-in-catalog",
                    &[
                        ("skill", skill_name),
                        ("url", url),
                        ("available", &available.join(", ")),
                    ]
                ));
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to read metadata for selected catalog skill {}",
                        skill_dir.display()
                    )
                });
            }
        };
        if entry_meta.file_type().is_symlink() {
            anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                "cli-skills-install-catalog-skill-symlink",
                &[("skill", skill_name), ("url", url)]
            ));
        }
        let selected = skill_dir.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize selected skill {}",
                skill_dir.display()
            )
        })?;
        if !selected.starts_with(&skills_root) {
            anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                "cli-skills-install-catalog-skill-escapes",
                &[("skill", skill_name), ("url", url)]
            ));
        }
        if !selected.is_dir() {
            let available = list_contained_catalog_skill_names(&skills_root);
            if available.is_empty() {
                anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                    "cli-skills-install-skill-not-in-catalog-empty",
                    &[("skill", skill_name), ("url", url)]
                ));
            }
            anyhow::bail!(crate::i18n::get_required_cli_string_with_args(
                "cli-skills-install-skill-not-in-catalog",
                &[
                    ("skill", skill_name),
                    ("url", url),
                    ("available", &available.join(", ")),
                ]
            ));
        }
        // i18n-exempt: internal invariant — the clone path is our own ASCII
        // `.skill-catalog-*` scratch dir, so this only fires on a broken
        // host filesystem; it is a developer diagnostic, not normal CLI output.
        let skill_dir_str = selected
            .to_str()
            .with_context(|| format!("skill path is not valid UTF-8: {}", selected.display()))?;
        install_local_skill_source(skill_dir_str, skills_path, allow_scripts)
    })()
}

pub fn install_registry_skill_source(
    source: &str,
    skills_path: &Path,
    allow_scripts: bool,
    workspace_dir: &Path,
    registry_url: Option<&str>,
    suppress_tier_banner: bool,
) -> Result<(PathBuf, usize)> {
    let registry_dir = ensure_skills_registry(workspace_dir, registry_url)?;
    let skill_dir = registry_dir.join("skills").join(source);

    if !skill_dir.is_dir() {
        let available = list_registry_skill_names(&registry_dir);
        if available.is_empty() {
            anyhow::bail!("skill '{source}' not found in the registry and no skills are available");
        }
        anyhow::bail!(
            "skill '{source}' not found in the registry.\nAvailable skills: {}",
            available.join(", ")
        );
    }

    if !suppress_tier_banner {
        let (tier, version) = lookup_registry_skill_tier(&registry_dir, source);
        print_install_tier_banner(source, version.as_deref(), tier);
    }

    install_local_skill_source(
        skill_dir.to_str().with_context(|| {
            format!(
                "registry path is not valid UTF-8: {}",
                skill_dir.display().to_string()
            )
        })?,
        skills_path,
        allow_scripts,
    )
}

/// Clone (or refresh) a user-configured extra registry into its own
/// `<workspace>/extra-registry-<name>/` directory, reusing the default
/// registry's clone/pull/sync helpers.
fn ensure_extra_registry(
    workspace_dir: &Path,
    registry_name: &str,
    repo_url: &str,
) -> Result<PathBuf> {
    let registry_dir = workspace_dir.join(format!("{EXTRA_REGISTRY_DIR_PREFIX}{registry_name}"));

    if !registry_dir.exists() {
        clone_skills_registry(&registry_dir, repo_url)?;
        return Ok(registry_dir);
    }

    if should_sync_skills_registry(&registry_dir) {
        if pull_skills_registry(&registry_dir) {
            let _ = mark_skills_registry_synced(&registry_dir);
        } else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "extra registry update failed; using local copy from {}",
                    registry_dir.display().to_string()
                )
            );
        }
    }

    Ok(registry_dir)
}

/// Install a skill from a user-configured extra registry, addressed as
/// `registry:<name>/<skill>`. The named registry must be present, enabled, and
/// of `kind = "git"`; it reuses the same git-clone registry mechanism as the
/// default bare-name registry and then installs the skill locally.
pub fn install_extra_registry_skill_source(
    source: &str,
    skills_path: &Path,
    allow_scripts: bool,
    workspace_dir: &Path,
    extra_registries: &[zeroclaw_config::schema::ExternalRegistry],
    suppress_tier_banner: bool,
) -> Result<(PathBuf, usize)> {
    let (registry_name, skill_name) = parse_extra_registry_source(source).with_context(|| {
        format!("invalid extra-registry spec '{source}': expected 'registry:<name>/<skill>'")
    })?;

    let registry = extra_registries
        .iter()
        .find(|r| r.name == registry_name && r.enabled)
        .with_context(|| {
            let configured: Vec<&str> = extra_registries
                .iter()
                .filter(|r| r.enabled)
                .map(|r| r.name.as_str())
                .collect();
            if configured.is_empty() {
                format!(
                    "registry '{registry_name}' is not configured or is disabled. \
                     Add it under [[skills.extra_registries]] in your config."
                )
            } else {
                format!(
                    "registry '{registry_name}' is not configured or is disabled. \
                     Configured registries: {}",
                    configured.join(", ")
                )
            }
        })?;

    if registry.kind != zeroclaw_config::schema::ExternalRegistryKind::Git {
        anyhow::bail!(
            "registry '{registry_name}' uses unsupported kind '{}'; only 'git' is supported",
            registry.kind
        );
    }

    let registry_dir = ensure_extra_registry(workspace_dir, &registry_name, &registry.url)?;
    let skill_dir = registry_dir.join("skills").join(&skill_name);

    if !skill_dir.is_dir() {
        let available = list_registry_skill_names(&registry_dir);
        if available.is_empty() {
            anyhow::bail!(
                "skill '{skill_name}' not found in registry '{registry_name}' and no skills are available"
            );
        }
        anyhow::bail!(
            "skill '{skill_name}' not found in registry '{registry_name}'.\nAvailable skills: {}",
            available.join(", ")
        );
    }

    if !suppress_tier_banner {
        let (tier, version) = lookup_registry_skill_tier(&registry_dir, &skill_name);
        print_install_tier_banner(&skill_name, version.as_deref(), tier);
    }

    install_local_skill_source(
        skill_dir.to_str().with_context(|| {
            format!(
                "registry path is not valid UTF-8: {}",
                skill_dir.display().to_string()
            )
        })?,
        skills_path,
        allow_scripts,
    )
}

// ─── Plugin-shipped skills (plugins-wasm only) ───────────────────────────────

#[cfg(feature = "plugins-wasm")]
pub fn load_plugin_skills_from_config(
    config: &zeroclaw_config::schema::Config,
) -> (Vec<Skill>, Vec<DroppedSkill>) {
    if !config.plugins.enabled {
        return (Vec::new(), Vec::new());
    }

    let plugins_dir = config.plugins.resolved_plugins_dir();

    let signature_mode = zeroclaw_plugins::host::PluginHost::resolve_signature_mode(
        &config.plugins.security.signature_mode,
    );
    let trusted_keys = config.plugins.security.trusted_publisher_keys.clone();

    let host = match zeroclaw_plugins::host::PluginHost::from_plugins_dir_with_security(
        &plugins_dir,
        signature_mode,
        trusted_keys,
    ) {
        Ok(host) => host,
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                "failed to discover plugin skills"
            );
            return (Vec::new(), Vec::new());
        }
    };

    let allow_scripts = config.skills.allow_scripts;
    let mut skills = Vec::new();
    let mut dropped = Vec::new();
    for (manifest, skills_dir) in host.skill_plugin_details() {
        let (raw_skills, raw_dropped) = load_skills_from_directory(&skills_dir, allow_scripts);
        for raw in raw_skills {
            skills.push(namespace_plugin_skill(&manifest.name, raw));
        }
        // Retag the workspace-loader's drops as plugin-origin.
        dropped.extend(raw_dropped.into_iter().map(|mut d| {
            d.origin_hint = "plugin".into();
            d
        }));
    }
    (skills, dropped)
}

#[cfg(feature = "plugins-wasm")]
fn namespace_plugin_skill(plugin_name: &str, mut skill: Skill) -> Skill {
    let qualified = format!("plugin:{}/{}", plugin_name, skill.name);
    skill.name = qualified;
    let plugin_tag = format!("plugin:{plugin_name}");
    if !skill.tags.iter().any(|t| t == &plugin_tag) {
        skill.tags.push(plugin_tag);
    }
    skill
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn slash_option_kinds_registry_is_walked_from_the_enum() {
        // The published registry is exactly `SlashOptionKind::ALL` walked into
        // descriptors, in order. No hand-authored rows: adding a variant to the
        // enum extends this without touching the builder.
        let registry = slash_option_kinds();
        assert_eq!(registry.len(), SlashOptionKind::ALL.len());
        for (descriptor, kind) in registry.iter().zip(SlashOptionKind::ALL) {
            assert_eq!(descriptor.manifest_name, kind.manifest_name());
            assert_eq!(descriptor.supports_choices, kind.supports_choices());
            assert_eq!(
                descriptor.supports_numeric_bounds,
                kind.supports_numeric_bounds()
            );
            assert_eq!(
                descriptor.supports_length_bounds,
                kind.supports_length_bounds()
            );
        }
    }

    #[test]
    fn only_scalar_kinds_carry_bounds_and_choices() {
        // Capability invariants the surfaces depend on: numeric bounds imply a
        // scalar with choices; length bounds are string-only.
        for kind in SlashOptionKind::ALL {
            if kind.supports_numeric_bounds() || kind.supports_length_bounds() {
                assert!(
                    kind.supports_choices(),
                    "{:?} carries bounds but is not choiceable",
                    kind.manifest_name()
                );
            }
        }
        assert!(SlashOptionKind::String.supports_length_bounds());
        assert!(!SlashOptionKind::String.supports_numeric_bounds());
        assert!(SlashOptionKind::Integer.supports_numeric_bounds());
        assert!(!SlashOptionKind::Integer.supports_length_bounds());
    }

    #[test]
    fn parse_simple_frontmatter_keeps_blank_line_in_block_scalar() {
        // A blank line is a paragraph break *inside* a YAML block scalar, not a
        // terminator. The parser must not truncate the description at it.
        let frontmatter = "name: x\ndescription: >-\n  para one\n\n  para two\n";
        let meta = parse_simple_frontmatter(frontmatter);
        let desc = meta.description.expect("description should be parsed");
        assert!(
            desc.contains("para one"),
            "first paragraph missing: {desc:?}"
        );
        assert!(
            desc.contains("para two"),
            "second paragraph after blank line was truncated: {desc:?}"
        );
        assert_eq!(meta.name.as_deref(), Some("x"));
    }

    #[test]
    fn parse_simple_frontmatter_block_scalar_stops_at_next_key() {
        // A real, non-indented next key must still terminate the block scalar.
        let frontmatter = "description: >-\n  hello\n  world\nversion: 1.2.3\n";
        let meta = parse_simple_frontmatter(frontmatter);
        assert_eq!(meta.description.as_deref(), Some("hello world"));
        assert_eq!(meta.version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn test_is_registry_source_accepts_bare_names() {
        assert!(is_registry_source("auto-coder"));
        assert!(is_registry_source("web-researcher"));
        assert!(is_registry_source("telegram-assistant"));
        assert!(is_registry_source("data_analyst"));
        assert!(is_registry_source("ci-helper"));
        assert!(is_registry_source("selfimproving"));
    }

    #[test]
    fn test_is_registry_source_rejects_empty() {
        assert!(!is_registry_source(""));
    }

    #[test]
    fn test_is_registry_source_rejects_paths() {
        assert!(!is_registry_source("./my-skill"));
        assert!(!is_registry_source("../my-skill"));
        assert!(!is_registry_source("/abs/path"));
        assert!(!is_registry_source("skills/auto-coder"));
        assert!(!is_registry_source("some\\path"));
        assert!(!is_registry_source("~/.zeroclaw/skills/foo"));
    }

    #[test]
    fn test_is_registry_source_rejects_urls() {
        assert!(!is_registry_source("https://github.com/foo/bar"));
        assert!(!is_registry_source("http://example.com"));
        assert!(!is_registry_source("ssh://git@host/repo"));
        assert!(!is_registry_source("git://host/repo"));
        assert!(!is_registry_source("git@github.com:user/repo"));
    }

    #[test]
    fn test_is_registry_source_rejects_prefixed() {
        assert!(!is_registry_source("external:my-skill"));
    }

    #[test]
    fn test_is_registry_source_rejects_traversal() {
        assert!(!is_registry_source(".."));
        assert!(!is_registry_source("foo..bar"));
    }

    #[test]
    fn test_is_registry_source_rejects_special_chars() {
        assert!(!is_registry_source(".hidden"));
        assert!(!is_registry_source("~tilde"));
    }

    #[test]
    fn test_is_extra_registry_source_accepts_valid() {
        assert!(is_extra_registry_source("registry:myreg/auto-coder"));
        assert!(is_extra_registry_source("registry:co_op/data_analyst"));
        assert!(is_extra_registry_source("registry:r1/ci-helper"));
    }

    #[test]
    fn test_is_extra_registry_source_rejects_malformed() {
        assert!(!is_extra_registry_source(""));
        assert!(!is_extra_registry_source("registry:"));
        assert!(!is_extra_registry_source("registry:onlyname"));
        assert!(!is_extra_registry_source("registry:a/b/c"));
        assert!(!is_extra_registry_source("registry:../x"));
        assert!(!is_extra_registry_source("registry:a /b"));
        assert!(!is_extra_registry_source("registry:a/b:c"));
        assert!(!is_extra_registry_source("registry:/skill"));
        assert!(!is_extra_registry_source("registry:name/"));
        // A bare name has no prefix and stays a Tier-1 registry install.
        assert!(!is_extra_registry_source("auto-coder"));
    }

    #[test]
    fn test_is_extra_registry_source_rejects_competing_schemes() {
        assert!(!is_extra_registry_source("external:x"));
        assert!(!is_extra_registry_source("https://github.com/o/r"));
        assert!(!is_extra_registry_source("git@github.com:o/r"));
        assert!(!is_extra_registry_source("./local"));
    }

    #[test]
    fn test_parse_extra_registry_source_splits() {
        assert_eq!(
            parse_extra_registry_source("registry:myreg/auto-coder"),
            Some(("myreg".to_string(), "auto-coder".to_string()))
        );
        assert_eq!(parse_extra_registry_source("registry:onlyname"), None);
        assert_eq!(parse_extra_registry_source("registry:a/b/c"), None);
        assert_eq!(parse_extra_registry_source("auto-coder"), None);
    }

    #[test]
    fn test_install_extra_registry_unknown_name_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_path = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_path).unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = install_extra_registry_skill_source(
            "registry:nope/demo",
            &skills_path,
            false,
            &workspace,
            &[],
            true,
        )
        .expect_err("unknown registry must error before any git work");
        assert!(err.to_string().contains("nope"), "got: {err}");
    }

    #[test]
    fn test_install_git_catalog_rejects_non_bare_skill_name() {
        // The bare-name guard must reject anything with a path separator before
        // any network/git work happens (hermetic — no clone is attempted).
        assert!(!is_registry_source("a/b"));

        let tmp = tempfile::tempdir().unwrap();
        let skills_path = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_path).unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = install_git_catalog_skill_source(
            "https://github.com/example/skills",
            "a/b",
            &skills_path,
            false,
            &workspace,
        )
        .expect_err("a slashed --skill name must be rejected before any git work");
        assert!(err.to_string().contains("bare skill name"), "got: {err}");
    }

    /// Build a local git repository that acts as a skill catalog: a real commit
    /// containing `skills/<name>/SKILL.md` for each requested skill. Returns the
    /// repo path, which doubles as the clone URL for
    /// `install_git_catalog_skill_source` (git clones local paths directly, so
    /// the test stays hermetic — no network).
    fn init_git_skill_catalog(root: &Path, skills: &[&str]) -> std::path::PathBuf {
        let repo = root.join("catalog");
        for name in skills {
            let skill_dir = repo.join("skills").join(name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!(
                    "---\nname: {name}\ndescription: hermetic git-catalog fixture\n---\n\n# {name}\n"
                ),
            )
            .unwrap();
        }
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git must be available to build the catalog fixture");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["add", "-A"]);
        // Pass identity/signing inline so the commit does not depend on the
        // runner's global git config.
        run(&[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        repo
    }

    #[test]
    fn install_git_catalog_skill_source_installs_selected_skill_through_audit() {
        // Happy path for the `--skill` replacement: clone a local git catalog,
        // resolve `skills/<name>/`, and install it through the shared
        // clone → local-copy → security-audit path. Skipped if git is absent.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let catalog = init_git_skill_catalog(tmp.path(), &["demo-skill", "other-skill"]);
        let skills_path = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_path).unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let (dest, files_scanned) = install_git_catalog_skill_source(
            catalog.to_str().unwrap(),
            "demo-skill",
            &skills_path,
            false,
            &workspace,
        )
        .expect("happy-path git-catalog install should succeed");

        // Installed at the expected destination, with the catalog's SKILL.md.
        assert_eq!(dest, skills_path.join("demo-skill"));
        assert!(
            dest.join("SKILL.md").is_file(),
            "the selected skill's SKILL.md must be installed"
        );
        // A non-zero scan count proves the security-audit path was entered.
        assert!(
            files_scanned >= 1,
            "install must run through the audit path; files_scanned = {files_scanned}"
        );
        // Only the requested skill is installed, not its sibling.
        assert!(!skills_path.join("other-skill").exists());
        // The transient clone scratch dir is cleaned up afterwards.
        let leftover = std::fs::read_dir(&workspace)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".skill-catalog-")
            });
        assert!(!leftover, "clone scratch dir must be removed after install");
    }

    #[test]
    fn install_git_catalog_skill_source_reports_missing_skill_after_clone() {
        // The main post-clone failure mode: the requested skill is not in the
        // catalog. The error must name it and list what *is* available, and must
        // not install anything.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let catalog = init_git_skill_catalog(tmp.path(), &["present-skill"]);
        let skills_path = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_path).unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = install_git_catalog_skill_source(
            catalog.to_str().unwrap(),
            "absent-skill",
            &skills_path,
            false,
            &workspace,
        )
        .expect_err("a skill missing from the catalog must error after clone");
        let msg = err.to_string();
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(
            msg.contains("present-skill"),
            "error should list the available skills; got: {msg}"
        );
        // Nothing installed, and the clone scratch dir is cleaned up.
        assert!(!skills_path.join("absent-skill").exists());
        let leftover = std::fs::read_dir(&workspace)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".skill-catalog-")
            });
        assert!(!leftover, "clone scratch dir must be removed after failure");
    }

    /// Commit whatever is currently in `repo`'s worktree with a hermetic
    /// identity, so tests can add symlink entries the fixture builder can't.
    #[cfg(unix)]
    fn git_commit_all(repo: &Path, message: &str) {
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(repo)
                .output()
                .expect("git must be available");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["add", "-A"]);
        run(&[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ]);
    }

    #[cfg(unix)]
    #[test]
    fn install_git_catalog_does_not_follow_registry_sync_marker_symlink() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let external_marker = tmp.path().join("external-marker");
        std::fs::write(&external_marker, "must remain unchanged").unwrap();

        let catalog = init_git_skill_catalog(tmp.path(), &["demo-skill"]);
        std::os::unix::fs::symlink(&external_marker, catalog.join(SKILLS_REGISTRY_SYNC_MARKER))
            .unwrap();
        git_commit_all(&catalog, "add hostile registry sync marker symlink");

        let skills_path = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_path).unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let (dest, _) = install_git_catalog_skill_source(
            catalog.to_str().unwrap(),
            "demo-skill",
            &skills_path,
            false,
            &workspace,
        )
        .expect("a catalog marker must not participate in transient clone state");

        assert!(dest.join("SKILL.md").is_file());
        assert_eq!(
            std::fs::read_to_string(&external_marker).unwrap(),
            "must remain unchanged",
            "a catalog-controlled marker symlink must not redirect a host write"
        );
        let leftover = std::fs::read_dir(&workspace)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".skill-catalog-")
            });
        assert!(!leftover, "clone scratch dir must be removed after install");
    }

    #[cfg(unix)]
    #[test]
    fn install_git_catalog_skill_source_rejects_symlinked_selected_skill() {
        // A catalog that commits `skills/<name>` as a symlink pointing outside
        // the repo must be refused: `is_dir()` follows the link and
        // `install_local_skill_source` would canonicalize it to the external
        // target and audit/copy it. The out-of-clone directory here is itself a
        // *clean* skill, proving the audit passing does not rescue containment.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        // A valid skill living outside the catalog — the escape target.
        let outside = tmp.path().join("outside").join("secret-skill");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("SKILL.md"),
            "---\nname: secret-skill\ndescription: outside the catalog\n---\n\n# secret\n",
        )
        .unwrap();

        let catalog = init_git_skill_catalog(tmp.path(), &["present-skill"]);
        // Commit an absolute symlink `skills/evil` -> the external skill dir.
        std::os::unix::fs::symlink(&outside, catalog.join("skills").join("evil")).unwrap();
        git_commit_all(&catalog, "add escaping symlink");

        let skills_path = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_path).unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = install_git_catalog_skill_source(
            catalog.to_str().unwrap(),
            "evil",
            &skills_path,
            false,
            &workspace,
        )
        .expect_err("a symlinked catalog entry must be rejected");
        assert!(
            err.to_string().contains("symlink"),
            "error should name the symlink; got: {err}"
        );
        // Nothing installed — neither the symlink name nor the escape target.
        assert!(!skills_path.join("evil").exists());
        assert!(!skills_path.join("secret-skill").exists());
        // The escape target on disk is untouched.
        assert!(outside.join("SKILL.md").is_file());
        let leftover = std::fs::read_dir(&workspace)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".skill-catalog-")
            });
        assert!(
            !leftover,
            "clone scratch dir must be removed after rejection"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_git_catalog_skill_source_rejects_selection_escaping_via_symlinked_skills_dir() {
        // Backstop for the case the symlink_metadata check alone misses: the
        // selected `skills/<name>` is a real directory, but its parent `skills`
        // is a symlink out of the clone. The final component is not a link, so
        // only the canonicalize-and-contain check catches the escape.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        // External directory that `skills` will point at, holding a clean skill.
        let external = tmp.path().join("external-skills");
        let victim = external.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(
            victim.join("SKILL.md"),
            "---\nname: victim\ndescription: outside the catalog\n---\n\n# victim\n",
        )
        .unwrap();

        // A repo whose entire `skills/` tree is a symlink to `external`.
        let catalog = tmp.path().join("catalog");
        std::fs::create_dir_all(&catalog).unwrap();
        std::os::unix::fs::symlink(&external, catalog.join("skills")).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&catalog)
            .output()
            .expect("git init");
        git_commit_all(&catalog, "symlink skills dir out of the repo");

        let skills_path = tmp.path().join("dest-skills");
        std::fs::create_dir_all(&skills_path).unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = install_git_catalog_skill_source(
            catalog.to_str().unwrap(),
            "victim",
            &skills_path,
            false,
            &workspace,
        )
        .expect_err("a selection resolving outside the clone must be rejected");
        assert!(
            err.to_string().contains("outside") || err.to_string().contains("symlink"),
            "error should describe the containment/symlink failure; got: {err}"
        );
        assert!(!skills_path.join("victim").exists());
        assert!(victim.join("SKILL.md").is_file());
        let leftover = std::fs::read_dir(&workspace)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".skill-catalog-")
            });
        assert!(
            !leftover,
            "clone scratch dir must be removed after rejection"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_git_catalog_missing_skill_rejects_symlinked_skills_root_before_enumeration() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let external = tmp.path().join("external-skills");
        let external_entry = external.join("external-private-name");
        std::fs::create_dir_all(&external_entry).unwrap();
        let external_manifest = external_entry.join("SKILL.md");
        let external_contents =
            "---\nname: external-private-name\ndescription: outside the catalog\n---\n";
        std::fs::write(&external_manifest, external_contents).unwrap();

        let catalog = tmp.path().join("catalog");
        std::fs::create_dir_all(&catalog).unwrap();
        std::os::unix::fs::symlink(&external, catalog.join("skills")).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&catalog)
            .output()
            .expect("git init");
        git_commit_all(&catalog, "symlink skills root out of the repo");

        let skills_path = tmp.path().join("dest-skills");
        std::fs::create_dir_all(&skills_path).unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = install_git_catalog_skill_source(
            catalog.to_str().unwrap(),
            "missing-skill",
            &skills_path,
            false,
            &workspace,
        )
        .expect_err("a symlinked catalog skills root must be rejected before enumeration");
        let message = err.to_string();
        assert!(message.contains("symlink"), "got: {message}");
        assert!(
            !message.contains("external-private-name"),
            "external entry names must not be enumerated; got: {message}"
        );
        assert_eq!(
            std::fs::read_dir(&skills_path).unwrap().count(),
            0,
            "nothing may be installed after rejecting the catalog root"
        );
        assert_eq!(
            std::fs::read_to_string(&external_manifest).unwrap(),
            external_contents,
            "the external target must remain untouched"
        );
        let leftover = std::fs::read_dir(&workspace)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".skill-catalog-")
            });
        assert!(
            !leftover,
            "clone scratch dir must be removed after rejection"
        );
    }

    #[test]
    fn tier_from_tags_recognizes_official() {
        assert_eq!(
            tier_from_tags(&["Official".into(), "Featured".into()]),
            SkillTier::Official
        );
        // Case-insensitive match.
        assert_eq!(tier_from_tags(&["official".into()]), SkillTier::Official);
    }

    #[test]
    fn tier_from_tags_recognizes_community() {
        assert_eq!(tier_from_tags(&["Community".into()]), SkillTier::Community);
    }

    #[test]
    fn tier_from_tags_recognizes_featured_only() {
        assert_eq!(tier_from_tags(&["Featured".into()]), SkillTier::Featured);
    }

    #[test]
    fn tier_from_tags_falls_back_to_unknown_when_no_tier_tag() {
        assert_eq!(tier_from_tags(&[]), SkillTier::Unknown);
        assert_eq!(
            tier_from_tags(&["productivity".into(), "automation".into()]),
            SkillTier::Unknown
        );
    }

    /// Resolve a tier banner against the English catalogue only — locale- and
    /// filesystem-independent, mirroring build_install_tier_banner's assembly.
    fn english_tier_banner(name: &str, version: Option<&str>, tier: SkillTier) -> String {
        let version_label = version.unwrap_or("?");
        let args = [("name", name), ("version", version_label)];
        let mut banner =
            crate::i18n::get_english_cli_string_with_args(install_tier_banner_key(tier), &args);
        if !banner.ends_with('\n') {
            banner.push('\n');
        }
        banner
    }

    #[test]
    fn build_install_tier_banner_official_is_single_line() {
        let banner = english_tier_banner("auto-coder", Some("0.3.0"), SkillTier::Official);
        assert!(banner.contains("Official (zeroclaw-labs maintained)"));
        assert!(banner.contains("Installing auto-coder v0.3.0"));
        assert!(!banner.contains("not audited"));
        // One trailing newline, no warn block.
        assert_eq!(banner.lines().count(), 1);
    }

    #[test]
    fn build_install_tier_banner_community_warns() {
        let banner = english_tier_banner("discord-moderator", Some("0.1.2"), SkillTier::Community);
        assert!(banner.contains("Community submission"));
        assert!(banner.contains("not audited by ZeroClaw"));
        assert!(banner.contains("zeroclaw skills audit discord-moderator"));
    }

    #[test]
    fn build_install_tier_banner_featured_uses_community_warning() {
        let banner = english_tier_banner("hand-picked", Some("1.0"), SkillTier::Featured);
        assert!(banner.contains("Community submission"));
        assert!(banner.contains("not audited by ZeroClaw"));
    }

    #[test]
    fn build_install_tier_banner_unknown_falls_back_to_community() {
        let banner = english_tier_banner("legacy", None, SkillTier::Unknown);
        assert!(banner.contains("Community submission"));
        assert!(banner.contains("not audited by ZeroClaw"));
        // Missing version is rendered as `v?` rather than panicking.
        assert!(banner.contains("v?"));
    }

    #[test]
    fn lookup_registry_skill_tier_resolves_from_registry_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let json = r#"{
            "version": 1,
            "skills": [
                { "name": "auto-coder", "version": "0.3.0", "tags": ["Official", "Featured"] },
                { "name": "discord-moderator", "version": "0.1.2", "tags": ["Community"] },
                { "name": "hand-picked", "version": "1.0.0", "tags": ["Featured"] },
                { "name": "untagged", "version": "0.0.1", "tags": ["productivity"] }
            ]
        }"#;
        std::fs::write(tmp.path().join("registry.json"), json).unwrap();

        assert_eq!(
            lookup_registry_skill_tier(tmp.path(), "auto-coder"),
            (SkillTier::Official, Some("0.3.0".to_string()))
        );
        assert_eq!(
            lookup_registry_skill_tier(tmp.path(), "discord-moderator"),
            (SkillTier::Community, Some("0.1.2".to_string()))
        );
        assert_eq!(
            lookup_registry_skill_tier(tmp.path(), "hand-picked"),
            (SkillTier::Featured, Some("1.0.0".to_string()))
        );
        // Skill present but no tier tag → Unknown (treated as Community by the banner).
        assert_eq!(
            lookup_registry_skill_tier(tmp.path(), "untagged"),
            (SkillTier::Unknown, Some("0.0.1".to_string()))
        );
        // Skill not in registry.json at all → Unknown with no version.
        assert_eq!(
            lookup_registry_skill_tier(tmp.path(), "missing"),
            (SkillTier::Unknown, None)
        );
    }

    #[test]
    fn lookup_registry_skill_tier_handles_missing_index() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(
            lookup_registry_skill_tier(tmp.path(), "anything"),
            (SkillTier::Unknown, None)
        );
    }

    #[test]
    fn lookup_registry_skill_tier_handles_malformed_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("registry.json"), "{ not json").unwrap();
        assert_eq!(
            lookup_registry_skill_tier(tmp.path(), "anything"),
            (SkillTier::Unknown, None)
        );
    }
}

#[cfg(test)]
mod prompts_section_tests {
    use super::*;
    use tempfile::TempDir;

    fn write_manifest(dir: &Path, toml: &str) -> std::path::PathBuf {
        let p = dir.join("SKILL.toml");
        std::fs::write(&p, toml).unwrap();
        p
    }

    #[test]
    fn prompts_inside_skill_section_are_loaded() {
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"
[skill]
name = "probe"
description = "test"
version = "0.1.0"
prompts = ["If asked about XYZZY, respond YES"]
"#,
        );
        let skill = load_skill_toml(&path).unwrap();
        assert_eq!(
            skill.prompts,
            vec!["If asked about XYZZY, respond YES".to_string()]
        );
    }

    #[test]
    fn typed_slash_options_are_parsed_from_the_skill_table() {
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"
[skill]
name = "search"
description = "Search the web"
version = "0.1.0"
tags = ["slash"]

[[skill.slash_options]]
name = "query"
description = "The search query"
type = "string"
required = true
max_length = 200

[[skill.slash_options]]
name = "sort"
description = "Sort order"
type = "string"
choices = [
    { name = "Newest", value = "new" },
    { name = "Oldest", value = "old" },
]
"#,
        );
        let skill = load_skill_toml(&path).unwrap();
        assert_eq!(skill.slash_options.len(), 2);

        let query = &skill.slash_options[0];
        assert_eq!(query.name, "query");
        assert_eq!(query.kind, "string");
        assert!(query.required);
        assert_eq!(query.max_length, Some(200));

        let sort = &skill.slash_options[1];
        assert_eq!(sort.name, "sort");
        assert!(!sort.required);
        assert_eq!(sort.choices.len(), 2);
        assert_eq!(sort.choices[0].name, "Newest");
        assert_eq!(sort.choices[0].value, "new");
    }

    #[test]
    fn description_localizations_parse_at_command_and_option_level() {
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"
[skill]
name = "search"
description = "Search the web"
version = "0.1.0"
tags = ["slash"]
description_localizations = { fr = "Rechercher sur le web", ja = "ウェブを検索" }

[[skill.slash_options]]
name = "query"
description = "The search query"
type = "string"
description_localizations = { fr = "La requête de recherche" }
"#,
        );
        let skill = load_skill_toml(&path).unwrap();
        assert_eq!(
            skill
                .description_localizations
                .get("fr")
                .map(String::as_str),
            Some("Rechercher sur le web")
        );
        assert_eq!(
            skill
                .description_localizations
                .get("ja")
                .map(String::as_str),
            Some("ウェブを検索")
        );
        assert_eq!(
            skill.slash_options[0]
                .description_localizations
                .get("fr")
                .map(String::as_str),
            Some("La requête de recherche")
        );
    }

    #[test]
    fn skills_without_slash_options_default_to_empty() {
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"
[skill]
name = "probe"
description = "test"
version = "0.1.0"
"#,
        );
        let skill = load_skill_toml(&path).unwrap();
        assert!(skill.slash_options.is_empty());
    }

    #[test]
    fn load_skill_md_parses_slash_options_from_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let md = r#"---
name: draft
description: Draft content to a spec.
tags: [slash]
slash_options:
  - name: format
    description: Output format.
    type: string
    required: true
    choices: [{name: Email, value: email}, {name: Tweet, value: tweet}]
  - name: words
    type: integer
    min: 10
    max: 2000
---
# Draft

Write it.
"#;
        let path = tmp.path().join("SKILL.md");
        std::fs::write(&path, md).unwrap();
        let skill = load_skill_md(&path, tmp.path()).unwrap();

        // Parity with SKILL.toml: the runtime Skill carries typed options.
        assert_eq!(skill.slash_options.len(), 2);
        assert_eq!(skill.slash_options[0].name, "format");
        assert!(skill.slash_options[0].required);
        assert_eq!(skill.slash_options[0].choices.len(), 2);
        assert_eq!(skill.slash_options[1].kind, "integer");
        assert_eq!(skill.slash_options[1].min, Some(10.0));
        assert_eq!(skill.slash_options[1].max, Some(2000.0));
        assert!(skill.tags.contains(&"slash".to_string()));

        // The options block lives in frontmatter, so the prompt (body) is clean.
        assert_eq!(skill.prompts.len(), 1);
        assert!(skill.prompts[0].contains("Write it."));
        assert!(!skill.prompts[0].contains("slash_options"));
    }

    #[test]
    fn load_skill_md_without_slash_options_is_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::write(&path, "---\nname: plain\ndescription: d\n---\n# Plain\n").unwrap();
        let skill = load_skill_md(&path, tmp.path()).unwrap();
        assert!(skill.slash_options.is_empty());
    }

    #[test]
    fn prompts_at_root_level_still_work() {
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"
[skill]
name = "probe"
description = "test"
version = "0.1.0"

prompts = ["legacy root-level prompt"]
"#,
        );
        let skill = load_skill_toml(&path).unwrap();
        assert_eq!(skill.prompts, vec!["legacy root-level prompt".to_string()]);
    }

    #[test]
    fn prompts_in_both_locations_are_merged_skill_first() {
        // Root-level prompts must precede the [skill] header in TOML.
        // Per the fix, [skill]-section prompts appear first in the merged
        // list, with root-level prompts appended after.
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(
            tmp.path(),
            r#"
prompts = ["from-root"]

[skill]
name = "probe"
description = "test"
version = "0.1.0"
prompts = ["from-skill-section"]
"#,
        );
        let skill = load_skill_toml(&path).unwrap();
        assert_eq!(
            skill.prompts,
            vec!["from-skill-section".to_string(), "from-root".to_string(),]
        );
    }
}

#[cfg(test)]
mod skill_manifest_tests {
    use super::*;

    #[test]
    fn parses_valid_skill_manifest() {
        let toml_str = r#"
[skill]
name = "x"
description = "y"
"#;
        let manifest: SkillManifest =
            toml::from_str(toml_str).expect("valid manifest should parse");
        assert_eq!(manifest.skill.name, "x");
        assert_eq!(manifest.skill.description, "y");
        assert_eq!(manifest.skill.version, "0.1.0");
        assert!(manifest.tools.is_empty());
        assert!(manifest.prompts.is_empty());
    }

    #[test]
    fn rejects_unknown_field_in_skill_block() {
        let toml_str = r#"
[skill]
name = "x"
description = "y"
descriptin = "oops"
"#;
        let err = toml::from_str::<SkillManifest>(toml_str)
            .expect_err("unknown field in [skill] should be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("descriptin"),
            "error should mention the unknown field 'descriptin'; got: {msg}"
        );
    }

    #[test]
    fn accepts_prompts_in_skill_block_with_strictness() {
        let toml_str = r#"
[skill]
name = "x"
description = "y"
prompts = ["one", "two"]
"#;
        let manifest: SkillManifest = toml::from_str(toml_str)
            .expect("manifest with prompts in [skill] should parse under deny_unknown_fields");
        assert_eq!(
            manifest.skill.prompts,
            vec!["one".to_string(), "two".to_string()]
        );
    }

    #[test]
    fn parses_skill_without_forge_block() {
        let toml_str = r#"
[skill]
name = "hand-authored"
description = "no forge block"
"#;
        let manifest: SkillManifest =
            toml::from_str(toml_str).expect("manifest without [forge] should parse cleanly");
        assert!(
            manifest.forge.is_none(),
            "forge should be None when [forge] is absent"
        );
        assert_eq!(manifest.skill.name, "hand-authored");
    }

    #[test]
    fn parses_skill_with_forge_block() {
        let toml_str = r#"
[skill]
name = "auto-integrated"
description = "from skillforge"

[forge]
source = "https://github.com/user/auto-integrated"
owner = "user"
language = "Rust"
license = true
stars = 42
updated_at = "2026-04-30"

[forge.requirements]
runtime = "zeroclaw >= 0.1"

[forge.metadata]
auto_integrated = true
forge_timestamp = "2026-04-30T12:00:00Z"
"#;
        let manifest: SkillManifest =
            toml::from_str(toml_str).expect("manifest with [forge] block should parse cleanly");
        let forge = manifest
            .forge
            .expect("forge should be Some when [forge] is present");
        assert_eq!(
            forge.source.as_deref(),
            Some("https://github.com/user/auto-integrated")
        );
        assert_eq!(forge.owner.as_deref(), Some("user"));
        assert_eq!(forge.language.as_deref(), Some("Rust"));
        assert_eq!(forge.license, Some(true));
        assert_eq!(forge.stars, Some(42));
        assert_eq!(forge.updated_at.as_deref(), Some("2026-04-30"));
        assert_eq!(
            forge.requirements.get("runtime").and_then(|v| v.as_str()),
            Some("zeroclaw >= 0.1"),
        );
        assert_eq!(
            forge
                .metadata
                .get("auto_integrated")
                .and_then(|v| v.as_bool()),
            Some(true),
        );
    }

    #[test]
    fn rejects_unknown_field_in_forge_block() {
        let toml_str = r#"
[skill]
name = "x"
description = "y"

[forge]
source = "https://github.com/user/x"
licence = true
"#;
        let err = toml::from_str::<SkillManifest>(toml_str)
            .expect_err("unknown field in [forge] should be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("licence"),
            "error should mention the unknown field 'licence'; got: {msg}"
        );
    }

    #[test]
    fn integrate_round_trip_emits_top_level_forge() {
        use crate::skillforge::scout::{ScoutResult, ScoutSource};
        use chrono::Utc;
        let candidate = ScoutResult {
            name: "round-trip".into(),
            url: "https://github.com/user/round-trip".into(),
            description: "round-trip test".into(),
            stars: 7,
            language: Some("Rust".into()),
            updated_at: Some(Utc::now()),
            source: ScoutSource::GitHub,
            owner: "user".into(),
            has_license: true,
        };

        // Generate the TOML the integrator would write and parse it back.
        let tmp = tempfile::TempDir::new().unwrap();
        let integrator = crate::skillforge::integrate::Integrator::new(
            tmp.path().to_string_lossy().into_owned(),
        );
        let skill_dir = integrator.integrate(&candidate).unwrap();
        let toml_str = std::fs::read_to_string(skill_dir.join("SKILL.toml")).unwrap();

        let manifest: SkillManifest = toml::from_str(&toml_str).unwrap_or_else(|e| {
            panic!(
                "integrator output must parse against SkillManifest with strict SkillMeta + ForgeMetadata; \
                 got error: {e}\n--- toml ---\n{toml_str}"
            )
        });
        let forge = manifest
            .forge
            .expect("integrator must emit a [forge] table");
        assert_eq!(forge.owner.as_deref(), Some("user"));
        assert_eq!(forge.stars, Some(7));
        assert_eq!(forge.license, Some(true));
        assert!(
            forge
                .source
                .as_deref()
                .is_some_and(|s| s.contains("round-trip")),
            "forge.source should carry the upstream URL"
        );
        assert_eq!(manifest.skill.name, "round-trip");
        assert_eq!(manifest.skill.description, "round-trip test");
    }

    #[test]
    fn workspace_swallow_site_skips_invalid_toml_without_panicking() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // Bad skill: typo in [skill] — rejected by deny_unknown_fields.
        let bad_dir = skills_dir.join("bad-skill");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(
            bad_dir.join("SKILL.toml"),
            r#"
[skill]
name = "bad"
description = "has a typo"
descriptin = "oops"
"#,
        )
        .unwrap();

        // Good skill: parses cleanly — must still load.
        let good_dir = skills_dir.join("good-skill");
        std::fs::create_dir_all(&good_dir).unwrap();
        std::fs::write(
            good_dir.join("SKILL.toml"),
            r#"
[skill]
name = "good"
description = "fine"
"#,
        )
        .unwrap();

        let (skills, dropped) = load_skills_from_directory(&skills_dir, false);
        // The bad skill is skipped (not panicked-on). The good skill loads.
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"good"),
            "good skill must load; got: {names:?}"
        );
        assert!(
            !names.contains(&"bad"),
            "bad skill must be skipped, not silently accepted; got: {names:?}"
        );
        // the skipped skill is surfaced as an audit drop, not silently lost.
        assert_eq!(dropped.len(), 1, "the bad TOML skill must be reported");
        assert_eq!(dropped[0].origin_hint, "workspace");
        assert!(matches!(
            dropped[0].reason,
            SkillDropReason::ManifestParseError(_)
        ));
    }

    #[test]
    fn workspace_script_bundling_skill_reported_as_scripts_blocked_drop() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let script_dir = skills_dir.join("script-skill");
        std::fs::create_dir_all(&script_dir).unwrap();
        std::fs::write(
            script_dir.join("SKILL.md"),
            "---\nname: script-skill\ndescription: bundles a shell helper\n---\n# Script Skill\n",
        )
        .unwrap();
        std::fs::write(script_dir.join("helper.sh"), "echo hi\n").unwrap();

        let (skills, dropped) = load_skills_from_directory(&skills_dir, false);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            !names.contains(&"script-skill"),
            "script-bundling skill must be dropped at the secure default; got: {names:?}"
        );
        assert_eq!(dropped.len(), 1, "the script skill must be reported");
        assert_eq!(dropped[0].origin_hint, "workspace");
        match &dropped[0].reason {
            SkillDropReason::AuditFindings {
                summary,
                scripts_blocked,
            } => {
                assert!(
                    *scripts_blocked,
                    "reason must flag scripts as the blocker; got: {summary}"
                );
                assert!(
                    summary.contains("script-like files are blocked"),
                    "summary must describe the script block; got: {summary}"
                );
            }
            other => panic!("expected AuditFindings, got: {other:?}"),
        }

        let (skills, dropped) = load_skills_from_directory(&skills_dir, true);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"script-skill"),
            "script-bundling skill must load once allow_scripts=true; got: {names:?}"
        );
        assert!(
            dropped.is_empty(),
            "no drops expected with allow_scripts=true; got: {dropped:?}"
        );
    }
    #[test]
    fn open_skills_swallow_site_skips_invalid_toml_without_panicking() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("open-skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let bad_dir = skills_dir.join("bad-open-skill");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(
            bad_dir.join("SKILL.toml"),
            r#"
[skill]
name = "bad-open"
description = "has a typo"
autor = "oops"
"#,
        )
        .unwrap();

        let good_dir = skills_dir.join("good-open-skill");
        std::fs::create_dir_all(&good_dir).unwrap();
        std::fs::write(
            good_dir.join("SKILL.toml"),
            r#"
[skill]
name = "good-open"
description = "fine"
"#,
        )
        .unwrap();

        let (skills, dropped) = load_open_skills_from_directory(&skills_dir, false);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(dropped.len(), 1, "the bad open-skill TOML must be reported");
        assert_eq!(dropped[0].origin_hint, "open-skills");
        assert!(
            names.contains(&"good-open"),
            "good open-skill must load; got: {names:?}"
        );
        assert!(
            !names.contains(&"bad-open"),
            "bad open-skill must be skipped, not silently accepted; got: {names:?}"
        );
    }
}

#[cfg(test)]
mod prompt_callable_name_tests {
    use super::*;
    use std::path::Path;

    fn tool(name: &str, kind: &str) -> SkillTool {
        SkillTool {
            name: name.to_string(),
            description: "desc".to_string(),
            kind: kind.to_string(),
            command: "echo hi".to_string(),
            args: HashMap::new(),
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        }
    }

    #[test]
    fn prompt_callable_name_matches_registered_tool_name() {
        let skill = Skill {
            name: "pr-review-toolkit:code-reviewer".to_string(),
            description: "review".to_string(),
            description_localizations: Default::default(),
            version: "1.0.0".to_string(),
            author: None,
            tags: Vec::new(),
            tools: vec![tool("run.lint", "shell")],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            location: None,
        };

        let prompt = skills_to_prompt_with_mode(
            std::slice::from_ref(&skill),
            Path::new("/tmp"),
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        );

        let registered =
            crate::tools::skill_tool::composed_tool_name(&skill.name, &skill.tools[0].name);
        assert!(
            prompt.contains(&format!("<name>{registered}</name>")),
            "prompt is missing the sanitized callable name `{registered}`:\n{prompt}",
        );
        // The raw, provider-invalid composed name must never reach the prompt.
        assert!(
            !prompt.contains("pr-review-toolkit:code-reviewer__run.lint"),
            "prompt advertised the raw, unsanitized composed name:\n{prompt}",
        );
    }

    fn tool_with_target(name: &str, kind: &str, target: &str) -> SkillTool {
        SkillTool {
            target: Some(target.to_string()),
            ..tool(name, kind)
        }
    }

    #[test]
    fn prompt_callable_predicate_matches_registration_preconditions() {
        // shell/script/http always register -> always prompt-callable.
        assert!(skill_tool_is_prompt_callable(&tool("run", "shell")));
        assert!(skill_tool_is_prompt_callable(&tool("run", "script")));
        assert!(skill_tool_is_prompt_callable(&tool("fetch", "http")));
        // builtin/mcp are elevation wrappers: callable only WITH a target.
        assert!(skill_tool_is_prompt_callable(&tool_with_target(
            "gen",
            "mcp",
            "images__generate"
        )));
        assert!(skill_tool_is_prompt_callable(&tool_with_target(
            "sh", "builtin", "shell"
        )));
        // ... and NOT callable without one (the converter's resolve_elevated_tool
        // would return None, so advertising them callable lies to the model).
        assert!(!skill_tool_is_prompt_callable(&tool("gen", "mcp")));
        assert!(!skill_tool_is_prompt_callable(&tool("sh", "builtin")));
        // A whitespace-only target is as good as absent.
        assert!(!skill_tool_is_prompt_callable(&tool_with_target(
            "gen", "mcp", "   "
        )));
        // unknown kinds are never callable.
        assert!(!skill_tool_is_prompt_callable(&tool("x", "weird")));
    }

    #[test]
    fn converter_skips_targetless_elevation_matching_the_prompt_predicate() {
        // The end-to-end invariant the renderer relies on: the registry converter
        // registers exactly the tools `skill_tool_is_prompt_callable` marks callable
        // (for what is statically decidable). A target-less builtin/mcp elevation
        // tool is skipped by the converter, so it must not be advertised callable.
        let security = std::sync::Arc::new(crate::security::SecurityPolicy::default());
        let skill = Skill {
            name: "ops".to_string(),
            description: "d".to_string(),
            description_localizations: Default::default(),
            version: "1.0.0".to_string(),
            author: None,
            tags: Vec::new(),
            tools: vec![
                tool("run", "shell"),  // always registers
                tool("orphan", "mcp"), // no target -> skipped
                tool("sh", "builtin"), // no target -> skipped
            ],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            location: None,
        };

        let registered: Vec<String> =
            crate::skills::skills_to_tools(std::slice::from_ref(&skill), security)
                .iter()
                .map(|t| t.name().to_string())
                .collect();

        // shell registers; the target-less elevation tools do not - matching the
        // prompt predicate for each.
        for t in &skill.tools {
            let composed = crate::tools::skill_tool::composed_tool_name(&skill.name, &t.name);
            let in_registry = registered.iter().any(|n| n == &composed);
            assert_eq!(
                in_registry,
                skill_tool_is_prompt_callable(t),
                "prompt-callable and registry-registered must agree for {} ({}): registry={in_registry}",
                t.name,
                t.kind,
            );
        }
    }

    #[test]
    fn prompt_lists_mcp_with_target_as_callable_and_targetless_as_not() {
        let skill = Skill {
            name: "imagegen".to_string(),
            description: "d".to_string(),
            description_localizations: Default::default(),
            version: "1.0.0".to_string(),
            author: None,
            tags: Vec::new(),
            tools: vec![
                tool_with_target("generate", "mcp", "images__generate"),
                tool("orphan", "mcp"), // no target -> not registered
            ],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            location: None,
        };

        let prompt = skills_to_prompt_with_mode(
            std::slice::from_ref(&skill),
            Path::new("/tmp"),
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        );

        // The callable block comes first, the unregistered <tools> block after.
        let callable_idx = prompt
            .find("<callable_tools")
            .expect("callable_tools block");
        let tools_at = prompt
            .find("<tools>")
            .expect("unregistered <tools> block present for the target-less mcp tool");
        assert!(
            callable_idx < tools_at,
            "callable block precedes unregistered block"
        );

        // The targeted mcp tool is advertised as callable (composed name, under
        // <callable_tools>, before the unregistered block).
        let callable = crate::tools::skill_tool::composed_tool_name(&skill.name, "generate");
        let callable_at = prompt
            .find(&format!("<name>{callable}</name>"))
            .expect("targeted mcp skill tool must be present as a callable name");
        assert!(
            callable_at > callable_idx && callable_at < tools_at,
            "targeted mcp skill tool must render under <callable_tools>:\n{prompt}"
        );

        // The target-less mcp tool renders under the unregistered <tools> block
        // (raw name, after the callable block) - the converter would skip it.
        let orphan_at = prompt
            .find("<name>orphan</name>")
            .expect("target-less mcp skill tool must be present under <tools>");
        assert!(
            orphan_at > tools_at,
            "target-less mcp skill tool must render as unregistered, not callable:\n{prompt}"
        );
    }
}

#[cfg(test)]
mod workspace_dir_regression_tests {
    use super::*;
    use tempfile::TempDir;

    fn make_config_with_agent_workspace(
        install_root: &Path,
        data_dir: &Path,
        agent_alias: &str,
        workspace_path: PathBuf,
    ) -> zeroclaw_config::schema::Config {
        let mut config = zeroclaw_config::schema::Config {
            config_path: install_root.join("config.toml"),
            data_dir: data_dir.to_path_buf(),
            ..Default::default()
        };

        let agent = zeroclaw_config::schema::AliasedAgentConfig {
            workspace: zeroclaw_config::multi_agent::AgentWorkspaceConfig {
                path: Some(workspace_path),
                ..Default::default()
            },
            ..Default::default()
        };

        config.agents.insert(agent_alias.to_string(), agent);
        config
    }

    fn write_test_skill(workspace: &Path, skill_name: &str) {
        let skill_dir = workspace.join("skills").join(skill_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.toml"),
            format!(
                r#"[skill]
name = "{skill_name}"
description = "regression test skill"
version = "0.1.0"
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn load_skills_for_agent_from_config_audited_returns_dropped() {
        let install_root = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();
        let agent_workspace = TempDir::new().unwrap();
        let agent_alias = "audit-agent";

        write_test_skill(agent_workspace.path(), "clean-skill");
        // A broken-manifest skill in the same workspace.
        let broken = agent_workspace.path().join("skills").join("broken-skill");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(
            broken.join("SKILL.toml"),
            "[skill]\nname = \"broken-skill\"\ndescription = \"d\"\nbogus = true\n",
        )
        .unwrap();

        let config = make_config_with_agent_workspace(
            install_root.path(),
            data_dir.path(),
            agent_alias,
            agent_workspace.path().to_path_buf(),
        );

        cache::invalidate();
        let (skills, dropped, _shadows) =
            load_skills_for_agent_from_config_audited(&config, agent_alias);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"clean-skill"), "got: {names:?}");
        assert!(!names.contains(&"broken-skill"), "got: {names:?}");
        assert_eq!(dropped.len(), 1, "the broken skill must be reported");
        assert_eq!(dropped[0].origin_hint, "workspace");
        assert!(matches!(
            dropped[0].reason,
            SkillDropReason::ManifestParseError(_)
        ));
    }

    #[test]
    fn load_skills_for_agent_from_config_uses_workspace_dir_not_data_dir() {
        let install_root = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();
        let agent_workspace = TempDir::new().unwrap();

        let agent_alias = "test-agent";
        let skill_name = "workspace-only-regression-skill";

        write_test_skill(agent_workspace.path(), skill_name);

        let config = make_config_with_agent_workspace(
            install_root.path(),
            data_dir.path(),
            agent_alias,
            agent_workspace.path().to_path_buf(),
        );

        let workspace_dir = config.agent_workspace_dir(agent_alias);
        assert_eq!(
            workspace_dir,
            agent_workspace.path(),
            "agent_workspace_dir must resolve to the custom workspace path"
        );
        assert_ne!(
            workspace_dir, config.data_dir,
            "workspace_dir and data_dir must be distinct for this test to be meaningful"
        );

        // Test the production helper — this is what the three call sites use.
        let skills_from_helper = load_skills_for_agent_from_config(&config, agent_alias);
        let helper_skill_names: Vec<&str> =
            skills_from_helper.iter().map(|s| s.name.as_str()).collect();
        assert!(
            helper_skill_names.contains(&skill_name),
            "load_skills_for_agent_from_config must load skills from agent workspace; got: {helper_skill_names:?}"
        );

        // Verify that using data_dir directly would NOT find the skill (the bug).
        let skills_from_data_dir = load_skills_for_agent(&config.data_dir, &config, agent_alias);
        let data_dir_skill_names: Vec<&str> = skills_from_data_dir
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            !data_dir_skill_names.contains(&skill_name),
            "skill in agent workspace must NOT be loaded when passing data_dir (this was the bug); got: {data_dir_skill_names:?}"
        );
    }

    #[test]
    fn load_skills_for_agent_from_config_empty_bundles_uses_workspace_dir() {
        let install_root = TempDir::new().unwrap();
        let data_dir = TempDir::new().unwrap();
        let agent_workspace = TempDir::new().unwrap();

        let agent_alias = "bundle-fallback-agent";
        let skill_name = "workspace-fallback-skill";

        write_test_skill(agent_workspace.path(), skill_name);

        let config = make_config_with_agent_workspace(
            install_root.path(),
            data_dir.path(),
            agent_alias,
            agent_workspace.path().to_path_buf(),
        );

        let skills = load_skills_for_agent_from_config(&config, agent_alias);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&skill_name),
            "with empty skill_bundles, workspace skills must still load; got: {names:?}"
        );
    }
}
