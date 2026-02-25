use crate::tools::traits::{Tool, ToolResult};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use tokio::process::Command;

/// ApplyPatchTool
///
/// A constrained “self-fix” primitive:
/// - Accepts a unified diff as a string
/// - Optionally dry-runs via `git apply --check`
/// - Applies via `git apply`
/// - Optionally stages + commits when `commit_message` is provided
///
/// Notes:
/// - This tool assumes it is running inside a git repo.
/// - It does NOT fetch, pull, or push.
/// - It does NOT run arbitrary scripts.
/// - It is intentionally narrow: patch in, apply/check, status/commit out.
pub struct ApplyPatchTool;

impl ApplyPatchTool {
    pub fn new() -> Self {
        Self
    }

    fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Unified diff text (e.g. output of `git diff`)."
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, only checks whether the patch would apply cleanly (no changes made).",
                    "default": true
                },
                "commit_message": {
                    "type": "string",
                    "description": "If provided (and dry_run=false), stage all changes and create a git commit with this message."
                }
            },
            "required": ["patch"]
        })
    }
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Safely check/apply a unified diff to the current git repository, optionally staging and committing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        Self::schema()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let patch = args
            .get("patch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing required field: patch (string)"))?
            .to_string();

        // Default to dry_run=true for safety if omitted.
        let dry_run = args
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let commit_message = args
            .get("commit_message")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Basic size guardrail (prevents accidental giant pastes).
        const MAX_PATCH_BYTES: usize = 1_000_000; // 1MB
        if patch.len() > MAX_PATCH_BYTES {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Patch too large ({} bytes). Refusing (> {} bytes).",
                    patch.len(),
                    MAX_PATCH_BYTES
                )),
            });
        }

        let repo_root = git_repo_root().await?;
        let mut log = String::new();
        let _ = writeln!(log, "Repo root: {}", repo_root.display());
        let _ = writeln!(log, "Mode: {}", if dry_run { "dry-run" } else { "apply" });

        // Write patch to a temp file.
        let mut tmp = NamedTempFile::new().context("Failed to create temp file for patch")?;
        std::io::Write::write_all(&mut tmp, patch.as_bytes())
            .context("Failed to write patch to temp file")?;
        let patch_path: PathBuf = tmp.path().to_path_buf();

        // Always run a check first (even if applying).
        {
            let (code, out, err) = run_cmd(
                &repo_root,
                "git",
                &["apply", "--check", patch_path.to_string_lossy().as_ref()],
            )
            .await?;

            log.push_str("\n# git apply --check\n");
            let _ = writeln!(log, "exit_code: {code}");
            if !out.is_empty() {
                log.push_str("stdout:\n");
                log.push_str(&out);
                if !out.ends_with('\n') {
                    log.push('\n');
                }
            }
            if !err.is_empty() {
                log.push_str("stderr:\n");
                log.push_str(&err);
                if !err.ends_with('\n') {
                    log.push('\n');
                }
            }

            if code != 0 {
                return Ok(ToolResult {
                    success: false,
                    output: log,
                    error: Some("Patch check failed (git apply --check). No changes made.".into()),
                });
            }
        }

        if dry_run {
            log.push_str("\nPatch check OK. Dry-run requested, no changes applied.\n");
            return Ok(ToolResult {
                success: true,
                output: log,
                error: None,
            });
        }

        // Apply patch.
        {
            let (code, out, err) = run_cmd(
                &repo_root,
                "git",
                &["apply", patch_path.to_string_lossy().as_ref()],
            )
            .await?;

            log.push_str("\n# git apply\n");
            let _ = writeln!(log, "exit_code: {code}");
            if !out.is_empty() {
                log.push_str("stdout:\n");
                log.push_str(&out);
                if !out.ends_with('\n') {
                    log.push('\n');
                }
            }
            if !err.is_empty() {
                log.push_str("stderr:\n");
                log.push_str(&err);
                if !err.ends_with('\n') {
                    log.push('\n');
                }
            }

            if code != 0 {
                return Ok(ToolResult {
                    success: false,
                    output: log,
                    error: Some("git apply failed. Patch may not have been applied.".into()),
                });
            }
        }

        // Show status.
        {
            let (_code, out, err) = run_cmd(&repo_root, "git", &["status", "--porcelain"]).await?;
            log.push_str("\n# git status --porcelain\n");
            if out.trim().is_empty() {
                log.push_str("(no changes)\n");
            } else {
                log.push_str(&out);
                if !out.ends_with('\n') {
                    log.push('\n');
                }
            }
            if !err.is_empty() {
                log.push_str("stderr:\n");
                log.push_str(&err);
                if !err.ends_with('\n') {
                    log.push('\n');
                }
            }
        }

        // Optionally stage + commit.
        if let Some(msg) = commit_message {
            let (code_add, _out_add, err_add) = run_cmd(&repo_root, "git", &["add", "-A"]).await?;
            log.push_str("\n# git add -A\n");
            let _ = writeln!(log, "exit_code: {code_add}");
            if !err_add.is_empty() {
                log.push_str("stderr:\n");
                log.push_str(&err_add);
                if !err_add.ends_with('\n') {
                    log.push('\n');
                }
            }
            if code_add != 0 {
                return Ok(ToolResult {
                    success: false,
                    output: log,
                    error: Some("git add failed".into()),
                });
            }

            let (code_commit, out_commit, err_commit) =
                run_cmd(&repo_root, "git", &["commit", "-m", msg.as_str()]).await?;
            log.push_str("\n# git commit -m <msg>\n");
            let _ = writeln!(log, "exit_code: {code_commit}");
            if !out_commit.is_empty() {
                log.push_str("stdout:\n");
                log.push_str(&out_commit);
                if !out_commit.ends_with('\n') {
                    log.push('\n');
                }
            }
            if !err_commit.is_empty() {
                log.push_str("stderr:\n");
                log.push_str(&err_commit);
                if !err_commit.ends_with('\n') {
                    log.push('\n');
                }
            }

            if code_commit != 0 {
                // Often means “nothing to commit” or hooks blocked it.
                return Ok(ToolResult {
                    success: false,
                    output: log,
                    error: Some(
                        "git commit failed (possibly nothing to commit, or hooks rejected)".into(),
                    ),
                });
            }

            // Show last commit summary.
            let (_code_show, out_show, err_show) =
                run_cmd(&repo_root, "git", &["show", "--stat", "--oneline", "-1"]).await?;
            log.push_str("\n# git show --stat --oneline -1\n");
            if !out_show.is_empty() {
                log.push_str(&out_show);
                if !out_show.ends_with('\n') {
                    log.push('\n');
                }
            }
            if !err_show.is_empty() {
                log.push_str("stderr:\n");
                log.push_str(&err_show);
                if !err_show.ends_with('\n') {
                    log.push('\n');
                }
            }
        }

        Ok(ToolResult {
            success: true,
            output: log,
            error: None,
        })
    }
}

async fn git_repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("Failed to read current_dir")?;
    let (code, out, err) = run_cmd(&cwd, "git", &["rev-parse", "--show-toplevel"]).await?;
    if code != 0 {
        return Err(anyhow!(
            "Not a git repo (git rev-parse failed). stderr: {}",
            err.trim()
        ));
    }
    let root = out.trim();
    if root.is_empty() {
        return Err(anyhow!("git rev-parse returned empty repo root"));
    }
    Ok(PathBuf::from(root))
}

async fn run_cmd(dir: &Path, program: &str, args: &[&str]) -> Result<(i32, String, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir);

    let output = cmd
        .output()
        .await
        .with_context(|| format!("Failed to run command: {program} {:?}", args))?;

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((code, stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_object() {
        let s = ApplyPatchTool::schema();
        assert!(s.is_object());
        assert_eq!(s["type"], "object");
        assert!(s["properties"].is_object());
        assert!(s["properties"]["patch"].is_object());
    }
}
