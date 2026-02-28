use anyhow::{Context, Result};
use directories::UserDirs;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

mod audit;
mod templates;

const OPEN_SKILLS_REPO_URL: &str = "https://github.com/besoeasy/open-skills";
const OPEN_SKILLS_SYNC_MARKER: &str = ".zeroclaw-open-skills-sync";
const OPEN_SKILLS_SYNC_INTERVAL_SECS: u64 = 60 * 60 * 24 * 7;

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
    #[serde(default)]
    tools: Vec<SkillTool>,
    #[serde(default)]
    prompts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillMeta {
    name: String,
    description: String,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

/// Load all skills from the workspace skills directory
pub fn load_skills(workspace_dir: &Path) -> Vec<Skill> {
    load_skills_with_open_skills_config(workspace_dir, None, None, None)
}

/// Load skills using runtime config values (preferred at runtime).
pub fn load_skills_with_config(workspace_dir: &Path, config: &crate::config::Config) -> Vec<Skill> {
    load_skills_with_open_skills_config(
        workspace_dir,
        Some(config.skills.open_skills_enabled),
        config.skills.open_skills_dir.as_deref(),
        Some(config.skills.allow_scripts),
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

fn load_skills_from_directory(skills_dir: &Path, allow_scripts: bool) -> Vec<Skill> {
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
                tracing::warn!(
                    "skipping insecure skill directory {}: {}",
                    path.display(),
                    report.summary()
                );
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

        // Try SKILL.toml first, then SKILL.md
        let manifest_path = path.join("SKILL.toml");
        let md_path = path.join("SKILL.md");

        if manifest_path.exists() {
            if let Ok(skill) = load_skill_toml(&manifest_path) {
                skills.push(skill);
            }
        } else if md_path.exists() {
            if let Ok(skill) = load_skill_md(&md_path, &path) {
                skills.push(skill);
            }
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
        return load_skills_from_directory(&nested_skills_dir, allow_scripts);
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
        // Never clone from the network during tests — tests that need a local
        // open-skills directory must provide one via config.skills.open_skills_dir.
        #[cfg(test)]
        return None;

        #[cfg(not(test))]
        {
            if !clone_open_skills_repo(&repo_dir) {
                return None;
            }
            let _ = mark_open_skills_synced(&repo_dir);
            return Some(repo_dir);
        }
    }

    // Never pull from the network during tests.
    #[cfg(not(test))]
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
    if let Some(parent) = repo_dir.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                "failed to create open-skills parent directory {}: {err}",
                parent.display()
            );
            return false;
        }
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

    Ok(Skill {
        name: manifest.skill.name,
        description: manifest.skill.description,
        version: manifest.skill.version,
        author: manifest.skill.author,
        tags: manifest.skill.tags,
        tools: manifest.tools,
        prompts: manifest.prompts,
        location: Some(path.to_path_buf()),
    })
}

/// Load a skill from a SKILL.md file (simpler format)
fn load_skill_md(path: &Path, dir: &Path) -> Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    let mut name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut version = "0.1.0".to_string();
    let mut author: Option<String> = None;

    // If _meta.json exists alongside SKILL.md, use it for name/version/author.
    // This covers skills installed from zip-based registries (e.g. any zip source).
    let meta_path = dir.join("_meta.json");
    if meta_path.exists() {
        if let Ok(raw) = std::fs::read(&meta_path) {
            if let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&raw) {
                if let Some(slug) = meta.get("slug").and_then(|v| v.as_str()) {
                    let normalized = normalize_skill_name(slug.split('/').last().unwrap_or(slug));
                    if !normalized.is_empty() {
                        name = normalized;
                    }
                }
                if let Some(v) = meta.get("version").and_then(|v| v.as_str()) {
                    version = v.to_string();
                }
                if let Some(owner) = meta.get("ownerId").and_then(|v| v.as_str()) {
                    author = Some(owner.to_string());
                }
            }
        }
    }

    Ok(Skill {
        name,
        description: extract_description(&content),
        version,
        author,
        tags: Vec::new(),
        tools: Vec::new(),
        prompts: vec![content],
        location: Some(path.to_path_buf()),
    })
}

fn load_open_skill_md(path: &Path) -> Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("open-skill")
        .to_string();

    Ok(Skill {
        name,
        description: extract_description(&content),
        version: "open-skills".to_string(),
        author: Some("besoeasy/open-skills".to_string()),
        tags: vec!["open-skills".to_string()],
        tools: Vec::new(),
        prompts: vec![content],
        location: Some(path.to_path_buf()),
    })
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
    if prefer_relative {
        if let Ok(relative) = location.strip_prefix(workspace_dir) {
            return relative.display().to_string();
        }
    }
    location.display().to_string()
}

/// Build the "Available Skills" system prompt section with full skill instructions.
pub fn skills_to_prompt(skills: &[Skill], workspace_dir: &Path) -> String {
    skills_to_prompt_with_mode(
        skills,
        workspace_dir,
        crate::config::SkillsPromptInjectionMode::Full,
    )
}

