//! Skill management tools for the background review fork.
//!
//! Three Tool impls exposed to the forked review agent:
//! - `skills_list`: enumerate installed skills (name, description, version).
//! - `skill_view`: read a single skill's SKILL.md (YAML front-matter + body
//!   preview) plus the names of files in `references/`, `templates/`,
//!   `scripts/`.
//! - `skill_manage`: mutating actions — `patch` (atomically rewrite the
//!   SKILL.md YAML front-matter via SkillImprover), `write_file` (add a file
//!   under `references/|templates/|scripts/`), `archive` (move to `.archive/`).
//!
//! Format follows the agentskills.io / Anthropic Agent Skills standard:
//! single `SKILL.md` per skill, YAML front-matter at top, Markdown body below.
//! These tools are NOT registered in the default tool registry — the review
//! fork builds them on demand so the main agent can't accidentally invoke
//! them.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use zeroclaw_api::tool::{Tool, ToolResult};

const ARCHIVE_DIRNAME: &str = ".archive";
const ALLOWED_FILE_PREFIXES: &[&str] = &["references/", "templates/", "scripts/"];
const MAX_FILE_BYTES: usize = 256 * 1024;
const BODY_PREVIEW_CHARS: usize = 2_000;

fn skills_root(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("skills")
}

fn resolve_skill_dir(workspace_dir: &Path, slug: &str) -> Result<PathBuf> {
    if slug.is_empty()
        || slug.contains("..")
        || slug.contains('/')
        || slug.contains('\\')
        || slug.starts_with('.')
    {
        bail!("Invalid skill slug: {slug}");
    }
    Ok(skills_root(workspace_dir).join(slug))
}

/// Resolve `workspace/skills/<slug>` and verify the canonical resolved path is
/// a non-symlinked directory inside the canonical skills root.
///
/// This is the OS-level boundary check that prevents a symlinked
/// `workspace/skills/<slug>` from redirecting mutating operations outside the
/// intended skills tree. The audit module already rejects symlinks *within* a
/// skill at load time; this helper rejects them at the *root* before mutation.
///
/// Returns `(canonical_skills_root, canonical_skill_dir)`.
fn safe_skill_dir(workspace_dir: &Path, slug: &str) -> Result<(PathBuf, PathBuf)> {
    let skill_dir = resolve_skill_dir(workspace_dir, slug)?;
    if !skill_dir.exists() {
        bail!("Skill '{slug}' not found");
    }
    // Reject the slug directory itself if it is a symlink. Without this check,
    // `canonicalize` below would resolve through the symlink and the
    // `starts_with` check would still pass (both sides resolve to the target).
    if skill_dir
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        bail!("Skill '{slug}' directory is a symlink — refusing");
    }
    let skills_root_path = skills_root(workspace_dir);
    let canonical_skills_root = skills_root_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", skills_root_path.display()))?;
    let canonical_skill_dir = skill_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize skill '{slug}'"))?;
    if !canonical_skill_dir.starts_with(&canonical_skills_root) {
        bail!("Skill '{slug}' escapes canonical skills root");
    }
    Ok((canonical_skills_root, canonical_skill_dir))
}

/// Read-only: list installed skills.
pub struct SkillsListTool {
    workspace_dir: PathBuf,
}

impl SkillsListTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for SkillsListTool {
    fn name(&self) -> &str {
        "skills_list"
    }

    fn description(&self) -> &str {
        "List installed skills with their name, version, and one-line description. \
         Read-only. Use before `skill_view` or `skill_manage` to find candidate \
         slugs."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    async fn execute(&self, _args: Value) -> Result<ToolResult> {
        let root = skills_root(&self.workspace_dir);
        let entries = match list_skill_entries(&root).await {
            Ok(e) => e,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read skills directory: {e}")),
                });
            }
        };

        if entries.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "0 installed skills.".to_string(),
                error: None,
            });
        }

        let mut out = format!("{} installed skills:\n\n", entries.len());
        for (slug, name, description, version) in entries {
            let display_name = if name.is_empty() { &slug } else { &name };
            out.push_str(&format!("- {display_name} v{version} ({slug})\n"));
            if !description.is_empty() {
                out.push_str(&format!("    {description}\n"));
            }
        }
        Ok(ToolResult {
            success: true,
            output: out,
            error: None,
        })
    }
}

