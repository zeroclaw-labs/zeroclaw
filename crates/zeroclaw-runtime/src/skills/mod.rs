pub mod skill_http;
pub mod skill_tool;
use anyhow::{Context, Result};
use directories::UserDirs;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use zip::ZipArchive;

pub mod audit;
pub mod creator;
pub mod improver;
pub mod testing;

const OPEN_SKILLS_REPO_URL: &str = "https://github.com/besoeasy/open-skills";
const OPEN_SKILLS_SYNC_MARKER: &str = ".zeroclaw-open-skills-sync";
const OPEN_SKILLS_SYNC_INTERVAL_SECS: u64 = 60 * 60 * 24 * 7;

// ─── ClawhHub / OpenClaw registry installers ───────────────────────────────
const CLAWHUB_DOMAIN: &str = "clawhub.ai";
const CLAWHUB_WWW_DOMAIN: &str = "www.clawhub.ai";
const CLAWHUB_DOWNLOAD_API: &str = "https://clawhub.ai/api/v1/download";
const MAX_CLAWHUB_ZIP_BYTES: u64 = 50 * 1024 * 1024; // 50 MiB

// ─── Skills registry (zeroclaw-skills) ────────────────────────────────────────
const SKILLS_REGISTRY_REPO_URL: &str = "https://github.com/zeroclaw-labs/zeroclaw-skills";
const SKILLS_REGISTRY_DIR_NAME: &str = "skills-registry";
const SKILLS_REGISTRY_SYNC_MARKER: &str = ".zeroclaw-skills-registry-sync";
const SKILLS_REGISTRY_SYNC_INTERVAL_SECS: u64 = 60 * 60 * 24;

/// A skill is a user-defined or community-built capability.
/// Skills live in `~/.zeroclaw/workspace/skills/<name>/SKILL.md`
/// and can include tool definitions, prompts, and automation scripts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tools: Vec<SkillTool>,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(skip)]
    pub location: Option<PathBuf>,
}

/// A tool defined by a skill (shell command, HTTP call, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTool {
    pub name: String,
    pub description: String,
    /// "shell", "http", "script"
    pub kind: String,
    /// The command/URL/script to execute
    pub command: String,
    #[serde(default)]
    pub args: HashMap<String, String>,
}

/// Skill manifest parsed from SKILL.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillManifest {
    skill: SkillMeta,
    /// SkillForge-emitted provenance metadata. Lives in a top-level `[forge]`
    /// table so that `SkillMeta` (the canonical skill-identity contract) is
    /// not coupled to the SkillForge integrator's emit format. Hand-authored
    /// SKILL.toml files omit this; auto-integrated skills carry it. See
    /// #6210 for the architectural rationale (FND-001 §4.2).
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
    #[serde(default = "default_version")]
    version: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    prompts: Vec<String>,
}

/// Provenance metadata emitted by the SkillForge integrator (see
/// `crates/zeroclaw-runtime/src/skillforge/integrate.rs`). Lives at the
/// top level of SKILL.toml under `[forge]`, kept separate from
/// `[skill]` so the canonical skill identity stays decoupled from the
/// integrator's emit format. Strict by design: a typo here is just as
/// bad as a typo in `[skill]` (silent misconfiguration of provenance).
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
    /// Free-form integrator metadata (e.g. `auto_integrated`,
    /// `forge_timestamp`).
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
}

fn default_version() -> String {
    "0.1.0".to_string()
}