/// Build the "Available Skills" system prompt section with configurable verbosity.
pub fn skills_to_prompt_with_mode(
    skills: &[Skill],
    workspace_dir: &Path,
    mode: crate::config::SkillsPromptInjectionMode,
) -> String {
    use std::fmt::Write;

    if skills.is_empty() {
        return String::new();
    }

    let mut prompt = match mode {
        crate::config::SkillsPromptInjectionMode::Full => String::from(
            "## Available Skills\n\n\
             Skill instructions and tool metadata are preloaded below.\n\
             Follow these instructions directly; do not read skill files at runtime unless the user asks.\n\n\
             <available_skills>\n",
        ),
        crate::config::SkillsPromptInjectionMode::Compact => String::from(
            "## Available Skills\n\n\
             Skill summaries are preloaded below to keep context compact.\n\
             Skill instructions are loaded on demand: read the skill file in `location` only when needed.\n\n\
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
            matches!(mode, crate::config::SkillsPromptInjectionMode::Compact),
        );
        write_xml_text_element(&mut prompt, 4, "location", &location);

        if matches!(mode, crate::config::SkillsPromptInjectionMode::Full) {
            if !skill.prompts.is_empty() {
                let _ = writeln!(prompt, "    <instructions>");
                for instruction in &skill.prompts {
                    write_xml_text_element(&mut prompt, 6, "instruction", instruction);
                }
                let _ = writeln!(prompt, "    </instructions>");
            }

            if !skill.tools.is_empty() {
                let _ = writeln!(prompt, "    <tools>");
                for tool in &skill.tools {
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

fn is_git_source(source: &str) -> bool {
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

fn install_local_skill_source(
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

fn install_git_skill_source(
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

// ─── Scaffold (zeroclaw skill new) ───────────────────────────────────────────

/// Create a new skill project from a named template.
///
/// Protocol: the generated WASM tool reads JSON from **stdin** and writes a
/// `{"success":bool,"output":"...","error":null|"..."}` JSON to **stdout**.
/// No custom SDK or ABI boilerplate needed — just standard WASI stdio.
pub fn scaffold_skill(
    name: &str,
    template_name: &str,
    dest_parent: &std::path::Path,
) -> Result<()> {
    // Validate name: allowlist only ASCII alphanumeric, '_', '-'; no path traversal.
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        anyhow::bail!(
            "Invalid skill name '{}': use only letters, digits, '_', or '-' (snake_case or kebab-case)",
            name
        );
    }

    let tmpl = templates::find(template_name).ok_or_else(|| {
        let names: Vec<&str> = templates::ALL.iter().map(|t| t.name).collect();
        anyhow::anyhow!(
            "Unknown template '{template_name}'. Run 'zeroclaw skill templates' to list available templates.\nAvailable: {}",
            names.join(", ")
        )
    })?;

    let skill_dir = dest_parent.join(name);
    if skill_dir.exists() {
        anyhow::bail!("Directory already exists: {}", skill_dir.display());
    }
    std::fs::create_dir_all(&skill_dir)?;

    let bin_name = name.replace('-', "_");

    // Run all file writes in a closure; remove skill_dir on any error to avoid
    // leaving a partial scaffold behind (mirrors install_registry_skill_source).
    let result = (|| -> Result<()> {
        for file in tmpl.files {
            let path = skill_dir.join(file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let content = templates::apply(file.content, name, &bin_name);
            std::fs::write(&path, content)?;
        }

        // Common files not in templates
        std::fs::write(
            skill_dir.join(".gitignore"),
            "tool.wasm\nnode_modules/\ntarget/\n*.js.map\n",
        )?;
        write_skill_md(&skill_dir, name, tmpl.description, tmpl.test_args)?;
        write_readme(&skill_dir, name, tmpl.language, tmpl.test_args)?;

        Ok(())
    })();

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&skill_dir);
            Err(e)
        }
    }
}

fn write_skill_md(
    dir: &std::path::Path,
    name: &str,
    description: &str,
    test_args: &str,
) -> Result<()> {
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "# {name}\n\n\
             {description}\n\n\
             ## Tools\n\n\
             ### {name}\n\n\
             {description}\n\n\
             **Example:**\n\
             ```json\n\
             {test_args}\n\
             ```\n\n\
             ## Test\n\n\
             ```bash\n\
             zeroclaw skill test . --args '{test_args}'\n\
             ```\n"
        ),
    )?;
    Ok(())
}

fn write_readme(dir: &std::path::Path, name: &str, language: &str, test_args: &str) -> Result<()> {
    let (build_cmd, test_note) = match language {
        "typescript" => (
            "npm install && npm run build",
            "Requires: node, npm, javy (https://github.com/bytecodealliance/javy)",
        ),
        "rust" => (
            "cargo build --target wasm32-wasip1 --release\ncp target/wasm32-wasip1/release/*.wasm tool.wasm",
            "Requires: rustup target add wasm32-wasip1  # one-time setup",
        ),
        "go" => (
            "tinygo build -o tool.wasm -target wasi .",
            "Requires: tinygo (https://tinygo.org)",
        ),
        "python" => (
            "componentize-py -d wit/ -w zeroclaw-skill componentize main -o tool.wasm",
            "Requires: componentize-py (pip install componentize-py)",
        ),
        _ => ("make", ""),
    };

    std::fs::write(
        dir.join("README.md"),
        format!(
            "# {name}\n\n\
             A ZeroClaw skill ({language}).\n\n\
             ## Protocol\n\n\
             Reads a JSON object from **stdin**, writes JSON to **stdout**:\n\n\
             ```json\n\
             // stdin  → args\n\
             {test_args}\n\n\
             // stdout ← result\n\
             {{\"success\": true, \"output\": \"...\"}}\n\
             ```\n\n\
             ## Build\n\n\
             {test_note}\n\n\
             ```bash\n\
             {build_cmd}\n\
             ```\n\n\
             ## Test\n\n\
             ```bash\n\
             zeroclaw skill test . --args '{test_args}'\n\
             ```\n\n\
             ## Publish\n\n\
             ```bash\n\
             zeroclaw skill install .\n\
             ```\n"
        ),
    )?;
    Ok(())
}

// ─── Local test (zeroclaw skill test) ────────────────────────────────────────

/// Run a WASM tool locally using the system `wasmtime` CLI binary.
///
/// Looks for `tool.wasm` inside `skill_path/tools/<tool_name>/` (installed layout)
/// OR directly as `skill_path/tool.wasm` (dev layout — right after build).
pub fn test_skill_locally(
    skill_path: &std::path::Path,
    tool_name: Option<&str>,
    args_json: &str,
) -> Result<()> {
    // Resolve .wasm path
    let wasm_path = resolve_wasm_path(skill_path, tool_name)?;

    // Validate JSON args
    let _: serde_json::Value = serde_json::from_str(args_json)
        .with_context(|| format!("--args is not valid JSON: {args_json}"))?;

    println!(
        "  Running: {} {}",
        console::style("wasmtime").cyan(),
        wasm_path.display()
    );
    println!("  Input:   {args_json}");
    println!();

    // Run via wasmtime CLI (captures stdout as tool output)
    let output = std::process::Command::new("wasmtime")
        .arg("run")
        .arg(&wasm_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context(
            "wasmtime not found — install it first:\n\n\
             \x20 macOS (Homebrew):  brew install wasmtime\n\
             \x20 macOS/Linux:       curl https://wasmtime.dev/install.sh -sSf | bash\n\
             \x20 Cargo (slow):      cargo install wasmtime-cli\n\n\
             After installing, restart your terminal and run this command again.\n\
             Docs: https://wasmtime.dev",
        )
        .and_then(|mut child| {
            use std::io::Write;
            // take() moves stdin out so it is dropped (closed) at end of block,
            // sending EOF to the child process — required for read_to_string to return.
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(args_json.as_bytes())?;
                // stdin dropped here → EOF sent
            }
            child.wait_with_output().map_err(anyhow::Error::from)
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("wasmtime exited with error:\n{stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{}", stdout);

    // Pretty-print if valid JSON
    match serde_json::from_str::<serde_json::Value>(&stdout) {
        Ok(v) => {
            println!();
            let success = v.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
            if success {
                println!(
                    "  {} Tool returned success",
                    console::style("✓").green().bold()
                );
            } else {
                let err = v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");
                println!(
                    "  {} Tool returned failure: {err}",
                    console::style("✗").red().bold()
                );
            }
        }
        Err(_) => {
            // stdout is not JSON — show as-is (maybe the tool printed plain text)
        }
    }

    Ok(())
}

/// Find the `.wasm` file for a skill directory.
///
/// Search order:
/// 1. `<path>/tool.wasm`                                   — dev build output
/// 2. `<path>/tools/<tool_name>/tool.wasm`                 — installed layout
/// 3. First `<path>/tools/*/tool.wasm` found               — installed, no name given
fn resolve_wasm_path(
    skill_path: &std::path::Path,
    tool_name: Option<&str>,
) -> Result<std::path::PathBuf> {
    // 1. Direct dev layout
    let direct = skill_path.join("tool.wasm");
    if direct.exists() {
        return Ok(direct);
    }

    // 2. Named tool inside installed layout
    if let Some(name) = tool_name {
        // Validate: must be a single normal path component (no traversal or separators).
        use std::path::Component;
        let name_path = std::path::Path::new(name);
        let is_single_normal = {
            let mut comps = name_path.components();
            matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
        };
        if !is_single_normal || name != name_path.file_name().and_then(|n| n.to_str()).unwrap_or("")
        {
            anyhow::bail!("invalid tool name '{}': must be a simple filename", name);
        }
        let named = skill_path.join("tools").join(name).join("tool.wasm");
        if named.exists() {
            return Ok(named);
        }
        anyhow::bail!(
            "tool.wasm not found for tool '{}' in {}",
            name,
            skill_path.display()
        );
    }

    // 3. First tool found
    let tools_dir = skill_path.join("tools");
    if let Ok(entries) = std::fs::read_dir(&tools_dir) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("tool.wasm");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    anyhow::bail!(
        "No tool.wasm found in {}.\n\
         Run the build command for your template first (e.g. 'npm run build', 'cargo build').",
        skill_path.display()
    )
}

// ─── Registry (ZeroMarket) source ────────────────────────────────────────────

/// Package reference format: `<namespace>/<name>[@<version>]`
/// Example: `zeromarket/github-pr-summary` or `acme/my-tool@0.2.1`
fn is_registry_source(source: &str) -> bool {
    // Filesystem paths are never registry sources
    if source.starts_with('.') || source.starts_with('/') || source.starts_with('~') {
        return false;
    }
    // Must be `namespace/name` (no scheme, no .git suffix, no slashes in name)
    let parts: Vec<&str> = source.split('/').collect();
    if parts.len() != 2 {
        return false;
    }
    let (ns, name_ver) = (parts[0], parts[1]);
    // Reject empty segments or path-traversal
    if ns.is_empty() || name_ver.is_empty() || ns.contains("..") || name_ver.contains("..") {
        return false;
    }
    // Must be identifier-safe characters only ('.' and '@' only in version segment)
    let is_safe_id = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    };
    // name_ver may be `name` or `name@version`; '@' is only allowed once as separator
    let (base_name, version_part) = match name_ver.split_once('@') {
        Some((n, v)) => (n, Some(v)),
        None => (name_ver, None),
    };
    if base_name.is_empty() {
        return false;
    }
    if let Some(v) = version_part {
        if v.is_empty() || v.contains('@') {
            return false;
        }
        if !is_safe_id(v) {
            return false;
        }
    }
    is_safe_id(ns) && is_safe_id(base_name)
}

/// Download a skill package (WASM tools + SKILL.toml) from the ZeroMarket registry.
///
/// Package layout on the registry:
/// ```text
/// GET <registry_url>/v1/packages/<namespace>/<name>[/<version>]
/// -> 200 JSON: { "name": "...", "version": "...", "tools": [{ "name": "...", "wasm_url": "...", "manifest_url": "..." }] }
/// ```
///
/// The function:
/// 1. Fetches the package index JSON
/// 2. Creates `skills_path/<name>/tools/<tool-name>/`
/// 3. Downloads `tool.wasm` and `manifest.json` for each tool
/// 4. Creates a minimal `SKILL.toml` so the skill shows up in `skill list`
fn install_registry_skill_source(
    source: &str,
    skills_path: &Path,
    registry_url: &str,
) -> Result<(PathBuf, usize)> {
    // Parse `namespace/name[@version]`
    let (ns_name, version) = match source.split_once('@') {
        Some((base, ver)) => (base, Some(ver)),
        None => (source, None),
    };
    let parts: Vec<&str> = ns_name.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        anyhow::bail!(
            "invalid registry source '{}': expected namespace/name[@version]",
            source
        );
    }
    if let Some(v) = version {
        if v.is_empty() || v.contains('/') {
            anyhow::bail!(
                "invalid version in '{}': version must be non-empty and contain no '/'",
                source
            );
        }
    }
    let (namespace, pkg_name) = (parts[0], parts[1]);

    // Build registry API URL
    let api_path = match version {
        Some(v) => format!("v1/packages/{namespace}/{pkg_name}/{v}"),
        None => format!("v1/packages/{namespace}/{pkg_name}"),
    };
    let api_url = format!("{}/{}", registry_url.trim_end_matches('/'), api_path);

    println!("  Fetching package index: {api_url}");

    // HTTP GET (synchronous via ureq-like reqwest blocking or std)
    // We use std::process + curl/wget to avoid pulling reqwest into this sync path.
    // At runtime the agent loop uses reqwest; here we keep it minimal.
    let index_bytes = fetch_url_blocking(&api_url, None)
        .with_context(|| format!("failed to fetch package index from {api_url}"))?;

    let index: RegistryPackageIndex = serde_json::from_slice(&index_bytes)
        .context("registry returned invalid package index JSON")?;

    // Destination skill directory: `skills/<pkg_name>/`
    let skill_dir_name = pkg_name.to_string();
    let skill_dir = skills_path.join(&skill_dir_name);
    if skill_dir.exists() {
        anyhow::bail!(
            "skill '{}' is already installed at {}; remove it first to reinstall",
            pkg_name,
            skill_dir.display()
        );
    }
    std::fs::create_dir_all(&skill_dir)?;

    // Run the actual work in a closure so we can clean up skill_dir on any error.
    let result = (|| -> Result<usize> {
        let mut files_written = 0usize;

        // Download each tool
        for tool in &index.tools {
            // Validate tool name: must be a single normal path component (no traversal).
            let tool_name_path = std::path::Path::new(&tool.name);
            let is_single_normal = {
                use std::path::Component;
                let mut comps = tool_name_path.components();
                matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
            };
            if !is_single_normal
                || !tool
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                anyhow::bail!("registry returned unsafe tool name: '{}'", tool.name);
            }

            let tool_dir = skill_dir.join("tools").join(&tool.name);
            std::fs::create_dir_all(&tool_dir)?;

            // Validate artifact URLs: must be HTTPS and on an allowed host
            // (registry host or registry-declared artifact CDN host).
            let artifact_base = index.artifact_base_url.as_deref();
            validate_artifact_url(&tool.wasm_url, registry_url, artifact_base)
                .with_context(|| format!("unsafe wasm_url for tool '{}'", tool.name))?;
            validate_artifact_url(&tool.manifest_url, registry_url, artifact_base)
                .with_context(|| format!("unsafe manifest_url for tool '{}'", tool.name))?;

            // Download tool.wasm
            println!("  Downloading tool: {}", tool.name);
            let wasm_bytes = fetch_url_blocking(&tool.wasm_url, None)
                .with_context(|| format!("failed to download WASM for tool '{}'", tool.name))?;
            std::fs::write(tool_dir.join("tool.wasm"), &wasm_bytes)?;
            files_written += 1;

            // Download manifest.json
            let manifest_bytes = fetch_url_blocking(&tool.manifest_url, None)
                .with_context(|| format!("failed to download manifest for tool '{}'", tool.name))?;

            // Validate manifest before writing (ensures it parses as WasmManifest)
            let _manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
                .with_context(|| format!("invalid manifest JSON for tool '{}'", tool.name))?;
            std::fs::write(tool_dir.join("manifest.json"), &manifest_bytes)?;
            files_written += 1;
        }

        // Write minimal SKILL.toml using safe TOML serialization to avoid
        // injection via description strings containing quotes or special chars.
        #[derive(serde::Serialize)]
        struct SkillMeta<'a> {
            name: &'a str,
            description: &'a str,
            version: &'a str,
            author: &'a str,
            tags: &'a [&'a str],
        }
        #[derive(serde::Serialize)]
        struct SkillToml<'a> {
            skill: SkillMeta<'a>,
        }
        let description = index
            .description
            .as_deref()
            .unwrap_or("Installed from ZeroMarket registry");
        let skill_toml_value = SkillToml {
            skill: SkillMeta {
                name: &skill_dir_name,
                description,
                version: &index.version,
                author: namespace,
                tags: &["wasm", "zeromarket"],
            },
        };
        let skill_toml_str =
            toml::to_string(&skill_toml_value).context("failed to serialize SKILL.toml")?;
        std::fs::write(skill_dir.join("SKILL.toml"), skill_toml_str)?;
        files_written += 1;

        Ok(files_written)
    })();

    match result {
        Ok(files_written) => Ok((skill_dir, files_written)),
        Err(e) => {
            // Remove partially-written skill_dir to avoid leaving broken state.
            let _ = std::fs::remove_dir_all(&skill_dir);
            Err(e)
        }
    }
}