/// Reads SKILL.md front-matter via the same lightweight parser the loader uses
/// (top-level `key: value` pairs only — no nested mappings).
async fn list_skill_entries(
    skills_dir: &Path,
) -> std::io::Result<Vec<(String, String, String, String)>> {
    let mut rd = match tokio::fs::read_dir(skills_dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut out = Vec::new();
    while let Some(entry) = rd.next_entry().await? {
        let slug = entry.file_name().to_string_lossy().into_owned();
        if slug.starts_with('.') {
            continue;
        }
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let md_path = entry.path().join("SKILL.md");
        let Ok(content) = tokio::fs::read_to_string(&md_path).await else {
            continue;
        };
        let Some((front, _)) = split_front_matter(&content) else {
            continue;
        };
        let name = front_value(&front, "name").unwrap_or_default();
        let description = front_value(&front, "description").unwrap_or_default();
        let version = front_value(&front, "version").unwrap_or_else(|| "0.0.0".to_string());
        out.push((slug, name, description, version));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Read-only: view a single skill's SKILL.md front-matter + body preview + support files.
pub struct SkillViewTool {
    workspace_dir: PathBuf,
}

impl SkillViewTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for SkillViewTool {
    fn name(&self) -> &str {
        "skill_view"
    }

    fn description(&self) -> &str {
        "Read a single skill's SKILL.md content (YAML front-matter + body \
         preview) plus the names of its support files under references/, \
         templates/, scripts/. Use this before deciding whether to patch the \
         skill or add a support file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "Skill slug (directory name under workspace/skills/)."
                }
            },
            "required": ["slug"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("missing `slug` argument"))?;

        let canonical_skill_dir = match safe_skill_dir(&self.workspace_dir, slug) {
            Ok((_, dir)) => dir,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        let md_path = canonical_skill_dir.join("SKILL.md");
        let md = match tokio::fs::read_to_string(&md_path).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Skill '{slug}' not found: {e}")),
                });
            }
        };

        let (front, body) = split_front_matter(&md).unwrap_or((String::new(), md.clone()));
        let support_files = collect_support_files(&canonical_skill_dir).await;

        let mut output = format!("# Skill '{slug}'\n\n## Front-matter\n\n```yaml\n{front}\n```\n");
        if !body.trim().is_empty() {
            let truncated = if body.len() > BODY_PREVIEW_CHARS {
                let end = body.floor_char_boundary(BODY_PREVIEW_CHARS);
                format!(
                    "{}…\n[truncated; full body is {} bytes]",
                    &body[..end],
                    body.len()
                )
            } else {
                body
            };
            output.push_str(&format!("\n## Body (Markdown)\n\n{truncated}\n"));
        }
        if !support_files.is_empty() {
            output.push_str("\n## Support files\n");
            for path in &support_files {
                output.push_str(&format!("- {path}\n"));
            }
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

async fn collect_support_files(skill_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for sub in ["references", "templates", "scripts"] {
        let dir = skill_dir.join(sub);
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            out.push(format!("{sub}/{name}"));
        }
    }
    out.sort();
    out
}

/// Mutating: patch a SKILL.md, write a support file, or archive a skill.
pub struct SkillManageTool {
    workspace_dir: PathBuf,
    config: zeroclaw_config::schema::SkillImprovementConfig,
    /// Mirrors `config.skills.allow_scripts` so post-mutation audit applies
    /// the same script policy that the loader/installer enforces.
    allow_scripts: bool,
}

impl SkillManageTool {
    pub fn new(
        workspace_dir: PathBuf,
        config: zeroclaw_config::schema::SkillImprovementConfig,
        allow_scripts: bool,
    ) -> Self {
        Self {
            workspace_dir,
            config,
            allow_scripts,
        }
    }
}

#[async_trait]
impl Tool for SkillManageTool {
    fn name(&self) -> &str {
        "skill_manage"
    }

    fn description(&self) -> &str {
        "Mutating operations on installed skills. Actions: `patch` (atomically \
         rewrite SKILL.md — supply the full new file content; the YAML \
         front-matter must have a `name` field), `write_file` (add a file \
         under references/, templates/, or scripts/), `archive` (move to \
         .archive/). All writes go through atomic temp-rename and validation \
         where applicable."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["patch", "write_file", "archive"],
                    "description": "Which mutation to perform."
                },
                "slug": {
                    "type": "string",
                    "description": "Skill slug to operate on."
                },
                "content": {
                    "type": "string",
                    "description": "For `patch`: new SKILL.md body (YAML front-matter + Markdown). For `write_file`: file contents."
                },
                "file_path": {
                    "type": "string",
                    "description": "For `write_file` only: relative path starting with `references/`, `templates/`, or `scripts/`."
                },
                "reason": {
                    "type": "string",
                    "description": "Short human-readable reason recorded in the skill's audit trail."
                }
            },
            "required": ["action", "slug"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("missing `action` argument"))?;
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("missing `slug` argument"))?;

        match action {
            "patch" => self.patch(slug, &args).await,
            "write_file" => self.write_file(slug, &args).await,
            "archive" => self.archive(slug).await,
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid: patch, write_file, archive"
                )),
            }),
        }
    }
}