/// Emit a user-visible warning when a skill directory is skipped due to audit
/// findings. When the findings mention blocked scripts and `allow_scripts` is
/// `false`, the message includes actionable remediation guidance so users know
/// how to enable their skill.
fn warn_skipped_skill(path: &Path, summary: &str, allow_scripts: bool) {
    let scripts_blocked = summary.contains("script-like files are blocked");
    if scripts_blocked && !allow_scripts {
        tracing::warn!(
            "skipping skill directory {}: {summary}. \
             To allow script files in skills, set `skills.allow_scripts = true` in your config.",
            path.display(),
        );
        eprintln!(
            "warning: skill '{}' was skipped because it contains script files. \
             Set `skills.allow_scripts = true` in your zeroclaw config to enable it.",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        );
    } else {
        tracing::warn!(
            "skipping insecure skill directory {}: {summary}",
            path.display(),
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
        tracing::warn!(
            "skill '{}': name mismatch between TOML ('{}') and SKILL.md ('{}')",
            dir_name,
            toml_skill.name,
            md_name,
        );
    }
    if let Some(ref md_desc) = parsed.meta.description {
        let md_desc = md_desc.trim();
        if !md_desc.is_empty() && md_desc != ">-" && md_desc != toml_skill.description.trim() {
            tracing::warn!(
                "skill '{}': description mismatch between TOML and SKILL.md — TOML takes precedence",
                dir_name,
            );
        }
    }
}

/// Load all skills from the workspace skills directory
pub fn load_skills(workspace_dir: &Path) -> Vec<Skill> {
    load_skills_with_open_skills_config(workspace_dir, None, None, None)
}

/// Load skills using runtime config values (preferred at runtime).
pub fn load_skills_with_config(
    workspace_dir: &Path,
    config: &zeroclaw_config::schema::Config,
) -> Vec<Skill> {
    #[allow(unused_mut)]
    let mut skills = load_skills_with_open_skills_config(
        workspace_dir,
        Some(config.skills.open_skills_enabled),
        config.skills.open_skills_dir.as_deref(),
        Some(config.skills.allow_scripts),
    );

    #[cfg(feature = "plugins-wasm")]
    skills.extend(load_plugin_skills_from_config(config));

    skills
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
}

fn load_skills_with_open_skills_config(
    workspace_dir: &Path,
    config_open_skills_enabled: Option<bool>,
    config_open_skills_dir: Option<&str>,
    config_allow_scripts: Option<bool>,
) -> Vec<Skill> {
    let mut skills = Vec::new();
    let allow_scripts = config_allow_scripts.unwrap_or(false);

    if let Some(open_skills_dir) =
        ensure_open_skills_repo(config_open_skills_enabled, config_open_skills_dir)
    {
        skills.extend(load_open_skills(&open_skills_dir, allow_scripts));
    }

    skills.extend(load_workspace_skills(workspace_dir, allow_scripts));
    skills
}

fn load_workspace_skills(workspace_dir: &Path, allow_scripts: bool) -> Vec<Skill> {
    let skills_dir = workspace_dir.join("skills");
    load_skills_from_directory(&skills_dir, allow_scripts)
}

pub fn load_skills_from_directory(skills_dir: &Path, allow_scripts: bool) -> Vec<Skill> {
    if !skills_dir.exists() {
        return Vec::new();
    }

    let mut skills = Vec::new();

    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return skills;
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
                warn_skipped_skill(&path, &summary, allow_scripts);
                continue;
            }
            Err(err) => {
                tracing::warn!(
                    "skipping unauditable skill directory {}: {err}",
                    path.display()
                );
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
                    tracing::warn!(
                        path = %toml_path.display(),
                        err  = %e,
                        "failed to load SKILL.toml — skill directory skipped",
                    );
                }
            }
        } else if md_path.exists()
            && let Ok(skill) = load_skill_md(&md_path, &path)
        {
            skills.push(skill);
        }
    }

    skills
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

fn load_open_skills_from_directory(skills_dir: &Path, allow_scripts: bool) -> Vec<Skill> {
    if !skills_dir.exists() {
        return Vec::new();
    }

    let mut skills = Vec::new();

    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return skills;
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
                warn_skipped_skill(&path, &summary, allow_scripts);
                continue;
            }
            Err(err) => {
                tracing::warn!(
                    "skipping unauditable open-skill directory {}: {err}",
                    path.display()
                );
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
                    tracing::warn!(
                        path = %toml_path.display(),
                        err  = %e,
                        "failed to load SKILL.toml — skill directory skipped",
                    );
                }
            }
        } else if md_path.exists()
            && let Ok(skill) = load_open_skill_md(&md_path)
        {
            skills.push(skill);
        }
    }

    skills
}