/// Minimal JSON shape returned by the ZeroMarket registry package index endpoint.
#[derive(Debug, serde::Deserialize)]
struct RegistryPackageIndex {
    version: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tools: Vec<RegistryToolEntry>,
    /// Optional CDN base URL where WASM artifacts are hosted.
    ///
    /// When present, artifact URLs may use this host instead of (or in addition
    /// to) the registry host.  The declared host must itself be HTTPS; the client
    /// validates that each artifact URL's host matches either the registry host or
    /// this declared base host.  This lets the registry operator store binaries on
    /// a separate CDN (e.g. Cloudflare R2) without client changes.
    #[serde(default)]
    artifact_base_url: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RegistryToolEntry {
    name: String,
    wasm_url: String,
    manifest_url: String,
}

/// Blocking HTTP GET using the system `curl` binary (avoids adding a sync HTTP
/// Extract the hostname from an `https://` URL (the part before the first
/// `'/'`, `'?'`, `'#'`, or `':'` after the scheme).
fn extract_url_host(url: &str) -> &str {
    url.strip_prefix("https://")
        .unwrap_or("")
        .split(&['/', '?', '#', ':'][..])
        .next()
        .unwrap_or("")
}

/// Validate that an artifact URL (wasm_url / manifest_url from the registry index)
/// is HTTPS and served from an allowed host, preventing SSRF via a malicious
/// registry response redirecting downloads to internal hosts.
///
/// Allowed hosts:
/// 1. The registry host itself (e.g. `zeromarket.vercel.app`).
/// 2. The declared `artifact_base_url` host from the package index — lets the
///    registry operator store binaries on a separate CDN (e.g. Cloudflare R2)
///    without hardcoding CDN domains in the client.  The declared base URL must
///    also use HTTPS; otherwise it is ignored.
fn validate_artifact_url(
    artifact_url: &str,
    registry_url: &str,
    artifact_base_url: Option<&str>,
) -> Result<()> {
    if !artifact_url.starts_with("https://") {
        anyhow::bail!("artifact URL must use HTTPS: {artifact_url}");
    }
    let registry_host = extract_url_host(registry_url);
    let artifact_host = extract_url_host(artifact_url);

    if registry_host.is_empty() || artifact_host.is_empty() {
        anyhow::bail!(
            "could not determine host from artifact URL '{}' or registry URL '{}'",
            artifact_url,
            registry_url
        );
    }

    if artifact_host == registry_host {
        return Ok(());
    }

    // Allow host declared by the registry as its artifact CDN, if that
    // declaration is itself a valid HTTPS URL.
    if let Some(base) = artifact_base_url {
        if base.starts_with("https://") {
            let base_host = extract_url_host(base);
            if !base_host.is_empty() && artifact_host == base_host {
                return Ok(());
            }
        }
    }

    // Allow Cloudflare R2 public bucket hostnames (`*.r2.dev`) as a trusted CDN
    // fallback.  R2 is a hosted object-storage service with no access to private
    // networks, so allowing artifacts from any R2 bucket does not create SSRF risk.
    // Registries that store binaries on R2 without declaring `artifact_base_url`
    // still work out of the box.
    if artifact_host.ends_with(".r2.dev") {
        return Ok(());
    }

    anyhow::bail!(
        "artifact host '{}' is not allowed (registry host: '{}'; declared artifact host: '{}')",
        artifact_host,
        registry_host,
        artifact_base_url
            .and_then(|u| {
                if u.starts_with("https://") {
                    Some(extract_url_host(u))
                } else {
                    None
                }
            })
            .unwrap_or("none")
    );
}

// ─── ClawhHub skill installer ────────────────────────────────────────────────
//
// ClawhHub (https://clawhub.ai) is the OpenClaw skill registry.
// Supported source formats:
//   - `https://clawhub.ai/<owner>/<slug>`  (profile URL, auto-detected by domain)
//   - `clawhub:<slug>`                     (short prefix)
//
// The download URL is: https://clawhub.ai/api/v1/download?slug=<slug>
// Zip contents follow the OpenClaw convention: `_meta.json` + `SKILL.md` + scripts.

const CLAWHUB_DOMAIN: &str = "clawhub.ai";
const CLAWHUB_DOWNLOAD_API: &str = "https://clawhub.ai/api/v1/download";

/// Returns true if `source` is a ClawhHub skill reference.
fn is_clawhub_source(source: &str) -> bool {
    if source.starts_with("clawhub:") {
        return true;
    }
    // Auto-detect from domain: https://clawhub.ai/...
    if let Some(rest) = source.strip_prefix("https://") {
        let host = rest.split('/').next().unwrap_or("");
        return host == CLAWHUB_DOMAIN;
    }
    false
}

/// Convert a ClawhHub source string into the zip download URL.
///
/// - `clawhub:gog`                       → `https://clawhub.ai/api/v1/download?slug=gog`
/// - `https://clawhub.ai/steipete/gog`   → `https://clawhub.ai/api/v1/download?slug=steipete/gog`
/// - `https://clawhub.ai/gog`            → `https://clawhub.ai/api/v1/download?slug=gog`
///
/// For profile URLs the full path (owner/slug) is forwarded verbatim as the slug query
/// parameter so the ClawhHub API can resolve owner-namespaced skills correctly.
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
    // Profile URL: https://clawhub.ai/<owner>/<slug>  or  https://clawhub.ai/<slug>
    // Forward the full path as the slug so the API can resolve owner-namespaced skills.
    if let Some(rest) = source.strip_prefix("https://") {
        let path = rest
            .strip_prefix(CLAWHUB_DOMAIN)
            .unwrap_or("")
            .trim_start_matches('/');
        let path = path.trim_end_matches('/');
        if path.is_empty() {
            anyhow::bail!("could not extract slug from ClawhHub URL: {source}");
        }
        // Keep the literal slash so the API receives `slug=owner/name`
        // (some backends do not decode %2F in query parameters).
        return Ok(format!("{CLAWHUB_DOWNLOAD_API}?slug={path}"));
    }
    anyhow::bail!("unrecognised ClawhHub source format: {source}")
}