impl SkillManageTool {
    /// Run the install/load audit on a skill directory and roll back on
    /// failure. Returns `Ok(())` on clean audit and an error message string
    /// on failure (the caller decides how to surface it).
    ///
    /// `pre_snapshot` is the original SKILL.md content captured before the
    /// mutation; if `Some`, audit failure restores it. For non-`patch` callers,
    /// the `_unused` parameter exists so this helper has one signature.
    async fn post_mutation_audit(
        &self,
        slug: &str,
        canonical_skill_dir: &Path,
        md_path: &Path,
        pre_snapshot: Option<String>,
        _unused: Option<()>,
    ) -> std::result::Result<(), String> {
        // Lightweight YAML-front-matter validation first (cheap, narrow). The
        // full audit below catches the broader issues (symlinks introduced into
        // the dir, oversized files, script files when scripts are disabled).
        if let Ok(written) = tokio::fs::read_to_string(md_path).await
            && let Err(e) = crate::skills::improver::validate_skill_content(&written)
        {
            restore_snapshot(md_path, pre_snapshot.as_deref()).await;
            return Err(format!(
                "Patch wrote but front-matter is invalid (rolled back): {e}"
            ));
        }

        let report = match crate::skills::audit::audit_skill_directory_with_options(
            canonical_skill_dir,
            crate::skills::audit::SkillAuditOptions {
                allow_scripts: self.allow_scripts,
            },
        ) {
            Ok(r) => r,
            Err(e) => {
                restore_snapshot(md_path, pre_snapshot.as_deref()).await;
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"slug": slug, "error": format!("{e}")})),
                    "Post-patch audit errored"
                );
                return Err(format!("Patch wrote but audit errored (rolled back): {e}"));
            }
        };
        if !report.is_clean() {
            restore_snapshot(md_path, pre_snapshot.as_deref()).await;
            return Err(format!(
                "Patch wrote but skill failed audit (rolled back): {}",
                report.summary()
            ));
        }
        Ok(())
    }

    async fn patch(&self, slug: &str, args: &Value) -> Result<ToolResult> {
        let canonical_skill_dir = match safe_skill_dir(&self.workspace_dir, slug) {
            Ok((_, dir)) => dir,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };
        let md_path = canonical_skill_dir.join("SKILL.md");
        if !md_path.exists() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Skill '{slug}' not found (no SKILL.md)")),
            });
        }
        // Reject symlinks — the patch target must be a regular file.
        if md_path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "SKILL.md for '{slug}' is a symlink — refusing patch"
                )),
            });
        }
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("`patch` requires `content`"))?;
        if content.len() > MAX_FILE_BYTES {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "patch content exceeds {MAX_FILE_BYTES} bytes ({} given)",
                    content.len()
                )),
            });
        }
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("Skill review");

        // Check the kill switch before the cooldown so the agent gets a
        // distinct, actionable error when improvement is disabled — otherwise
        // both reasons collapse onto the cooldown message via
        // `should_improve_skill`, and the agent wastes turns waiting for a
        // cooldown that the disabled flag will never clear.
        if !self.config.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Skill improvement is disabled (enabled: false)".to_string()),
            });
        }

        let mut improver = crate::skills::improver::SkillImprover::new(
            self.workspace_dir.clone(),
            self.config.clone(),
        );
        if !improver.should_improve_skill(slug) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Skill '{slug}' is on cooldown — try again later")),
            });
        }

        // Snapshot the original SKILL.md so we can roll back if the
        // post-mutation install/load audit rejects the resulting skill tree.
        let pre_snapshot = tokio::fs::read_to_string(&md_path).await.ok();

        match improver.improve_skill(slug, content, reason).await {
            Ok(_) => {
                // Run the same audit the loader/installer enforces. Mutation
                // success must mean the resulting skill tree would still pass
                // install/load audit under the active `allow_scripts` policy.
                if let Err(err) = self
                    .post_mutation_audit(slug, &canonical_skill_dir, &md_path, pre_snapshot, None)
                    .await
                {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(err),
                    });
                }
                Ok(ToolResult {
                    success: true,
                    output: format!("Patched skill '{slug}'."),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Patch failed: {e}")),
            }),
        }
    }

    async fn write_file(&self, slug: &str, args: &Value) -> Result<ToolResult> {
        let canonical_skill_dir = match safe_skill_dir(&self.workspace_dir, slug) {
            Ok((_, dir)) => dir,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("`write_file` requires `file_path`"))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("`write_file` requires `content`"))?;

        if !ALLOWED_FILE_PREFIXES
            .iter()
            .any(|prefix| file_path.starts_with(prefix))
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "file_path must start with one of: {}",
                    ALLOWED_FILE_PREFIXES.join(", ")
                )),
            });
        }
        if file_path.contains("..") || file_path.contains('\0') {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("file_path contains forbidden segment".to_string()),
            });
        }
        if content.len() > MAX_FILE_BYTES {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "content exceeds {MAX_FILE_BYTES} bytes ({} given)",
                    content.len()
                )),
            });
        }

        let target = canonical_skill_dir.join(file_path);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Verify target parent stays under canonical skill dir. The skill dir
        // itself is already canonical (and non-symlinked) from `safe_skill_dir`.
        let canonical_target_parent = target
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .unwrap_or_else(|| canonical_skill_dir.clone());
        if !canonical_target_parent.starts_with(&canonical_skill_dir) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("file_path escapes skill directory".to_string()),
            });
        }

        // Reject symlinks — writes must land on a regular file, not follow a
        // symlink to an arbitrary location.
        if target.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("target path is a symlink — refusing write".to_string()),
            });
        }

        // Snapshot for rollback: capture pre-existing content if file existed,
        // otherwise remember that it didn't so we can delete on audit failure.
        let pre_snapshot = if target.exists() {
            tokio::fs::read(&target).await.ok()
        } else {
            None
        };
        let target_existed = target.exists();

        tokio::fs::write(&target, content.as_bytes()).await?;

        // Post-mutation install/load audit under the active script policy.
        let report = match crate::skills::audit::audit_skill_directory_with_options(
            &canonical_skill_dir,
            crate::skills::audit::SkillAuditOptions {
                allow_scripts: self.allow_scripts,
            },
        ) {
            Ok(r) => r,
            Err(e) => {
                rollback_write(&target, pre_snapshot.as_deref(), target_existed).await;
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Post-write audit errored: {e}")),
                });
            }
        };
        if !report.is_clean() {
            rollback_write(&target, pre_snapshot.as_deref(), target_existed).await;
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Wrote {file_path} but skill failed audit (rolled back): {}",
                    report.summary()
                )),
            });
        }
        Ok(ToolResult {
            success: true,
            output: format!("Wrote {file_path} for skill '{slug}'."),
            error: None,
        })
    }

    async fn archive(&self, slug: &str) -> Result<ToolResult> {
        let (canonical_skills_root, canonical_skill_dir) =
            match safe_skill_dir(&self.workspace_dir, slug) {
                Ok(pair) => pair,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    });
                }
            };
        // Anchor the archive dir under the canonical skills root. Create then
        // canonicalize to defend against `.archive` being introduced as a
        // symlink after `safe_skill_dir` resolved the slug dir.
        let archive_dir_path = canonical_skills_root.join(ARCHIVE_DIRNAME);
        tokio::fs::create_dir_all(&archive_dir_path).await?;
        if archive_dir_path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Archive directory {ARCHIVE_DIRNAME} is a symlink — refusing archive"
                )),
            });
        }
        let canonical_archive_dir = archive_dir_path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", archive_dir_path.display()))?;
        if !canonical_archive_dir.starts_with(&canonical_skills_root) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("archive directory escapes canonical skills root".to_string()),
            });
        }
        let target = canonical_archive_dir.join(slug);
        let final_target = if target.exists() {
            let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
            canonical_archive_dir.join(format!("{slug}-{stamp}"))
        } else {
            target
        };
        // Belt and suspenders: `final_target`'s parent must be the canonical
        // archive dir, not somewhere else due to a weird slug-name path quirk.
        if final_target.parent() != Some(canonical_archive_dir.as_path()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("archive target escapes archive directory".to_string()),
            });
        }
        tokio::fs::rename(&canonical_skill_dir, &final_target).await?;
        Ok(ToolResult {
            success: true,
            output: format!("Archived skill '{slug}' to {}", final_target.display()),
            error: None,
        })
    }
}