fn load_open_skills(repo_dir: &Path, allow_scripts: bool) -> Vec<Skill> {
    // Modern open-skills layout stores skill packages in `skills/<name>/SKILL.md`.
    // Prefer that structure to avoid treating repository docs (e.g. CONTRIBUTING.md)
    // as executable skills.
    let nested_skills_dir = repo_dir.join("skills");
    if nested_skills_dir.is_dir() {
        return load_open_skills_from_directory(&nested_skills_dir, allow_scripts);
    }

    let mut skills = Vec::new();

    let Ok(entries) = std::fs::read_dir(repo_dir) else {
        return skills;
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
                tracing::warn!(
                    "skipping insecure open-skill file {}: {}",
                    path.display(),
                    report.summary()
                );
                continue;
            }
            Err(err) => {
                tracing::warn!(
                    "skipping unauditable open-skill file {}: {err}",
                    path.display()
                );
                continue;
            }
        }

        if let Ok(skill) = load_open_skill_md(&path) {
            skills.push(skill);
        }
    }

    skills
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
            tracing::warn!(
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
            tracing::warn!(
                "open-skills update failed; using local copy from {}",
                repo_dir.display()
            );
        }
    }

    Some(repo_dir)
}

fn clone_open_skills_repo(repo_dir: &Path) -> bool {
    if let Some(parent) = repo_dir.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            "failed to create open-skills parent directory {}: {err}",
            parent.display()
        );
        return false;
    }

    let output = Command::new("git")
        .args(["clone", "--depth", "1", OPEN_SKILLS_REPO_URL])
        .arg(repo_dir)
        .output();

    match output {
        Ok(result) if result.status.success() => {
            tracing::info!("initialized open-skills at {}", repo_dir.display());
            true
        }
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            tracing::warn!("failed to clone open-skills: {stderr}");
            false
        }
        Err(err) => {
            tracing::warn!("failed to run git clone for open-skills: {err}");
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
            tracing::warn!("failed to pull open-skills updates: {stderr}");
            false
        }
        Err(err) => {
            tracing::warn!("failed to run git pull for open-skills: {err}");
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
    // dropped because SkillMeta had no `prompts` field. Fixes #5721.
    let mut prompts = manifest.skill.prompts;
    prompts.extend(manifest.prompts);

    Ok(Skill {
        name: manifest.skill.name,
        description: manifest.skill.description,
        version: manifest.skill.version,
        author: manifest.skill.author,
        tags: manifest.skill.tags,
        tools: manifest.tools,
        prompts,
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
        version: parsed.meta.version.unwrap_or_else(default_version),
        author: parsed.meta.author,
        tags: parsed.meta.tags,
        tools: Vec::new(),
        prompts: vec![parsed.body],
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
            if line.starts_with(' ') || line.starts_with('\t') {
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
        return relative.display().to_string();
    }
    location.display().to_string()
}

/// Build the "Available Skills" system prompt section with full skill instructions.
pub fn skills_to_prompt(skills: &[Skill], workspace_dir: &Path) -> String {
    skills_to_prompt_with_mode(
        skills,
        workspace_dir,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
    )
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
            // Tools with known kinds (shell, script, http) are registered as
            // callable tool specs and can be invoked directly via function calling.
            // We note them here for context but mark them as callable.
            let registered: Vec<_> = skill
                .tools
                .iter()
                .filter(|t| matches!(t.kind.as_str(), "shell" | "script" | "http"))
                .collect();
            let unregistered: Vec<_> = skill
                .tools
                .iter()
                .filter(|t| !matches!(t.kind.as_str(), "shell" | "script" | "http"))
                .collect();

            if !registered.is_empty() {
                let _ = writeln!(
                    prompt,
                    "    <callable_tools hint=\"These are registered as callable tool specs. Invoke them directly by name ({{}}.{{}}) instead of using shell.\">"
                );
                for tool in &registered {
                    let _ = writeln!(prompt, "      <tool>");
                    write_xml_text_element(
                        &mut prompt,
                        8,
                        "name",
                        &format!("{}.{}", skill.name, tool.name),
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

/// Convert skill tools into callable `Tool` trait objects.
///
/// Each skill's `[[tools]]` entries are converted to either `SkillShellTool`
/// (for `shell`/`script` kinds) or `SkillHttpTool` (for `http` kind),
/// enabling them to appear as first-class callable tool specs rather than
/// only as XML in the system prompt.
pub fn skills_to_tools(
    skills: &[Skill],
    security: std::sync::Arc<crate::security::SecurityPolicy>,
) -> Vec<Box<dyn zeroclaw_api::tool::Tool>> {
    let mut tools: Vec<Box<dyn zeroclaw_api::tool::Tool>> = Vec::new();
    for skill in skills {
        for tool in &skill.tools {
            match tool.kind.as_str() {
                "shell" | "script" => {
                    tools.push(Box::new(crate::skills::skill_tool::SkillShellTool::new(
                        &skill.name,
                        tool,
                        security.clone(),
                    )));
                }
                "http" => {
                    tools.push(Box::new(crate::skills::skill_http::SkillHttpTool::new(
                        &skill.name,
                        tool,
                    )));
                }
                other => {
                    tracing::warn!(
                        "Unknown skill tool kind '{}' for {}.{}, skipping",
                        other,
                        skill.name,
                        tool.name
                    );
                }
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

fn is_clawhub_host(host: &str) -> bool {
    host.eq_ignore_ascii_case(CLAWHUB_DOMAIN) || host.eq_ignore_ascii_case(CLAWHUB_WWW_DOMAIN)
}

fn parse_clawhub_url(source: &str) -> Option<Url> {
    let parsed = Url::parse(source).ok()?;
    match parsed.scheme() {
        "https" | "http" => {}
        _ => return None,
    }

    if !parsed.host_str().is_some_and(is_clawhub_host) {
        return None;
    }

    Some(parsed)
}

pub fn is_clawhub_source(source: &str) -> bool {
    if source.starts_with("clawhub:") {
        return true;
    }
    parse_clawhub_url(source).is_some()
}

fn clawhub_download_url(source: &str) -> Result<String> {
    // Short prefix: clawhub:<slug>
    if let Some(slug) = source.strip_prefix("clawhub:") {
        let slug = slug.trim().trim_end_matches('/');
        if slug.is_empty() || slug.contains('/') {
            anyhow::bail!(
                "invalid clawhub source '{}': expected 'clawhub:<slug>' (no slashes in slug)",
                source
            );
        }
        return Ok(format!("{CLAWHUB_DOWNLOAD_API}?slug={slug}"));
    }

    // Profile URL: https://clawhub.ai/<owner>/<slug> or https://www.clawhub.ai/<slug>
    if let Some(parsed) = parse_clawhub_url(source) {
        let path = parsed
            .path_segments()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("/");

        if path.is_empty() {
            anyhow::bail!("could not extract slug from ClawhHub URL: {source}");
        }

        return Ok(format!("{CLAWHUB_DOWNLOAD_API}?slug={path}"));
    }

    anyhow::bail!("unrecognised ClawhHub source format: {source}")
}

fn normalize_skill_name(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c == '-' { '_' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

fn clawhub_skill_dir_name(source: &str) -> Result<String> {
    if let Some(slug) = source.strip_prefix("clawhub:") {
        let slug = slug.trim().trim_end_matches('/');
        let base = slug.rsplit('/').next().unwrap_or(slug);
        let name = normalize_skill_name(base);
        return Ok(if name.is_empty() {
            "skill".to_string()
        } else {
            name
        });
    }

    let parsed = parse_clawhub_url(source)
        .ok_or_else(|| anyhow::anyhow!("invalid clawhub URL: {source}"))?;

    let path = parsed
        .path_segments()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let base = path.last().copied().unwrap_or("skill");
    let name = normalize_skill_name(base);
    Ok(if name.is_empty() {
        "skill".to_string()
    } else {
        name
    })
}

pub fn is_git_source(source: &str) -> bool {
    // ClawHub URLs look like https:// but are not git repos
    if is_clawhub_source(source) {
        return false;
    }
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
            .with_context(|| format!("failed to remove {}", git_dir.display()))?;
    }
    Ok(())
}

fn copy_dir_recursive_secure(src: &Path, dest: &Path) -> Result<()> {
    let src_meta = std::fs::symlink_metadata(src)
        .with_context(|| format!("failed to read metadata for {}", src.display()))?;
    if src_meta.file_type().is_symlink() {
        anyhow::bail!(
            "Refusing to copy symlinked skill source path: {}",
            src.display()
        );
    }
    if !src_meta.is_dir() {
        anyhow::bail!("Skill source must be a directory: {}", src.display());
    }

    std::fs::create_dir_all(dest)
        .with_context(|| format!("failed to create destination {}", dest.display()))?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&src_path)
            .with_context(|| format!("failed to read metadata for {}", src_path.display()))?;

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
                    src_path.display(),
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
        anyhow::bail!("Destination skill already exists: {}", dest.display());
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

pub fn install_clawhub_skill_source(
    source: &str,
    skills_path: &Path,
    allow_scripts: bool,
) -> Result<(PathBuf, usize)> {
    let download_url = clawhub_download_url(source)
        .with_context(|| format!("invalid ClawhHub source: {source}"))?;
    let skill_dir_name = clawhub_skill_dir_name(source)?;
    let installed_dir = skills_path.join(&skill_dir_name);
    if installed_dir.exists() {
        anyhow::bail!(
            "Destination skill already exists: {}",
            installed_dir.display()
        );
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let resp = client
        .get(&download_url)
        .send()
        .with_context(|| format!("failed to fetch zip from {download_url}"))?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        anyhow::bail!("ClawhHub rate limit reached (HTTP 429). Wait a moment and retry.");
    }
    if !resp.status().is_success() {
        anyhow::bail!("ClawhHub download failed (HTTP {})", resp.status());
    }

    let bytes = resp.bytes()?.to_vec();
    if bytes.len() as u64 > MAX_CLAWHUB_ZIP_BYTES {
        anyhow::bail!(
            "ClawhHub zip rejected: too large ({} bytes > {})",
            bytes.len(),
            MAX_CLAWHUB_ZIP_BYTES
        );
    }

    std::fs::create_dir_all(&installed_dir)?;

    let cursor = Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor).context("downloaded content is not a valid zip")?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().to_string();

        if raw_name.is_empty()
            || raw_name.contains("..")
            || raw_name.starts_with('/')
            || raw_name.contains('\\')
            || raw_name.contains(':')
        {
            let _ = std::fs::remove_dir_all(&installed_dir);
            anyhow::bail!("zip entry contains unsafe path: {raw_name}");
        }

        let out_path = installed_dir.join(&raw_name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut out_file = std::fs::File::create(&out_path)
            .with_context(|| format!("failed to create extracted file: {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out_file)?;
    }

    let has_manifest = installed_dir.join("SKILL.md").exists()
        || installed_dir.join("SKILL.toml").exists()
        || installed_dir.join("manifest.toml").exists();
    if !has_manifest {
        std::fs::write(
            installed_dir.join("SKILL.toml"),
            format!(
                "[skill]\nname = \"{}\"\ndescription = \"ClawhHub installed skill\"\nversion = \"0.1.0\"\n",
                skill_dir_name
            ),
        )?;
    }

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

fn clone_skills_registry(registry_dir: &Path, repo_url: &str) -> Result<()> {
    if let Some(parent) = registry_dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create registry parent: {}", parent.display()))?;
    }

    let output = Command::new("git")
        .args(["clone", "--depth", "1", repo_url])
        .arg(registry_dir)
        .output()
        .context("failed to run git clone for skills registry")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("failed to clone skills registry: {stderr}");
    }

    tracing::info!("cloned skills registry to {}", registry_dir.display());
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
            tracing::warn!("failed to pull skills registry updates: {stderr}");
            false
        }
        Err(err) => {
            tracing::warn!("failed to run git pull for skills registry: {err}");
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
            tracing::warn!(
                "skills registry update failed; using local copy from {}",
                registry_dir.display()
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

pub fn install_registry_skill_source(
    source: &str,
    skills_path: &Path,
    allow_scripts: bool,
    workspace_dir: &Path,
    registry_url: Option<&str>,
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

    install_local_skill_source(
        skill_dir.to_str().with_context(|| {
            format!("registry path is not valid UTF-8: {}", skill_dir.display())
        })?,
        skills_path,
        allow_scripts,
    )
}

// ─── Plugin-shipped skills (plugins-wasm only) ───────────────────────────────

/// Load skills from skill-capable plugins discovered by the plugin host.
///
/// Each plugin's `skills/` directory is fed to the existing skill loader, and
/// every loaded skill is renamed to `plugin:<plugin>/<skill>` to avoid
/// collisions with user-authored skills and between bundles. The `plugin:<name>`
/// tag is also added so prompts can distinguish plugin skills.
#[cfg(feature = "plugins-wasm")]
pub fn load_plugin_skills_from_config(config: &zeroclaw_config::schema::Config) -> Vec<Skill> {
    if !config.plugins.enabled {
        return Vec::new();
    }

    let plugins_dir = expand_plugins_dir(&config.plugins.plugins_dir);
    let parent = match plugins_dir.parent() {
        Some(p) => p.to_path_buf(),
        None => return Vec::new(),
    };

    let signature_mode = zeroclaw_plugins::host::PluginHost::parse_signature_mode(
        &config.plugins.security.signature_mode,
    );
    let trusted_keys = config.plugins.security.trusted_publisher_keys.clone();

    let host = match zeroclaw_plugins::host::PluginHost::with_security(
        &parent,
        signature_mode,
        trusted_keys,
    ) {
        Ok(host) => host,
        Err(err) => {
            tracing::warn!("failed to discover plugin skills: {err}");
            return Vec::new();
        }
    };

    let allow_scripts = config.skills.allow_scripts;
    let mut skills = Vec::new();
    for (manifest, skills_dir) in host.skill_plugin_details() {
        for raw in load_skills_from_directory(&skills_dir, allow_scripts) {
            skills.push(namespace_plugin_skill(&manifest.name, raw));
        }
    }
    skills
}

#[cfg(feature = "plugins-wasm")]
fn expand_plugins_dir(plugins_dir: &str) -> PathBuf {
    if let Some(rest) = plugins_dir.strip_prefix("~/")
        && let Some(dirs) = UserDirs::new()
    {
        return dirs.home_dir().join(rest);
    }
    PathBuf::from(plugins_dir)
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
    fn test_is_registry_source_rejects_clawhub() {
        assert!(!is_registry_source("clawhub:my-skill"));
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

    /// Positive control covering the new field × strictness intersection:
    /// after the rebase onto master (which added `prompts: Vec<String>`
    /// to `SkillMeta` per #5972), the field must continue to parse cleanly
    /// under `#[serde(deny_unknown_fields)]`.
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

    /// Hand-authored skills that don't carry SkillForge provenance must parse
    /// without error — `forge` is `Option<ForgeMetadata>` with `default`.
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

    /// Happy path: a SkillForge-emitted manifest with a fully populated
    /// `[forge]` table, including the nested `[forge.requirements]` and
    /// `[forge.metadata]` sub-tables.
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

    /// `ForgeMetadata` carries `#[serde(deny_unknown_fields)]` — a typo at
    /// the `[forge]` level (e.g. `licence` next to `license`) must surface
    /// loudly the same way a typo in `[skill]` does.
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

    /// Round-trip guard: the SkillForge integrator must emit `[forge]` keys
    /// at the top level (sibling to `[skill]`), not inside `[skill]`. If a
    /// future refactor moves these back, this test fails because the parsed
    /// manifest's `forge` field would be `None` (and `SkillMeta` would
    /// reject the unknown keys via `deny_unknown_fields`).
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
        // Crucial guard: none of the provenance keys leaked into [skill].
        // A failure here means generate_toml regressed and is putting forge
        // keys back inside `[skill]` — `deny_unknown_fields` on `SkillMeta`
        // would have caught that already as a parse error, but assert
        // explicitly so the failure is unambiguous in CI output.
        assert_eq!(manifest.skill.name, "round-trip");
        assert_eq!(manifest.skill.description, "round-trip test");
    }

    /// Behavioral assertion for the swallow-site fix: a SKILL.toml whose
    /// `[skill]` block has a typo causes `load_skill_toml` to return `Err`,
    /// and `load_skills_from_directory` skips it without panicking and
    /// without including it in the loaded set. The accompanying
    /// `tracing::warn!` call (with structured `path` and `err` fields) is
    /// verified by source inspection — the codebase does not currently
    /// pull in a `tracing-subscriber` test harness, and adding one purely
    /// for this assertion would violate the AGENTS.md anti-pattern of
    /// adding dependencies for minor convenience.
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

        let skills = load_skills_from_directory(&skills_dir, false);
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
    }

    /// Behavioral assertion for the open-skills swallow-site fix.
    /// Same shape as the workspace test above; covers `load_open_skills_from_directory`.
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

        let skills = load_open_skills_from_directory(&skills_dir, false);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
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