// ─── Generic zip-URL skill installer ─────────────────────────────────────────
//
// Installs a skill from any HTTPS URL that returns a zip archive.
// Supports two source formats:
//   - `zip:https://example.com/path/to/skill.zip`  (explicit prefix)
//   - `https://example.com/skill.zip`              (`.zip` suffix auto-detection)
//
// No system-level `unzip` binary is required; extraction is done in-process
// using the `zip` crate. This makes the feature portable and dependency-free.
//
// If the zip contains a `_meta.json` at its root (OpenClaw registry convention),
// the name, version, and author fields are read from it. Otherwise the skill
// name is derived from the URL's last path segment.

/// Returns true if `source` should be handled as a zip-URL download.
fn is_zip_url_source(source: &str) -> bool {
    // Explicit `zip:https://...` prefix
    if let Some(rest) = source.strip_prefix("zip:") {
        return rest.starts_with("https://");
    }
    // Direct HTTPS URL ending in `.zip`
    let path_part = source.split('?').next().unwrap_or(source);
    source.starts_with("https://") && path_part.ends_with(".zip")
}

/// Strips the `zip:` prefix if present, returning the bare HTTPS URL.
fn zip_url_from_source(source: &str) -> &str {
    source.strip_prefix("zip:").unwrap_or(source)
}

/// Normalize a raw slug or filename into a valid skill directory name.
/// Lowercases, replaces hyphens with underscores, strips everything else.
fn normalize_skill_name(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c == '-' { '_' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

/// Read skill metadata (name, version, author) from a zip archive.
///
/// Checks for `_meta.json` at the root of the archive first (OpenClaw/ClawhHub
/// convention). Falls back to the URL-derived name passed via `url_hint`.
fn extract_zip_skill_meta(
    bytes: &[u8],
    url_hint: &str,
) -> Result<(String, String, Option<String>)> {
    use std::io::Read as _;

    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).context("downloaded content is not a valid zip archive")?;

    if let Ok(mut f) = archive.by_name("_meta.json") {
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok();
        if let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&buf) {
            let slug_raw = meta.get("slug").and_then(|v| v.as_str()).unwrap_or("");
            let base = slug_raw.split('/').last().unwrap_or(slug_raw);
            let name = normalize_skill_name(base);
            if !name.is_empty() {
                let version = meta
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0.1.0")
                    .to_string();
                let author = meta
                    .get("ownerId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                return Ok((name, version, author));
            }
        }
    }

    // Fallback: derive name from the URL path (strip query string and .zip suffix)
    let url_path = url_hint.split('?').next().unwrap_or(url_hint);
    let last_seg = url_path.rsplit('/').next().unwrap_or("skill");
    let base = last_seg.strip_suffix(".zip").unwrap_or(last_seg);
    let name = normalize_skill_name(base);
    let name = if name.is_empty() {
        "skill".to_string()
    } else {
        name
    };
    Ok((name, "0.1.0".to_string(), None))
}

/// Install a skill from a local `.zip` file (e.g. downloaded manually from ClawhHub).
///
/// Usage: `zeroclaw skill install /path/to/skill.zip`
fn install_local_zip_source(zip_path: &Path, skills_path: &Path) -> Result<(PathBuf, usize)> {
    let bytes = std::fs::read(zip_path)
        .with_context(|| format!("failed to read zip file: {}", zip_path.display()))?;
    let hint = zip_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("skill.zip");
    extract_zip_bytes_to_skills(&bytes, hint, skills_path)
}

/// Download a zip archive from `url` and install it as a skill under `skills_path`.
///
/// `auth_token` is an optional Bearer token added as `Authorization: Bearer <token>`.
/// Extraction is done in-process (no `unzip` binary required).
/// Returns the installed skill directory path and the number of files written.
fn install_zip_url_source(
    url: &str,
    skills_path: &Path,
    auth_token: Option<&str>,
) -> Result<(PathBuf, usize)> {
    let bytes = fetch_url_blocking(url, auth_token)
        .with_context(|| format!("failed to fetch zip from {url}"))?;
    extract_zip_bytes_to_skills(&bytes, url, skills_path)
}

/// Core zip extraction logic shared by local and remote zip installers.
///
/// Runs a full security audit on the zip contents before extracting a single byte.
/// `name_hint` is used as a fallback for skill name detection (URL or filename).
fn extract_zip_bytes_to_skills(
    bytes: &[u8],
    name_hint: &str,
    skills_path: &Path,
) -> Result<(PathBuf, usize)> {
    // ── Security audit BEFORE extraction ────────────────────────────────────
    // Runs zip-specific checks: entry count, path traversal, native binaries,
    // per-file and total decompressed size limits, compression ratio (zip bomb),
    // and high-risk shell pattern detection in text files.
    let audit_report =
        audit::audit_zip_bytes(bytes).context("zip pre-extraction security check failed")?;
    if !audit_report.is_clean() {
        let findings = audit_report
            .findings
            .iter()
            .map(|f| format!("  - {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!(
            "zip skill rejected by security audit ({} finding{}):\n{findings}",
            audit_report.findings.len(),
            if audit_report.findings.len() == 1 {
                ""
            } else {
                "s"
            }
        );
    }

    let (skill_name, skill_version, skill_author) = extract_zip_skill_meta(bytes, name_hint)
        .with_context(|| format!("could not determine skill name from zip: {name_hint}"))?;

    let skill_dir = skills_path.join(&skill_name);
    if skill_dir.exists() {
        anyhow::bail!(
            "skill '{}' already exists at {}; run 'zeroclaw skill remove {}' first",
            skill_name,
            skill_dir.display(),
            skill_name
        );
    }
    std::fs::create_dir_all(&skill_dir)?;

    // Extract zip entries
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).context("failed to re-open zip archive for extraction")?;

    let mut files_written = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().to_string();

        // Security: reject path traversal attempts
        if raw_name.contains("..") || raw_name.starts_with('/') {
            let _ = std::fs::remove_dir_all(&skill_dir);
            anyhow::bail!("zip entry contains unsafe path: {raw_name}");
        }

        let out_path = skill_dir.join(&raw_name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = std::fs::File::create(&out_path)
                .with_context(|| format!("failed to create {}", out_path.display()))?;
            std::io::copy(&mut entry, &mut out_file)?;
            files_written += 1;
        }
    }

    // Write a minimal SKILL.toml so the skill appears in `zeroclaw skill list`
    // (only if neither SKILL.toml nor SKILL.md was included in the zip)
    let toml_path = skill_dir.join("SKILL.toml");
    if !toml_path.exists() && !skill_dir.join("SKILL.md").exists() {
        let author_line = skill_author
            .map(|a| format!("author = \"{a}\"\n"))
            .unwrap_or_default();
        std::fs::write(
            &toml_path,
            format!(
                "[skill]\nname = \"{skill_name}\"\ndescription = \"Zip-installed skill\"\nversion = \"{skill_version}\"\n{author_line}"
            ),
        )?;
        files_written += 1;
    }

    Ok((skill_dir, files_written))
}