// ─── Rollback helpers (used by `patch` and `write_file` audit failure paths) ─

async fn restore_snapshot(target: &Path, snapshot: Option<&str>) {
    if let Some(s) = snapshot
        && let Err(e) = tokio::fs::write(target, s).await
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(
                    ::serde_json::json!({"target": target.display().to_string(), "error": format!("{e}")})
                ),
            "Failed to restore snapshot after audit rollback"
        );
    }
}

async fn rollback_write(target: &Path, snapshot: Option<&[u8]>, target_existed: bool) {
    if target_existed {
        if let Some(bytes) = snapshot
            && let Err(e) = tokio::fs::write(target, bytes).await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"target": target.display().to_string(), "error": format!("{e}")})
                    ),
                "Failed to restore file after audit rollback"
            );
        }
    } else if let Err(e) = tokio::fs::remove_file(target).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(
                    ::serde_json::json!({"target": target.display().to_string(), "error": format!("{e}")})
                ),
            "Failed to remove file after audit rollback"
        );
    }
}

// ─── YAML front-matter helpers (file-local copies; identical to improver) ───

fn split_front_matter(content: &str) -> Option<(String, String)> {
    let normalized = content.replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n")?;
    if let Some(idx) = rest.find("\n---\n") {
        Some((rest[..idx].to_string(), rest[idx + 5..].to_string()))
    } else {
        rest.strip_suffix("\n---")
            .map(|front| (front.to_string(), String::new()))
    }
}

fn front_value(front: &str, key: &str) -> Option<String> {
    for line in front.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim() == key {
            let v = v.trim();
            let unquoted = v.trim_matches('"').trim_matches('\'');
            return Some(unquoted.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn cfg_no_cooldown() -> zeroclaw_config::schema::SkillImprovementConfig {
        zeroclaw_config::schema::SkillImprovementConfig {
            enabled: true,
            cooldown_secs: 0,
            ..Default::default()
        }
    }

    fn cfg_with_cooldown(secs: u64) -> zeroclaw_config::schema::SkillImprovementConfig {
        zeroclaw_config::schema::SkillImprovementConfig {
            enabled: true,
            cooldown_secs: secs,
            ..Default::default()
        }
    }

    async fn write_skill(workspace: &Path, slug: &str, md: &str) {
        let dir = workspace.join("skills").join(slug);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("SKILL.md"), md).await.unwrap();
    }

    const VALID_SKILL: &str = "---\nname: deploy\ndescription: Run a production deploy\nversion: \"0.1.0\"\n---\n\n# Deploy\nDoes a production deploy.\n";

    // ─── skills_list ────────────────────────────────────────

    #[tokio::test]
    async fn skills_list_empty_when_no_skills() {
        let dir = tempdir();
        let tool = SkillsListTool::new(dir.path().to_path_buf());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("0 installed"));
    }

    #[tokio::test]
    async fn skills_list_enumerates_installed_skills() {
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        write_skill(
            dir.path(),
            "test-runner",
            "---\nname: test-runner\ndescription: Run the test suite\nversion: \"0.2.0\"\n---\n\nBody\n",
        )
        .await;

        let tool = SkillsListTool::new(dir.path().to_path_buf());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("deploy"));
        assert!(result.output.contains("test-runner"));
        assert!(result.output.contains("0.1.0"));
        assert!(result.output.contains("0.2.0"));
    }

    #[tokio::test]
    async fn skills_list_skips_archive_dir() {
        let dir = tempdir();
        write_skill(dir.path(), "active", VALID_SKILL).await;
        let archive_path = dir.path().join("skills").join(".archive").join("old-skill");
        tokio::fs::create_dir_all(&archive_path).await.unwrap();
        tokio::fs::write(archive_path.join("SKILL.md"), VALID_SKILL)
            .await
            .unwrap();

        let tool = SkillsListTool::new(dir.path().to_path_buf());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("active"));
        assert!(!result.output.contains("old-skill"));
    }

    // ─── skill_view ─────────────────────────────────────────

    #[tokio::test]
    async fn skill_view_rejects_path_traversal() {
        let dir = tempdir();
        let tool = SkillViewTool::new(dir.path().to_path_buf());
        for bad in ["../etc/passwd", "..", "foo/bar", ".hidden", ""] {
            let result = tool
                .execute(json!({ "slug": bad }))
                .await
                .expect("execute should not error");
            assert!(!result.success, "expected rejection for slug {bad:?}");
        }
    }

    #[tokio::test]
    async fn skill_view_returns_front_matter_and_body() {
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;

        let tool = SkillViewTool::new(dir.path().to_path_buf());
        let result = tool.execute(json!({ "slug": "deploy" })).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("name: deploy"));
        assert!(result.output.contains("Run a production deploy"));
        assert!(result.output.contains("Does a production deploy"));
    }

    #[tokio::test]
    async fn skill_view_lists_support_files() {
        let dir = tempdir();
        let skill_dir = dir.path().join("skills").join("deploy");
        tokio::fs::create_dir_all(skill_dir.join("references"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(skill_dir.join("scripts"))
            .await
            .unwrap();
        tokio::fs::write(skill_dir.join("SKILL.md"), VALID_SKILL)
            .await
            .unwrap();
        tokio::fs::write(skill_dir.join("references").join("api.md"), "...")
            .await
            .unwrap();
        tokio::fs::write(skill_dir.join("scripts").join("verify.sh"), "...")
            .await
            .unwrap();

        let tool = SkillViewTool::new(dir.path().to_path_buf());
        let result = tool.execute(json!({ "slug": "deploy" })).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("references/api.md"));
        assert!(result.output.contains("scripts/verify.sh"));
    }

    // ─── skill_manage: patch ────────────────────────────────

    const IMPROVED_SKILL: &str = "---\nname: deploy\ndescription: Run a production deploy (now with a pre-flight check)\nversion: \"0.1.1\"\n---\n\n# Deploy\nDoes a production deploy.\nRuns a pre-flight check first.\n";

    #[tokio::test]
    async fn skill_manage_patch_atomically_updates_md() {
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);

        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "deploy",
                "content": IMPROVED_SKILL,
                "reason": "User noted missing pre-flight check",
            }))
            .await
            .unwrap();
        assert!(result.success, "patch failed: {:?}", result.error);

        let on_disk =
            tokio::fs::read_to_string(dir.path().join("skills").join("deploy").join("SKILL.md"))
                .await
                .unwrap();
        assert!(on_disk.contains("pre-flight check"));
        assert!(on_disk.contains("0.1.1"));
        assert!(on_disk.contains("updated_at:"));
        assert!(on_disk.contains("improvement_reason:"));
        assert!(on_disk.contains("User noted missing pre-flight check"));
        assert!(on_disk.contains("<!-- Improvement:"));
        assert!(
            !dir.path()
                .join("skills")
                .join("deploy")
                .join(".SKILL.md.tmp")
                .exists()
        );
    }

    #[tokio::test]
    async fn skill_manage_patch_blocks_when_skill_is_on_cooldown() {
        // Regression for #6683: with a non-zero cooldown configured, a skill
        // whose front-matter carries a fresh `updated_at` is on cooldown and
        // a patch must be refused with a structured error rather than writing.
        let dir = tempdir();
        let recent = chrono::Utc::now().to_rfc3339();
        let md = format!(
            "---\nname: deploy\ndescription: Run a production deploy\nversion: \"0.1.0\"\nupdated_at: {recent}\n---\n\n# Deploy\nDoes a production deploy.\n"
        );
        write_skill(dir.path(), "deploy", &md).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_with_cooldown(3600), true);

        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "deploy",
                "content": IMPROVED_SKILL,
                "reason": "second rewrite within cooldown",
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|e| e.contains("cooldown")),
            "error must name the cooldown, got {:?}",
            result.error
        );

        let on_disk =
            tokio::fs::read_to_string(dir.path().join("skills").join("deploy").join("SKILL.md"))
                .await
                .unwrap();
        assert!(!on_disk.contains("pre-flight check"));
    }

    #[tokio::test]
    async fn skill_manage_patch_proceeds_when_skill_is_stale() {
        // Regression for #6683: an `updated_at` older than cooldown_secs is
        // stale and a patch must proceed.
        let dir = tempdir();
        let stale = (chrono::Utc::now() - chrono::Duration::seconds(7200)).to_rfc3339();
        let md = format!(
            "---\nname: deploy\ndescription: Run a production deploy\nversion: \"0.1.0\"\nupdated_at: {stale}\n---\n\n# Deploy\nDoes a production deploy.\n"
        );
        write_skill(dir.path(), "deploy", &md).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_with_cooldown(3600), true);

        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "deploy",
                "content": IMPROVED_SKILL,
                "reason": "stale skill, rewrite allowed",
            }))
            .await
            .unwrap();
        assert!(result.success, "patch failed: {:?}", result.error);

        let on_disk =
            tokio::fs::read_to_string(dir.path().join("skills").join("deploy").join("SKILL.md"))
                .await
                .unwrap();
        assert!(on_disk.contains("pre-flight check"));
    }

    #[tokio::test]
    async fn skill_manage_patch_proceeds_when_no_updated_at() {
        // Regression for #6683: a skill with no `updated_at` is not on
        // cooldown — first patch must proceed even with a cooldown configured.
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_with_cooldown(3600), true);

        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "deploy",
                "content": IMPROVED_SKILL,
                "reason": "first rewrite, no prior timestamp",
            }))
            .await
            .unwrap();
        assert!(result.success, "patch failed: {:?}", result.error);

        let on_disk =
            tokio::fs::read_to_string(dir.path().join("skills").join("deploy").join("SKILL.md"))
                .await
                .unwrap();
        assert!(on_disk.contains("pre-flight check"));
    }

    #[tokio::test]
    async fn skill_manage_patch_rejects_invalid_content() {
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);

        // No front-matter → validation rejects.
        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "deploy",
                "content": "just markdown, no yaml front-matter",
                "reason": "broken",
            }))
            .await
            .unwrap();
        assert!(!result.success);
        let on_disk =
            tokio::fs::read_to_string(dir.path().join("skills").join("deploy").join("SKILL.md"))
                .await
                .unwrap();
        assert_eq!(on_disk, VALID_SKILL);
    }

    #[tokio::test]
    async fn skill_manage_patch_rejects_missing_skill() {
        let dir = tempdir();
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);
        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "nonexistent",
                "content": IMPROVED_SKILL,
                "reason": "n/a",
            }))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn skill_manage_patch_blocked_when_improvement_disabled() {
        // `enabled = false` is the per-tool kill switch. The error message
        // must name the disabled state — not the cooldown — so the operator
        // (or the agent reading the tool history) knows the gate is the
        // feature flag, not a timer that will eventually clear.
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let cfg = zeroclaw_config::schema::SkillImprovementConfig {
            enabled: false,
            cooldown_secs: 0,
            ..Default::default()
        };
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg, true);

        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "deploy",
                "content": IMPROVED_SKILL,
                "reason": "n/a",
            }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap_or_default();
        assert_eq!(err, "Skill improvement is disabled (enabled: false)");
    }

    // ─── skill_manage: write_file ───────────────────────────

    #[tokio::test]
    async fn skill_manage_write_file_creates_references_md() {
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);

        let result = tool
            .execute(json!({
                "action": "write_file",
                "slug": "deploy",
                "file_path": "references/staging-quirks.md",
                "content": "# Staging quirks\n\n- env DEPLOY_TOKEN must be set\n",
            }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);

        let written = tokio::fs::read_to_string(
            dir.path()
                .join("skills")
                .join("deploy")
                .join("references")
                .join("staging-quirks.md"),
        )
        .await
        .unwrap();
        assert!(written.contains("DEPLOY_TOKEN"));
    }

    #[tokio::test]
    async fn skill_manage_write_file_rejects_bad_prefix() {
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);

        for bad in [
            "SKILL.md",
            "../../etc/passwd",
            "secrets/key.pem",
            "references/../../etc/passwd",
            "/etc/passwd",
        ] {
            let result = tool
                .execute(json!({
                    "action": "write_file",
                    "slug": "deploy",
                    "file_path": bad,
                    "content": "nope",
                }))
                .await
                .unwrap();
            assert!(!result.success, "expected rejection for {bad:?}");
        }
        let md =
            tokio::fs::read_to_string(dir.path().join("skills").join("deploy").join("SKILL.md"))
                .await
                .unwrap();
        assert_eq!(md, VALID_SKILL);
    }

    #[tokio::test]
    async fn skill_manage_write_file_enforces_size_cap() {
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);

        let oversized = "x".repeat(MAX_FILE_BYTES + 1);
        let result = tool
            .execute(json!({
                "action": "write_file",
                "slug": "deploy",
                "file_path": "references/big.md",
                "content": oversized,
            }))
            .await
            .unwrap();
        assert!(!result.success);
    }

    // ─── skill_manage: archive ──────────────────────────────

    #[tokio::test]
    async fn skill_manage_archive_moves_skill() {
        let dir = tempdir();
        write_skill(dir.path(), "obsolete", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);

        let result = tool
            .execute(json!({ "action": "archive", "slug": "obsolete" }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);

        assert!(!dir.path().join("skills").join("obsolete").exists());
        assert!(
            dir.path()
                .join("skills")
                .join(".archive")
                .join("obsolete")
                .join("SKILL.md")
                .exists()
        );
    }

    #[tokio::test]
    async fn skill_manage_archive_does_not_clobber_existing_archive() {
        let dir = tempdir();
        write_skill(dir.path(), "obsolete", VALID_SKILL).await;
        let archive_dir = dir.path().join("skills").join(".archive").join("obsolete");
        tokio::fs::create_dir_all(&archive_dir).await.unwrap();
        tokio::fs::write(archive_dir.join("SKILL.md"), VALID_SKILL)
            .await
            .unwrap();

        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);
        let result = tool
            .execute(json!({ "action": "archive", "slug": "obsolete" }))
            .await
            .unwrap();
        assert!(result.success);

        assert!(archive_dir.join("SKILL.md").exists());
        let entries: Vec<_> = std::fs::read_dir(dir.path().join("skills").join(".archive"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(entries.iter().any(|e| e.starts_with("obsolete-")));
    }

    #[tokio::test]
    async fn skill_manage_rejects_unknown_action() {
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);
        let result = tool
            .execute(json!({ "action": "nuke", "slug": "deploy" }))
            .await
            .unwrap();
        assert!(!result.success);
    }

    // ─── Symlink rejection (safe_skill_dir boundary) ────────
    //
    // These tests verify that a symlinked `workspace/skills/<slug>` cannot be
    // used to redirect mutating operations outside the canonical skills root.

    #[cfg(unix)]
    fn symlink_skill_dir(workspace: &Path, slug: &str, real_target: &Path) {
        let link_path = workspace.join("skills").join(slug);
        std::fs::create_dir_all(workspace.join("skills")).unwrap();
        std::os::unix::fs::symlink(real_target, &link_path).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skill_manage_patch_rejects_symlinked_skill_dir() {
        let dir = tempdir();
        // Real skill directory lives elsewhere; the slug entry in workspace
        // skills is a symlink that points at it. A naive resolver would
        // happily patch through the symlink.
        let real_dir = dir.path().join("elsewhere");
        tokio::fs::create_dir_all(&real_dir).await.unwrap();
        tokio::fs::write(real_dir.join("SKILL.md"), VALID_SKILL)
            .await
            .unwrap();
        symlink_skill_dir(dir.path(), "deploy", &real_dir);

        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);
        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "deploy",
                "content": VALID_SKILL,
                "reason": "should be refused",
            }))
            .await
            .unwrap();
        assert!(
            !result.success,
            "patch through symlinked skill dir must be refused"
        );
        assert!(result.error.unwrap_or_default().contains("symlink"));
        // Original SKILL.md untouched.
        let on_disk = tokio::fs::read_to_string(real_dir.join("SKILL.md"))
            .await
            .unwrap();
        assert_eq!(on_disk, VALID_SKILL);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skill_manage_write_file_rejects_symlinked_skill_dir() {
        let dir = tempdir();
        let real_dir = dir.path().join("elsewhere");
        tokio::fs::create_dir_all(&real_dir).await.unwrap();
        tokio::fs::write(real_dir.join("SKILL.md"), VALID_SKILL)
            .await
            .unwrap();
        symlink_skill_dir(dir.path(), "deploy", &real_dir);

        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);
        let result = tool
            .execute(json!({
                "action": "write_file",
                "slug": "deploy",
                "file_path": "references/note.md",
                "content": "should not land",
            }))
            .await
            .unwrap();
        assert!(
            !result.success,
            "write_file through symlinked skill dir must be refused"
        );
        assert!(result.error.unwrap_or_default().contains("symlink"));
        assert!(!real_dir.join("references").join("note.md").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skill_manage_archive_rejects_symlinked_skill_dir() {
        let dir = tempdir();
        let real_dir = dir.path().join("elsewhere");
        tokio::fs::create_dir_all(&real_dir).await.unwrap();
        tokio::fs::write(real_dir.join("SKILL.md"), VALID_SKILL)
            .await
            .unwrap();
        symlink_skill_dir(dir.path(), "deploy", &real_dir);

        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);
        let result = tool
            .execute(json!({ "action": "archive", "slug": "deploy" }))
            .await
            .unwrap();
        assert!(
            !result.success,
            "archive of symlinked skill dir must be refused"
        );
        assert!(result.error.unwrap_or_default().contains("symlink"));
        // Symlink still in place, real dir untouched.
        assert!(dir.path().join("skills").join("deploy").exists());
        assert!(real_dir.join("SKILL.md").exists());
    }

    // ─── Post-mutation audit with rollback ──────────────────

    #[tokio::test]
    async fn skill_manage_patch_rolls_back_on_audit_failure() {
        // Skill dir contains a `scripts/foo.sh`. With `allow_scripts: false`,
        // post-mutation audit fails — the SKILL.md must be rolled back to its
        // pre-patch content so the user is not left with a half-applied edit
        // on a skill the loader would reject.
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;
        let scripts_dir = dir.path().join("skills").join("deploy").join("scripts");
        tokio::fs::create_dir_all(&scripts_dir).await.unwrap();
        tokio::fs::write(scripts_dir.join("foo.sh"), "#!/bin/sh\necho hi\n")
            .await
            .unwrap();

        let tool = SkillManageTool::new(
            dir.path().to_path_buf(),
            cfg_no_cooldown(),
            false, // allow_scripts: false → audit will reject scripts/foo.sh
        );

        let new_content = "---\nname: deploy\ndescription: rewritten\n---\n\n# Deploy v2\n";
        let result = tool
            .execute(json!({
                "action": "patch",
                "slug": "deploy",
                "content": new_content,
                "reason": "rewrite",
            }))
            .await
            .unwrap();
        assert!(!result.success, "patch on audit-failing skill must fail");
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("rolled back"),
            "expected rollback note, got: {err}"
        );
        // SKILL.md restored to original.
        let on_disk =
            tokio::fs::read_to_string(dir.path().join("skills").join("deploy").join("SKILL.md"))
                .await
                .unwrap();
        assert_eq!(on_disk, VALID_SKILL);
    }

    #[tokio::test]
    async fn skill_manage_write_file_audit_failure_removes_file() {
        // Write a `scripts/foo.sh` with `allow_scripts: false`. Audit rejects
        // it → the newly written file must be removed (target did not exist
        // before the write).
        let dir = tempdir();
        write_skill(dir.path(), "deploy", VALID_SKILL).await;

        let tool = SkillManageTool::new(
            dir.path().to_path_buf(),
            cfg_no_cooldown(),
            false, // allow_scripts: false
        );

        let result = tool
            .execute(json!({
                "action": "write_file",
                "slug": "deploy",
                "file_path": "scripts/foo.sh",
                "content": "#!/bin/sh\necho hi\n",
            }))
            .await
            .unwrap();
        assert!(
            !result.success,
            "write of script under allow_scripts=false must fail"
        );
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("rolled back"),
            "expected rollback note, got: {err}"
        );
        // File removed (didn't exist before, so rollback = delete).
        assert!(
            !dir.path()
                .join("skills")
                .join("deploy")
                .join("scripts")
                .join("foo.sh")
                .exists(),
            "rollback should have removed the new script file"
        );
    }

    #[tokio::test]
    async fn skill_manage_archive_pins_target_under_canonical_skills_root() {
        // Sanity check that `archive` builds the target inside the canonical
        // `.archive` directory under skills, not anywhere else.
        let dir = tempdir();
        write_skill(dir.path(), "obsolete", VALID_SKILL).await;
        let tool = SkillManageTool::new(dir.path().to_path_buf(), cfg_no_cooldown(), true);
        let result = tool
            .execute(json!({ "action": "archive", "slug": "obsolete" }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);
        let archived = dir
            .path()
            .join("skills")
            .join(".archive")
            .join("obsolete")
            .join("SKILL.md");
        assert!(archived.exists());
        // Archived path is under canonical skills root.
        let canonical_skills = dir.path().join("skills").canonicalize().unwrap();
        let canonical_archived = archived.canonicalize().unwrap();
        assert!(canonical_archived.starts_with(&canonical_skills));
    }
}