/// crate to this sync code path). Falls back to a basic TCP approach is not needed
/// because `curl` is universally available on target platforms.
///
/// `auth_token` — if `Some`, adds `Authorization: Bearer <token>` to the request.
fn fetch_url_blocking(url: &str, auth_token: Option<&str>) -> Result<Vec<u8>> {
    // Validate URL scheme — only https:// allowed to prevent SSRF
    if !url.starts_with("https://") {
        anyhow::bail!("registry URL must use HTTPS: {url}");
    }

    // Use --write-out to append the HTTP status code on a separate line so we
    // can give actionable error messages (e.g. 429 rate-limit guidance) without
    // needing a separate HEAD request.
    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--max-redirs",
        "5",
        "--max-time",
        "30",
        "--write-out",
        "\n%{http_code}",
    ]);
    if let Some(token) = auth_token {
        cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    cmd.arg(url);

    let output = cmd
        .output()
        .context("failed to run 'curl' — ensure curl is installed")?;

    // Parse the HTTP status code appended by --write-out.
    let stdout = output.stdout;
    let (body, http_status) = if let Some(nl) = stdout.iter().rposition(|&b| b == b'\n') {
        let code_bytes = stdout[nl + 1..]
            .iter()
            .copied()
            .take_while(|b| b.is_ascii_digit())
            .collect::<Vec<_>>();
        let status: u16 = String::from_utf8_lossy(&code_bytes).parse().unwrap_or(0);
        (stdout[..nl].to_vec(), status)
    } else {
        (stdout, 0)
    };

    if http_status == 429 {
        anyhow::bail!(
            "ClawhHub rate limit reached (HTTP 429). \
             Wait a moment and retry, or set `clawhub_token` in the `[skills]` section \
             of your config.toml to use authenticated requests."
        );
    }

    if !output.status.success() || (http_status != 0 && http_status >= 400) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if http_status != 0 {
            anyhow::bail!("HTTP {http_status} from {url}: {stderr}");
        }
        anyhow::bail!("curl failed for {url}: {stderr}");
    }

    Ok(body)
}

// ─── Handle command ───────────────────────────────────────────────────────────

/// Handle the `skills` CLI command
#[allow(clippy::too_many_lines)]
pub fn handle_command(command: crate::SkillCommands, config: &crate::config::Config) -> Result<()> {
    let workspace_dir = &config.workspace_dir;
    match command {
        crate::SkillCommands::New { name, template } => {
            let dest = std::env::current_dir().unwrap_or_else(|_| workspace_dir.clone());

            scaffold_skill(&name, &template, &dest)
                .with_context(|| format!("failed to scaffold skill '{name}'"))?;

            // Resolve template again for display (find is cheap; scaffold_skill already
            // validated that the template exists, so this should never be None).
            let tmpl = templates::find(&template).ok_or_else(|| {
                anyhow::anyhow!("template '{}' not found after scaffold", template)
            })?;

            let skill_dir = dest.join(&name);
            println!(
                "  {} Skill '{}' created at {}",
                console::style("✓").green().bold(),
                name,
                skill_dir.display()
            );
            println!(
                "  Template: {} ({})",
                console::style(tmpl.name).cyan(),
                tmpl.language
            );
            println!();
            println!("  Next steps:");
            println!("    cd {name}");
            match tmpl.language {
                "typescript" => {
                    println!("    npm install && npm run build   # → tool.wasm");
                }
                "rust" => {
                    println!(
                        "    {}  # one-time setup",
                        console::style("rustup target add wasm32-wasip1").yellow()
                    );
                    println!("    cargo build --target wasm32-wasip1 --release");
                    println!("    cp target/wasm32-wasip1/release/*.wasm tool.wasm");
                }
                "go" => {
                    println!("    tinygo build -o tool.wasm -target wasi .");
                }
                "python" => {
                    println!("    pip install componentize-py");
                    println!("    componentize-py -d wit/ -w zeroclaw-skill componentize main -o tool.wasm");
                }
                _ => {}
            }
            println!("    zeroclaw skill test . --args '{}'", tmpl.test_args);
            println!();
            println!(
                "  {} 'zeroclaw skill test' requires the {} CLI:",
                console::style("Note:").dim(),
                console::style("wasmtime").cyan()
            );
            println!(
                "    macOS:       {}",
                console::style("brew install wasmtime").yellow()
            );
            println!(
                "    Linux/macOS: {}",
                console::style("curl https://wasmtime.dev/install.sh -sSf | bash").yellow()
            );
            println!();
            println!(
                "  To publish: upload this folder to {}",
                console::style("https://zeromarket.dev/upload").underlined()
            );

            Ok(())
        }

        crate::SkillCommands::Test { path, tool, args } => {
            let skill_path = std::path::Path::new(&path);
            let skill_path = if skill_path.is_absolute() {
                skill_path.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| workspace_dir.clone())
                    .join(skill_path)
            };

            // If `path` is just a skill name, resolve from installed skills dir
            let skill_path = if !skill_path.exists() && !path.contains('/') && !path.contains('\\')
            {
                skills_dir(workspace_dir).join(&path)
            } else {
                skill_path
            };

            if !skill_path.exists() {
                anyhow::bail!(
                    "Skill path not found: {}\n\
                     Tip: run from the skill directory or pass an absolute path.",
                    skill_path.display()
                );
            }

            let args_json = args.as_deref().unwrap_or("{\"input\":\"test\"}");

            test_skill_locally(&skill_path, tool.as_deref(), args_json)
                .with_context(|| format!("skill test failed for {}", skill_path.display()))?;

            Ok(())
        }

        crate::SkillCommands::List => {
            let skills = load_skills_with_config(workspace_dir, config);
            if skills.is_empty() {
                println!("No skills installed.");
                println!();
                println!("  Create one: mkdir -p ~/.zeroclaw/workspace/skills/my-skill");
                println!("              echo '# My Skill' > ~/.zeroclaw/workspace/skills/my-skill/SKILL.md");
                println!();
                println!("  Or install: zeroclaw skills install <source>");
            } else {
                println!("Installed skills ({}):", skills.len());
                println!();
                for skill in &skills {
                    println!(
                        "  {} {} — {}",
                        console::style(&skill.name).white().bold(),
                        console::style(format!("v{}", skill.version)).dim(),
                        skill.description
                    );
                    if !skill.tools.is_empty() {
                        println!(
                            "    Tools: {}",
                            skill
                                .tools
                                .iter()
                                .map(|t| t.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    if !skill.tags.is_empty() {
                        println!("    Tags:  {}", skill.tags.join(", "));
                    }
                }
            }
            println!();
            Ok(())
        }
        crate::SkillCommands::Audit { source } => {
            let source_path = PathBuf::from(&source);
            let target = if source_path.exists() {
                source_path
            } else {
                skills_dir(workspace_dir).join(&source)
            };

            if !target.exists() {
                anyhow::bail!("Skill source or installed skill not found: {source}");
            }

            let report = audit::audit_skill_directory_with_options(
                &target,
                audit::SkillAuditOptions {
                    allow_scripts: config.skills.allow_scripts,
                },
            )?;
            if report.is_clean() {
                println!(
                    "  {} Skill audit passed for {} ({} files scanned).",
                    console::style("✓").green().bold(),
                    target.display(),
                    report.files_scanned
                );
                return Ok(());
            }

            println!(
                "  {} Skill audit failed for {}",
                console::style("✗").red().bold(),
                target.display()
            );
            for finding in report.findings {
                println!("    - {finding}");
            }
            anyhow::bail!("Skill audit failed.");
        }
        crate::SkillCommands::Install { source } => {
            println!("Installing skill from: {source}");

            let skills_path = skills_dir(workspace_dir);
            std::fs::create_dir_all(&skills_path)?;

            if is_clawhub_source(&source) {
                let download_url = clawhub_download_url(&source)
                    .with_context(|| format!("invalid ClawhHub source: {source}"))?;
                let token = config.skills.clawhub_token.as_deref();
                let (installed_dir, files_written) =
                    install_zip_url_source(&download_url, &skills_path, token)
                        .with_context(|| format!("failed to install ClawhHub skill: {source}"))?;
                println!(
                    "  {} ClawhHub skill installed: {} ({} files written)",
                    console::style("✓").green().bold(),
                    installed_dir.display(),
                    files_written
                );
                println!("  Run 'zeroclaw skill list' to verify the new tools are available.");
            } else if is_zip_url_source(&source) {
                // Generic zip-URL install: supports `zip:https://...` prefix and
                // direct `.zip` URLs.  No system `unzip` binary required.
                let url = zip_url_from_source(&source);
                let (installed_dir, files_written) =
                    install_zip_url_source(url, &skills_path, None)
                        .with_context(|| format!("failed to install zip skill from: {url}"))?;
                println!(
                    "  {} Skill installed from zip: {} ({} files written)",
                    console::style("✓").green().bold(),
                    installed_dir.display(),
                    files_written
                );
                println!("  Run 'zeroclaw skill list' to verify the new tools are available.");
            } else if is_git_source(&source) {
                let (installed_dir, files_scanned) =
                    install_git_skill_source(&source, &skills_path, config.skills.allow_scripts)
                        .with_context(|| format!("failed to install git skill source: {source}"))?;
                println!(
                    "  {} Skill installed and audited: {} ({} files scanned)",
                    console::style("✓").green().bold(),
                    installed_dir.display(),
                    files_scanned
                );
                println!("  Security audit completed successfully.");
            } else if is_registry_source(&source) {
                // ZeroMarket (or compatible) registry: `namespace/name[@version]`
                let registry_url = &config.wasm.registry_url;
                let (installed_dir, files_written) =
                    install_registry_skill_source(&source, &skills_path, registry_url)
                        .with_context(|| format!("failed to install registry package: {source}"))?;
                println!(
                    "  {} WASM skill package installed: {} ({} files written)",
                    console::style("✓").green().bold(),
                    installed_dir.display(),
                    files_written
                );
                println!("  Run 'zeroclaw skill list' to verify the new tools are available.");
            } else {
                // Check if source is a local .zip file before falling back to directory install
                let source_path = std::path::Path::new(&source);
                let is_local_zip = source_path
                    .extension()
                    .map_or(false, |e| e.eq_ignore_ascii_case("zip"))
                    && source_path.is_file();

                if is_local_zip {
                    let (dest, files_written) = install_local_zip_source(source_path, &skills_path)
                        .with_context(|| format!("failed to install zip skill from: {source}"))?;
                    println!(
                        "  {} Skill installed from zip: {} ({} files written)",
                        console::style("✓").green().bold(),
                        dest.display(),
                        files_written
                    );
                    println!("  Run 'zeroclaw skill list' to verify the new tools are available.");
                } else {
                    let (dest, files_scanned) = install_local_skill_source(
                        &source,
                        &skills_path,
                        config.skills.allow_scripts,
                    )
                    .with_context(|| format!("failed to install local skill source: {source}"))?;
                    println!(
                        "  {} Skill installed and audited: {} ({} files scanned)",
                        console::style("✓").green().bold(),
                        dest.display(),
                        files_scanned
                    );
                    println!("  Security audit completed successfully.");
                }
            }

            Ok(())
        }
        crate::SkillCommands::Remove { name } => {
            // Reject path traversal attempts
            if name.contains("..") || name.contains('/') || name.contains('\\') {
                anyhow::bail!("Invalid skill name: {name}");
            }

            let skill_path = skills_dir(workspace_dir).join(&name);

            // Verify the resolved path is actually inside the skills directory
            let canonical_skills = skills_dir(workspace_dir)
                .canonicalize()
                .unwrap_or_else(|_| skills_dir(workspace_dir));
            if let Ok(canonical_skill) = skill_path.canonicalize() {
                if !canonical_skill.starts_with(&canonical_skills) {
                    anyhow::bail!("Skill path escapes skills directory: {name}");
                }
            }

            if !skill_path.exists() {
                anyhow::bail!("Skill not found: {name}");
            }

            std::fs::remove_dir_all(&skill_path)?;
            println!(
                "  {} Skill '{}' removed.",
                console::style("✓").green().bold(),
                name
            );
            Ok(())
        }

        crate::SkillCommands::Templates => {
            println!("  Available skill templates:\n");
            println!(
                "  {:<20} {:<12} {}",
                console::style("NAME").bold(),
                console::style("LANGUAGE").bold(),
                console::style("DESCRIPTION").bold(),
            );
            println!("  {}", "─".repeat(72));
            for tmpl in templates::ALL {
                println!(
                    "  {:<20} {:<12} {}",
                    console::style(tmpl.name).cyan(),
                    tmpl.language,
                    tmpl.description,
                );
            }
            println!();
            println!("  Usage:");
            println!("    zeroclaw skill new <name> --template <template-name>");
            println!();
            println!("  Example:");
            println!(
                "    zeroclaw skill new my_weather --template {}",
                console::style("weather_lookup").cyan()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
#[allow(clippy::similar_names)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    fn open_skills_env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn unset(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn load_empty_skills_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills = load_skills(dir.path());
        assert!(skills.is_empty());
    }

    #[test]
    fn load_skill_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("test-skill");
        fs::create_dir_all(&skill_dir).unwrap();

        fs::write(
            skill_dir.join("SKILL.toml"),
            r#"
[skill]
name = "test-skill"
description = "A test skill"
version = "1.0.0"
tags = ["test"]

[[tools]]
name = "hello"
description = "Says hello"
kind = "shell"
command = "echo hello"
"#,
        )
        .unwrap();

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "test-skill");
        assert_eq!(skills[0].tools.len(), 1);
        assert_eq!(skills[0].tools[0].name, "hello");
    }

    #[test]
    fn load_skill_from_md() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("md-skill");
        fs::create_dir_all(&skill_dir).unwrap();

        fs::write(
            skill_dir.join("SKILL.md"),
            "# My Skill\nThis skill does cool things.\n",
        )
        .unwrap();

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "md-skill");
        assert!(skills[0].description.contains("cool things"));
    }

    #[test]
    fn skills_to_prompt_empty() {
        let prompt = skills_to_prompt(&[], Path::new("/tmp"));
        assert!(prompt.is_empty());
    }

    #[test]
    fn skills_to_prompt_with_skills() {
        let skills = vec![Skill {
            name: "test".to_string(),
            description: "A test".to_string(),
            version: "1.0.0".to_string(),
            author: None,
            tags: vec![],
            tools: vec![],
            prompts: vec!["Do the thing.".to_string()],
            location: None,
        }];
        let prompt = skills_to_prompt(&skills, Path::new("/tmp"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>test</name>"));
        assert!(prompt.contains("<instruction>Do the thing.</instruction>"));
    }

    #[test]
    fn skills_to_prompt_compact_mode_omits_instructions_and_tools() {
        let skills = vec![Skill {
            name: "test".to_string(),
            description: "A test".to_string(),
            version: "1.0.0".to_string(),
            author: None,
            tags: vec![],
            tools: vec![SkillTool {
                name: "run".to_string(),
                description: "Run task".to_string(),
                kind: "shell".to_string(),
                command: "echo hi".to_string(),
                args: HashMap::new(),
            }],
            prompts: vec!["Do the thing.".to_string()],
            location: Some(PathBuf::from("/tmp/workspace/skills/test/SKILL.md")),
        }];
        let prompt = skills_to_prompt_with_mode(
            &skills,
            Path::new("/tmp/workspace"),
            crate::config::SkillsPromptInjectionMode::Compact,
        );

        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>test</name>"));
        assert!(prompt.contains("<location>skills/test/SKILL.md</location>"));
        assert!(prompt.contains("loaded on demand"));
        assert!(!prompt.contains("<instructions>"));
        assert!(!prompt.contains("<instruction>Do the thing.</instruction>"));
        assert!(!prompt.contains("<tools>"));
    }

    #[test]
    fn init_skills_creates_readme() {
        let dir = tempfile::tempdir().unwrap();
        init_skills_dir(dir.path()).unwrap();
        assert!(dir.path().join("skills").join("README.md").exists());
    }

    #[test]
    fn init_skills_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        init_skills_dir(dir.path()).unwrap();
        init_skills_dir(dir.path()).unwrap(); // second call should not fail
        assert!(dir.path().join("skills").join("README.md").exists());
    }

    #[test]
    fn load_nonexistent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("nonexistent");
        let skills = load_skills(&fake);
        assert!(skills.is_empty());
    }

    #[test]
    fn load_ignores_files_in_skills_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        // A file, not a directory — should be ignored
        fs::write(skills_dir.join("not-a-skill.txt"), "hello").unwrap();
        let skills = load_skills(dir.path());
        assert!(skills.is_empty());
    }

    #[test]
    fn load_ignores_dir_without_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let empty_skill = skills_dir.join("empty-skill");
        fs::create_dir_all(&empty_skill).unwrap();
        // Directory exists but no SKILL.toml or SKILL.md
        let skills = load_skills(dir.path());
        assert!(skills.is_empty());
    }

    #[test]
    fn load_multiple_skills() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");

        for name in ["alpha", "beta", "gamma"] {
            let skill_dir = skills_dir.join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(
                skill_dir.join("SKILL.md"),
                format!("# {name}\nSkill {name} description.\n"),
            )
            .unwrap();
        }

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 3);
    }

    #[test]
    fn toml_skill_with_multiple_tools() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("multi-tool");
        fs::create_dir_all(&skill_dir).unwrap();

        fs::write(
            skill_dir.join("SKILL.toml"),
            r#"
[skill]
name = "multi-tool"
description = "Has many tools"
version = "2.0.0"
author = "tester"
tags = ["automation", "devops"]

[[tools]]
name = "build"
description = "Build the project"
kind = "shell"
command = "cargo build"

[[tools]]
name = "test"
description = "Run tests"
kind = "shell"
command = "cargo test"

[[tools]]
name = "deploy"
description = "Deploy via HTTP"
kind = "http"
command = "https://api.example.com/deploy"
"#,
        )
        .unwrap();

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 1);
        let s = &skills[0];
        assert_eq!(s.name, "multi-tool");
        assert_eq!(s.version, "2.0.0");
        assert_eq!(s.author.as_deref(), Some("tester"));
        assert_eq!(s.tags, vec!["automation", "devops"]);
        assert_eq!(s.tools.len(), 3);
        assert_eq!(s.tools[0].name, "build");
        assert_eq!(s.tools[1].kind, "shell");
        assert_eq!(s.tools[2].kind, "http");
    }

    #[test]
    fn toml_skill_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("minimal");
        fs::create_dir_all(&skill_dir).unwrap();

        fs::write(
            skill_dir.join("SKILL.toml"),
            r#"
[skill]
name = "minimal"
description = "Bare minimum"
"#,
        )
        .unwrap();

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].version, "0.1.0"); // default version
        assert!(skills[0].author.is_none());
        assert!(skills[0].tags.is_empty());
        assert!(skills[0].tools.is_empty());
    }

    #[test]
    fn toml_skill_invalid_syntax_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("broken");
        fs::create_dir_all(&skill_dir).unwrap();

        fs::write(skill_dir.join("SKILL.toml"), "this is not valid toml {{{{").unwrap();

        let skills = load_skills(dir.path());
        assert!(skills.is_empty()); // broken skill is skipped
    }

    #[test]
    fn md_skill_heading_only() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("heading-only");
        fs::create_dir_all(&skill_dir).unwrap();

        fs::write(skill_dir.join("SKILL.md"), "# Just a Heading\n").unwrap();

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "No description");
    }

    #[test]
    fn skills_to_prompt_includes_tools() {
        let skills = vec![Skill {
            name: "weather".to_string(),
            description: "Get weather".to_string(),
            version: "1.0.0".to_string(),
            author: None,
            tags: vec![],
            tools: vec![SkillTool {
                name: "get_weather".to_string(),
                description: "Fetch forecast".to_string(),
                kind: "shell".to_string(),
                command: "curl wttr.in".to_string(),
                args: HashMap::new(),
            }],
            prompts: vec![],
            location: None,
        }];
        let prompt = skills_to_prompt(&skills, Path::new("/tmp"));
        assert!(prompt.contains("weather"));
        assert!(prompt.contains("<name>get_weather</name>"));
        assert!(prompt.contains("<description>Fetch forecast</description>"));
        assert!(prompt.contains("<kind>shell</kind>"));
    }

    #[test]
    fn skills_to_prompt_escapes_xml_content() {
        let skills = vec![Skill {
            name: "xml<skill>".to_string(),
            description: "A & B".to_string(),
            version: "1.0.0".to_string(),
            author: None,
            tags: vec![],
            tools: vec![],
            prompts: vec!["Use <tool> & check \"quotes\".".to_string()],
            location: None,
        }];

        let prompt = skills_to_prompt(&skills, Path::new("/tmp"));
        assert!(prompt.contains("<name>xml&lt;skill&gt;</name>"));
        assert!(prompt.contains("<description>A &amp; B</description>"));
        assert!(prompt.contains(
            "<instruction>Use &lt;tool&gt; &amp; check &quot;quotes&quot;.</instruction>"
        ));
    }

    #[test]
    fn git_source_detection_accepts_remote_protocols_and_scp_style() {
        let sources = [
            "https://github.com/some-org/some-skill.git",
            "http://github.com/some-org/some-skill.git",
            "ssh://git@github.com/some-org/some-skill.git",
            "git://github.com/some-org/some-skill.git",
            "git@github.com:some-org/some-skill.git",
            "git@localhost:skills/some-skill.git",
        ];

        for source in sources {
            assert!(
                is_git_source(source),
                "expected git source detection for '{source}'"
            );
        }
    }

    #[test]
    fn git_source_detection_rejects_local_paths_and_invalid_inputs() {
        let sources = [
            "./skills/local-skill",
            "/tmp/skills/local-skill",
            "C:\\skills\\local-skill",
            "git@github.com",
            "ssh://",
            "not-a-url",
            "dir/git@github.com:org/repo.git",
        ];

        for source in sources {
            assert!(
                !is_git_source(source),
                "expected local/invalid source detection for '{source}'"
            );
        }
    }

    #[test]
    fn skills_dir_path() {
        let base = std::path::Path::new("/home/user/.zeroclaw");
        let dir = skills_dir(base);
        assert_eq!(dir, PathBuf::from("/home/user/.zeroclaw/skills"));
    }

    #[test]
    fn toml_prefers_over_md() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("dual");
        fs::create_dir_all(&skill_dir).unwrap();

        fs::write(
            skill_dir.join("SKILL.toml"),
            "[skill]\nname = \"from-toml\"\ndescription = \"TOML wins\"\n",
        )
        .unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# From MD\nMD description\n").unwrap();

        let skills = load_skills(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "from-toml"); // TOML takes priority
    }

    #[test]
    fn open_skills_enabled_resolution_prefers_env_then_config_then_default_false() {
        assert!(!open_skills_enabled_from_sources(None, None));
        assert!(open_skills_enabled_from_sources(Some(true), None));
        assert!(!open_skills_enabled_from_sources(Some(true), Some("0")));
        assert!(open_skills_enabled_from_sources(Some(false), Some("yes")));
        // Invalid env values should fall back to config.
        assert!(open_skills_enabled_from_sources(
            Some(true),
            Some("invalid")
        ));
        assert!(!open_skills_enabled_from_sources(
            Some(false),
            Some("invalid")
        ));
    }

    #[test]
    fn resolve_open_skills_dir_resolution_prefers_env_then_config_then_home() {
        let home = Path::new("/tmp/home-dir");
        assert_eq!(
            resolve_open_skills_dir_from_sources(
                Some("/tmp/env-skills"),
                Some("/tmp/config"),
                Some(home)
            ),
            Some(PathBuf::from("/tmp/env-skills"))
        );
        assert_eq!(
            resolve_open_skills_dir_from_sources(
                Some("   "),
                Some("/tmp/config-skills"),
                Some(home)
            ),
            Some(PathBuf::from("/tmp/config-skills"))
        );
        assert_eq!(
            resolve_open_skills_dir_from_sources(None, None, Some(home)),
            Some(PathBuf::from("/tmp/home-dir/open-skills"))
        );
        assert_eq!(resolve_open_skills_dir_from_sources(None, None, None), None);
    }

    #[test]
    fn load_skills_with_config_reads_open_skills_dir_without_network() {
        let _env_guard = open_skills_env_lock().lock().unwrap();
        let _enabled_guard = EnvVarGuard::unset("ZEROCLAW_OPEN_SKILLS_ENABLED");
        let _dir_guard = EnvVarGuard::unset("ZEROCLAW_OPEN_SKILLS_DIR");

        let dir = tempfile::tempdir().unwrap();
        let workspace_dir = dir.path().join("workspace");
        fs::create_dir_all(workspace_dir.join("skills")).unwrap();

        let open_skills_dir = dir.path().join("open-skills-local");
        fs::create_dir_all(open_skills_dir.join("skills/http_request")).unwrap();
        fs::write(open_skills_dir.join("README.md"), "# open skills\n").unwrap();
        fs::write(
            open_skills_dir.join("CONTRIBUTING.md"),
            "# contribution guide\n",
        )
        .unwrap();
        fs::write(
            open_skills_dir.join("skills/http_request/SKILL.md"),
            "# HTTP request\nFetch API responses.\n",
        )
        .unwrap();

        let mut config = crate::config::Config::default();
        config.workspace_dir = workspace_dir.clone();
        config.skills.open_skills_enabled = true;
        config.skills.open_skills_dir = Some(open_skills_dir.to_string_lossy().to_string());

        let skills = load_skills_with_config(&workspace_dir, &config);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "http_request");
        assert_ne!(skills[0].name, "CONTRIBUTING");
    }

    // ── is_registry_source ────────────────────────────────────────────────────

    // ── registry install: directory naming ───────────────────────────────────

    /// The installed skill directory must be named after the package name only,
    /// not prefixed with the namespace. This keeps skill directories short and
    /// predictable regardless of who published the package.
    #[test]
    fn registry_install_dir_name_is_package_name_only() {
        // Simulate the naming logic from install_registry_skill_source.
        for (source, expected_dir) in [
            ("zeroclaw-org/weather-lookup", "weather-lookup"),
            ("zeroclaw-org/calculator", "calculator"),
            ("zeroclaw-user/my_tool", "my_tool"),
        ] {
            let parts: Vec<&str> = source.splitn(3, '/').collect();
            let pkg_name = parts[1];
            // strip optional @version suffix
            let pkg_name = pkg_name.split('@').next().unwrap_or(pkg_name);
            let skill_dir_name = pkg_name.to_string();
            assert_eq!(
                skill_dir_name, expected_dir,
                "registry source '{source}' should install to dir '{expected_dir}', got '{skill_dir_name}'"
            );
        }
    }

    #[test]
    fn is_registry_source_accepts_valid_namespace_name() {
        assert!(is_registry_source("zeroclaw/weather-lookup"));
        assert!(is_registry_source("community/my_tool"));
        assert!(is_registry_source("org-name/tool_name"));
        assert!(is_registry_source("ns/name@1.0.0")); // version suffix
    }

    #[test]
    fn is_registry_source_rejects_local_path_prefixes() {
        assert!(!is_registry_source("./weather_lookup"));
        assert!(!is_registry_source("../parent/skill"));
        assert!(!is_registry_source("/absolute/path/skill"));
        assert!(!is_registry_source("~/home/skill"));
    }

    #[test]
    fn is_registry_source_rejects_git_urls_and_http_schemes() {
        assert!(!is_registry_source("https://github.com/org/skill"));
        assert!(!is_registry_source("http://example.com/skill"));
        assert!(!is_registry_source("git@github.com:org/skill.git"));
        assert!(!is_registry_source("ssh://git@github.com/org/skill"));
    }

    #[test]
    fn is_registry_source_rejects_invalid_formats() {
        assert!(!is_registry_source("just-a-name")); // no slash
        assert!(!is_registry_source("a/b/c")); // too many slashes
        assert!(!is_registry_source("ns/..")); // path traversal in name
        assert!(!is_registry_source("../ns/name")); // path traversal prefix
        assert!(!is_registry_source("")); // empty
        assert!(!is_registry_source("/")); // empty segments
    }

    // ── scaffold_skill: validation ────────────────────────────────────────────

    #[test]
    fn scaffold_skill_rejects_traversal_in_name() {
        let dir = tempfile::tempdir().unwrap();
        let result = scaffold_skill("../escape", "typescript", dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Invalid skill name"), "unexpected: {msg}");
    }

    #[test]
    fn scaffold_skill_rejects_slash_in_name() {
        let dir = tempfile::tempdir().unwrap();
        let result = scaffold_skill("ns/name", "typescript", dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn scaffold_skill_rejects_space_in_name() {
        let dir = tempfile::tempdir().unwrap();
        let result = scaffold_skill("my tool", "typescript", dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn scaffold_skill_rejects_empty_name() {
        let dir = tempfile::tempdir().unwrap();
        let result = scaffold_skill("", "typescript", dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn scaffold_skill_rejects_unknown_template() {
        let dir = tempfile::tempdir().unwrap();
        let result = scaffold_skill("zeroclaw_test_tool", "cobol", dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Unknown template"), "unexpected: {msg}");
    }

    #[test]
    fn scaffold_skill_rejects_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("zeroclaw_test_tool")).unwrap();
        let result = scaffold_skill("zeroclaw_test_tool", "typescript", dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("already exists"), "unexpected: {msg}");
    }

    // ── scaffold_skill: output correctness ───────────────────────────────────

    #[test]
    fn scaffold_skill_typescript_creates_required_files() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_skill("zeroclaw_test_ts", "typescript", dir.path()).unwrap();
        let skill_dir = dir.path().join("zeroclaw_test_ts");
        assert!(skill_dir.join("SKILL.md").exists(), "SKILL.md missing");
        assert!(skill_dir.join("README.md").exists(), "README.md missing");
        assert!(skill_dir.join(".gitignore").exists(), ".gitignore missing");
        assert!(
            skill_dir.join("manifest.json").exists(),
            "manifest.json missing"
        );
    }

    #[test]
    fn scaffold_skill_rust_creates_required_files() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_skill("zeroclaw_test_rs", "rust", dir.path()).unwrap();
        let skill_dir = dir.path().join("zeroclaw_test_rs");
        assert!(skill_dir.join("Cargo.toml").exists(), "Cargo.toml missing");
        assert!(
            skill_dir.join("src").join("main.rs").exists(),
            "src/main.rs missing"
        );
        assert!(skill_dir.join("SKILL.md").exists(), "SKILL.md missing");
    }

    #[test]
    fn scaffold_skill_substitutes_name_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_skill("zeroclaw_subst_test", "rust", dir.path()).unwrap();
        let cargo_toml =
            fs::read_to_string(dir.path().join("zeroclaw_subst_test").join("Cargo.toml")).unwrap();
        assert!(
            cargo_toml.contains("zeroclaw_subst_test"),
            "Cargo.toml should contain skill name, got:\n{cargo_toml}"
        );
        assert!(
            !cargo_toml.contains("__SKILL_NAME__"),
            "__SKILL_NAME__ placeholder was not substituted"
        );
    }

    #[test]
    fn scaffold_skill_go_creates_required_files() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_skill("zeroclaw_test_go", "go", dir.path()).unwrap();
        let skill_dir = dir.path().join("zeroclaw_test_go");
        assert!(
            skill_dir.join("manifest.json").exists(),
            "manifest.json missing"
        );
        assert!(skill_dir.join("SKILL.md").exists(), "SKILL.md missing");
    }

    #[test]
    fn scaffold_skill_gitignore_always_created() {
        for template in ["rust", "typescript", "go", "python"] {
            let dir = tempfile::tempdir().unwrap();
            let name = format!("zeroclaw_test_{template}");
            scaffold_skill(&name, template, dir.path()).unwrap();
            assert!(
                dir.path().join(&name).join(".gitignore").exists(),
                ".gitignore missing for template {template}"
            );
        }
    }

    #[test]
    fn scaffold_skill_skill_md_contains_name() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_skill("zeroclaw_md_check", "typescript", dir.path()).unwrap();
        let skill_md =
            fs::read_to_string(dir.path().join("zeroclaw_md_check").join("SKILL.md")).unwrap();
        assert!(
            skill_md.contains("zeroclaw_md_check"),
            "SKILL.md should reference skill name, got:\n{skill_md}"
        );
    }

    // ── ClawhHub source detection and URL building ────────────────────────────

    #[test]
    fn is_clawhub_source_accepts_profile_url() {
        assert!(is_clawhub_source("https://clawhub.ai/steipete/gog"));
        assert!(is_clawhub_source("https://clawhub.ai/gog"));
        assert!(is_clawhub_source("https://clawhub.ai/user/my-skill"));
    }

    #[test]
    fn is_clawhub_source_accepts_short_prefix() {
        assert!(is_clawhub_source("clawhub:gog"));
        assert!(is_clawhub_source("clawhub:weather-lookup"));
    }

    #[test]
    fn is_clawhub_source_rejects_other_domains() {
        assert!(!is_clawhub_source("https://github.com/org/skill"));
        assert!(!is_clawhub_source("https://example.com/skill.zip"));
        assert!(!is_clawhub_source("zeroclaw/skill"));
    }

    #[test]
    fn clawhub_download_url_from_profile_url() {
        // Owner-namespaced URL: full path forwarded as slug with literal slash
        let url = clawhub_download_url("https://clawhub.ai/steipete/gog").unwrap();
        assert_eq!(url, "https://clawhub.ai/api/v1/download?slug=steipete/gog");
    }

    #[test]
    fn clawhub_download_url_from_single_path_url() {
        // Single-segment URL: path is just the skill name
        let url = clawhub_download_url("https://clawhub.ai/gog").unwrap();
        assert_eq!(url, "https://clawhub.ai/api/v1/download?slug=gog");
    }

    #[test]
    fn clawhub_download_url_from_short_prefix() {
        let url = clawhub_download_url("clawhub:gog").unwrap();
        assert_eq!(url, "https://clawhub.ai/api/v1/download?slug=gog");
    }

    #[test]
    fn clawhub_download_url_rejects_slash_in_prefix_slug() {
        assert!(clawhub_download_url("clawhub:owner/gog").is_err());
    }

    // ── is_zip_url_source ─────────────────────────────────────────────────────

    #[test]
    fn is_zip_url_source_accepts_explicit_prefix() {
        assert!(is_zip_url_source("zip:https://example.com/skill.zip"));
        assert!(is_zip_url_source(
            "zip:https://example.com/api/download?slug=my-skill"
        ));
    }

    #[test]
    fn is_zip_url_source_accepts_direct_zip_url() {
        assert!(is_zip_url_source("https://example.com/skill.zip"));
        assert!(is_zip_url_source(
            "https://releases.example.com/my-skill-1.0.0.zip"
        ));
    }

    #[test]
    fn is_zip_url_source_rejects_non_zip_https() {
        // Plain HTTPS URL without .zip suffix and without zip: prefix
        assert!(!is_zip_url_source("https://github.com/org/skill"));
        assert!(!is_zip_url_source(
            "https://example.com/api/download?slug=foo"
        ));
    }

    #[test]
    fn is_zip_url_source_rejects_non_https() {
        assert!(!is_zip_url_source("zip:http://example.com/skill.zip"));
        assert!(!is_zip_url_source("http://example.com/skill.zip"));
        assert!(!is_zip_url_source("ftp://example.com/skill.zip"));
    }

    #[test]
    fn is_zip_url_source_rejects_other_formats() {
        assert!(!is_zip_url_source("zeroclaw/skill"));
        assert!(!is_zip_url_source("./local/skill.zip"));
        assert!(!is_zip_url_source("/absolute/path/skill.zip"));
    }

    // ── normalize_skill_name ──────────────────────────────────────────────────

    #[test]
    fn normalize_skill_name_converts_hyphens_and_lowercases() {
        assert_eq!(normalize_skill_name("My-Skill"), "my_skill");
        assert_eq!(normalize_skill_name("weather-lookup"), "weather_lookup");
        assert_eq!(normalize_skill_name("GOG"), "gog");
    }

    #[test]
    fn normalize_skill_name_strips_non_alnum() {
        assert_eq!(normalize_skill_name("skill.v1"), "skillv1");
        assert_eq!(normalize_skill_name("skill@1.0.0"), "skill100");
    }
}

#[cfg(test)]
mod symlink_tests;
