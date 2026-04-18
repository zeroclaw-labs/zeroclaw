use super::artifact::{append_artifacts, is_artifact_extension, Artifact};
use super::file_write::DownloadUrlConfig;
use super::traits::{Tool, ToolResult};
use crate::runtime::RuntimeAdapter;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Maximum shell command execution time before kill.
const SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;
/// Environment variables safe to pass to shell commands.
/// Only functional variables are included — never API keys or secrets.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Maximum recursion depth when scanning workspace for artifacts.
/// Bounded to keep snapshot cheap on deep skill checkouts.
const ARTIFACT_SCAN_MAX_DEPTH: usize = 8;
/// Hard cap on entries scanned per workspace pass — protects against
/// pathological trees (huge `node_modules`, model checkpoints, etc.) that
/// somehow slipped past `ARTIFACT_SCAN_SKIP_DIRS`.
const ARTIFACT_SCAN_MAX_ENTRIES: usize = 5_000;
/// Maximum number of artifacts surfaced per shell call. Skills like `unzip` or
/// `pandoc -t docx --extract-media` can produce dozens of files; surfacing all
/// of them as download buttons is noise — keep the most recent few.
const ARTIFACT_MAX_PER_CALL: usize = 5;
/// Directories never worth scanning for downloadable artifacts. Conservative
/// list — anything matching SCM, package-manager, or build caches.
const ARTIFACT_SCAN_SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    "dist",
    "build",
    ".next",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".idea",
    ".vscode",
];

/// Shell command execution tool with sandboxing.
///
/// When `download` is configured, post-execution the workspace is scanned for
/// newly-created or modified files matching [`SHELL_ARTIFACT_EXTENSIONS`] and
/// the resulting [`Artifact`] list is appended to the tool output via the
/// sentinel codec ([`append_artifacts`]). This is how skills that produce
/// `.docx`/`.pptx`/`.xlsx`/`.pdf` via `node`/`python`/`pandoc` get picked up
/// — they never touch `file_write` so the legacy `Download:` mechanism would
/// otherwise miss them entirely.
pub struct ShellTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    download: Option<DownloadUrlConfig>,
}

impl ShellTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        Self {
            security,
            runtime,
            download: None,
        }
    }

    /// Construct with download URL signing enabled. Required for shell-produced
    /// files to surface as artifacts in chat clients.
    pub fn with_download(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        download: DownloadUrlConfig,
    ) -> Self {
        Self {
            security,
            runtime,
            download: Some(download),
        }
    }
}

/// Walk `workspace_dir` collecting `(relative_path, mtime)` for every file
/// whose extension is in [`SHELL_ARTIFACT_EXTENSIONS`].
///
/// Bounded by [`ARTIFACT_SCAN_MAX_DEPTH`] and [`ARTIFACT_SCAN_MAX_ENTRIES`].
/// Skips directories listed in [`ARTIFACT_SCAN_SKIP_DIRS`].
///
/// Errors are silently ignored — this is a best-effort attribution layer,
/// not a critical-path operation. Returning a partial map is always
/// preferable to failing the entire shell call.
fn scan_workspace_artifact_mtimes(workspace_dir: &Path) -> HashMap<PathBuf, SystemTime> {
    let mut out = HashMap::new();
    let mut entries_seen: usize = 0;
    let mut stack: Vec<(PathBuf, usize)> = vec![(workspace_dir.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > ARTIFACT_SCAN_MAX_DEPTH || entries_seen >= ARTIFACT_SCAN_MAX_ENTRIES {
            continue;
        }
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read_dir.flatten() {
            entries_seen += 1;
            if entries_seen >= ARTIFACT_SCAN_MAX_ENTRIES {
                break;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            // Defence in depth: never follow symlinks during scan — same
            // principle as the symlink protection in file_write.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if ARTIFACT_SCAN_SKIP_DIRS.contains(&name_str.as_ref()) {
                    continue;
                }
                stack.push((path, depth + 1));
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let path_str = path.to_string_lossy();
            if !is_artifact_extension(&path_str) {
                continue;
            }
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.insert(path, mtime);
        }
    }
    out
}

/// Diff two mtime snapshots: return paths that are new in `after` or whose
/// mtime advanced relative to `before`.
fn detect_new_or_modified(
    before: &HashMap<PathBuf, SystemTime>,
    after: &HashMap<PathBuf, SystemTime>,
) -> Vec<(PathBuf, SystemTime)> {
    let mut changed: Vec<(PathBuf, SystemTime)> = after
        .iter()
        .filter(|(path, mtime)| match before.get(*path) {
            Some(prev) => mtime > &prev,
            None => true,
        })
        .map(|(p, m)| (p.clone(), *m))
        .collect();
    // Newest-first so when we cap at ARTIFACT_MAX_PER_CALL we keep the most
    // recent (typically the actual deliverable, not intermediate temp files).
    changed.sort_by(|a, b| b.1.cmp(&a.1));
    changed
}

/// Build [`Artifact`] objects from a list of changed workspace paths.
/// Resolves them to workspace-relative form, signs download URLs if configured,
/// and discards entries that fail to stat (e.g. deleted between scan and now).
fn build_shell_artifacts(
    workspace_dir: &Path,
    changed: &[(PathBuf, SystemTime)],
    download: Option<&DownloadUrlConfig>,
) -> Vec<Artifact> {
    changed
        .iter()
        .take(ARTIFACT_MAX_PER_CALL)
        .filter_map(|(path, _mtime)| {
            let rel = path.strip_prefix(workspace_dir).ok()?;
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let download_url = download.map(|dl| {
                crate::gateway::signed_url::sign_download_url(
                    &dl.base_url,
                    &rel_str,
                    &dl.secret,
                    crate::gateway::signed_url::DEFAULT_TTL_SECS,
                )
            });
            // `Artifact::from_workspace_path` handles size/mime resolution
            // consistently with file_write/file_edit.
            Artifact::from_workspace_path(workspace_dir, &rel_str, download_url)
        })
        .collect()
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn collect_allowed_shell_env_vars(security: &SecurityPolicy) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in SAFE_ENV_VARS
        .iter()
        .copied()
        .chain(security.shell_env_passthrough.iter().map(|s| s.as_str()))
    {
        let candidate = key.trim();
        if candidate.is_empty() || !is_valid_env_var_name(candidate) {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the workspace directory"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk commands in supervised mode",
                    "default": false
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;
        let approved = args
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        match self.security.validate_command_execution(command, approved) {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        }

        if let Some(path) = self.security.forbidden_path_argument(command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Execute with timeout to prevent hanging commands.
        // Clear the environment to prevent leaking API keys and other secrets
        // (CWE-200), then re-add only safe, functional variables.
        let mut cmd = match self
            .runtime
            .build_shell_command(command, &self.security.workspace_dir)
        {
            Ok(cmd) => cmd,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build runtime command: {e}")),
                });
            }
        };
        cmd.env_clear();

        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }

        // Pre-execution snapshot of artifact-extension files in the workspace.
        // Skipped entirely when no download config is wired (typical in tests
        // and minimal deployments) — there is no consumer for the artifacts.
        let pre_snapshot = self
            .download
            .as_ref()
            .map(|_| scan_workspace_artifact_mtimes(&self.security.workspace_dir));

        let result =
            tokio::time::timeout(Duration::from_secs(SHELL_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate output to prevent OOM
                if stdout.len() > MAX_OUTPUT_BYTES {
                    let mut b = MAX_OUTPUT_BYTES.min(stdout.len());
                    while b > 0 && !stdout.is_char_boundary(b) {
                        b -= 1;
                    }
                    stdout.truncate(b);
                    stdout.push_str("\n... [output truncated at 1MB]");
                }
                if stderr.len() > MAX_OUTPUT_BYTES {
                    let mut b = MAX_OUTPUT_BYTES.min(stderr.len());
                    while b > 0 && !stderr.is_char_boundary(b) {
                        b -= 1;
                    }
                    stderr.truncate(b);
                    stderr.push_str("\n... [stderr truncated at 1MB]");
                }

                // Post-execution: detect new/modified artifact files and append
                // them to stdout via the sentinel codec. Only runs when the
                // command succeeded — failed commands likely produced partial
                // or corrupt files we shouldn't surface as deliverables.
                if output.status.success() {
                    if let (Some(pre), Some(dl)) = (pre_snapshot, self.download.as_ref()) {
                        let post = scan_workspace_artifact_mtimes(&self.security.workspace_dir);
                        let changed = detect_new_or_modified(&pre, &post);
                        if !changed.is_empty() {
                            let artifacts = build_shell_artifacts(
                                &self.security.workspace_dir,
                                &changed,
                                Some(dl),
                            );
                            append_artifacts(&mut stdout, &artifacts);
                        }
                    }
                }

                Ok(ToolResult {
                    success: output.status.success(),
                    output: stdout,
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute command: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Command timed out after {SHELL_TIMEOUT_SECS}s and was killed"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{NativeRuntime, RuntimeAdapter};
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    #[test]
    fn shell_tool_name() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(schema["required"]
            .as_array()
            .expect("schema required field should be an array")
            .contains(&json!("command")));
        assert!(schema["properties"]["approved"].is_object());
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command execution should succeed");
        assert!(result.success);
        assert!(result.output.trim().contains("hello"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn shell_blocks_disallowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "rm -rf /"}))
            .await
            .expect("disallowed command execution should return a result");
        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or("");
        assert!(error.contains("not allowed") || error.contains("high-risk"));
    }

    #[tokio::test]
    async fn shell_blocks_readonly() {
        let tool = ShellTool::new(test_security(AutonomyLevel::ReadOnly), test_runtime());
        let result = tool
            .execute(json!({"command": "ls"}))
            .await
            .expect("readonly command execution should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_ref()
            .expect("error field should be present for blocked command")
            .contains("not allowed"));
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz"}))
            .await
            .expect("command with nonexistent path should return a result");
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_blocks_absolute_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat /etc/passwd"}))
            .await
            .expect("absolute path argument should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Path blocked"));
    }

    #[tokio::test]
    async fn shell_blocks_option_assignment_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "grep --file=/etc/passwd root ./src"}))
            .await
            .expect("option-assigned forbidden path should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Path blocked"));
    }

    #[tokio::test]
    async fn shell_blocks_short_option_attached_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "grep -f/etc/passwd root ./src"}))
            .await
            .expect("short option attached forbidden path should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Path blocked"));
    }

    #[tokio::test]
    async fn shell_blocks_tilde_user_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat ~root/.ssh/id_rsa"}))
            .await
            .expect("tilde-user path should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Path blocked"));
    }

    #[tokio::test]
    async fn shell_blocks_input_redirection_path_bypass() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat </etc/passwd"}))
            .await
            .expect("input redirection bypass should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not allowed"));
    }

    fn test_security_with_env_cmd() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into(), "echo".into()],
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_env_passthrough(vars: &[&str]) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into()],
            shell_env_passthrough: vars.iter().map(|v| (*v).to_string()).collect(),
            ..SecurityPolicy::default()
        })
    }

    /// RAII guard that restores an environment variable to its original state on drop,
    /// ensuring cleanup even if the test panics.
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(val) => std::env::set_var(self.key, val),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_does_not_leak_api_key() {
        let _g1 = EnvGuard::set("API_KEY", "sk-test-secret-12345");
        let _g2 = EnvGuard::set("ZEROCLAW_API_KEY", "sk-test-secret-67890");

        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command execution should succeed");
        assert!(result.success);
        assert!(
            !result.output.contains("sk-test-secret-12345"),
            "API_KEY leaked to shell command output"
        );
        assert!(
            !result.output.contains("sk-test-secret-67890"),
            "ZEROCLAW_API_KEY leaked to shell command output"
        );
    }

    #[tokio::test]
    async fn shell_preserves_path_and_home_for_env_command() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command should succeed");
        assert!(result.success);
        assert!(
            result.output.contains("HOME="),
            "HOME should be available in shell environment"
        );
        assert!(
            result.output.contains("PATH="),
            "PATH should be available in shell environment"
        );
    }

    #[tokio::test]
    async fn shell_blocks_plain_variable_expansion() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "echo $HOME"}))
            .await
            .expect("plain variable expansion should be blocked");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not allowed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_allows_configured_env_passthrough() {
        let _guard = EnvGuard::set("ZEROCLAW_TEST_PASSTHROUGH", "db://unit-test");
        let tool = ShellTool::new(
            test_security_with_env_passthrough(&["ZEROCLAW_TEST_PASSTHROUGH"]),
            test_runtime(),
        );

        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command execution should succeed");
        assert!(result.success);
        assert!(result
            .output
            .contains("ZEROCLAW_TEST_PASSTHROUGH=db://unit-test"));
    }

    #[test]
    fn invalid_shell_env_passthrough_names_are_filtered() {
        let security = SecurityPolicy {
            shell_env_passthrough: vec![
                "VALID_NAME".into(),
                "BAD-NAME".into(),
                "1NOPE".into(),
                "ALSO_VALID".into(),
            ],
            ..SecurityPolicy::default()
        };
        let vars = collect_allowed_shell_env_vars(&security);
        assert!(vars.contains(&"VALID_NAME".to_string()));
        assert!(vars.contains(&"ALSO_VALID".to_string()));
        assert!(!vars.contains(&"BAD-NAME".to_string()));
        assert!(!vars.contains(&"1NOPE".to_string()));
    }

    #[tokio::test]
    async fn shell_requires_approval_for_medium_risk_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["touch".into()],
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });

        let tool = ShellTool::new(security.clone(), test_runtime());
        let denied = tool
            .execute(json!({"command": "touch zeroclaw_shell_approval_test"}))
            .await
            .expect("unapproved command should return a result");
        assert!(!denied.success);
        assert!(denied
            .error
            .as_deref()
            .unwrap_or("")
            .contains("explicit approval"));

        let allowed = tool
            .execute(json!({
                "command": "touch zeroclaw_shell_approval_test",
                "approved": true
            }))
            .await
            .expect("approved command execution should succeed");
        assert!(allowed.success);

        let _ =
            tokio::fs::remove_file(std::env::temp_dir().join("zeroclaw_shell_approval_test")).await;
    }

    // ── §5.2 Shell timeout enforcement tests ─────────────────

    #[test]
    fn shell_timeout_constant_is_reasonable() {
        assert_eq!(SHELL_TIMEOUT_SECS, 60, "shell timeout must be 60 seconds");
    }

    #[test]
    fn shell_output_limit_is_1mb() {
        assert_eq!(
            MAX_OUTPUT_BYTES, 1_048_576,
            "max output must be 1 MB to prevent OOM"
        );
    }

    // ── §5.3 Non-UTF8 binary output tests ────────────────────

    #[test]
    fn shell_safe_env_vars_excludes_secrets() {
        for var in SAFE_ENV_VARS {
            let lower = var.to_lowercase();
            assert!(
                !lower.contains("key") && !lower.contains("secret") && !lower.contains("token"),
                "SAFE_ENV_VARS must not include sensitive variable: {var}"
            );
        }
    }

    #[test]
    fn shell_safe_env_vars_includes_essentials() {
        assert!(
            SAFE_ENV_VARS.contains(&"PATH"),
            "PATH must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"HOME"),
            "HOME must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"TERM"),
            "TERM must be in safe env vars"
        );
    }

    #[tokio::test]
    async fn shell_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "echo test"}))
            .await
            .expect("rate-limited command should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn shell_handles_nonexistent_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "nonexistent_binary_xyz_12345"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_captures_stderr_output() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Full), test_runtime());
        let result = tool
            .execute(json!({"command": "echo error_msg >&2"}))
            .await
            .unwrap();
        assert!(result.error.as_deref().unwrap_or("").contains("error_msg"));
    }

    #[tokio::test]
    async fn shell_record_action_budget_exhaustion() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 1,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());

        let r1 = tool
            .execute(json!({"command": "echo first"}))
            .await
            .unwrap();
        assert!(r1.success);

        let r2 = tool
            .execute(json!({"command": "echo second"}))
            .await
            .unwrap();
        assert!(!r2.success);
        assert!(
            r2.error.as_deref().unwrap_or("").contains("Rate limit")
                || r2.error.as_deref().unwrap_or("").contains("budget")
        );
    }

    // ── Artifact detection (workspace pre/post diff) ─────────────────────

    /// Direct unit test of `scan_workspace_artifact_mtimes`: must include
    /// extension-whitelisted files and skip everything else.
    #[test]
    fn scan_picks_only_whitelisted_extensions() {
        let dir = std::env::temp_dir().join("zeroclaw_test_shell_scan_whitelist");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("doc.docx"), b"x").unwrap();
        std::fs::write(dir.join("script.py"), b"y").unwrap();
        std::fs::write(dir.join("report.pdf"), b"z").unwrap();

        let snap = scan_workspace_artifact_mtimes(&dir);
        let names: HashSet<_> = snap
            .keys()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert!(names.contains("doc.docx"));
        assert!(names.contains("report.pdf"));
        assert!(!names.contains("script.py"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Skip dirs (`node_modules`, `.git`, …) must not be descended into,
    /// regardless of how many artifact files they contain.
    #[test]
    fn scan_skips_heavy_dirs() {
        let dir = std::env::temp_dir().join("zeroclaw_test_shell_scan_skip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules/dep")).unwrap();
        std::fs::create_dir_all(dir.join(".git/objects")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("node_modules/dep/leak.docx"), b"x").unwrap();
        std::fs::write(dir.join(".git/objects/leak.pdf"), b"x").unwrap();
        std::fs::write(dir.join("src/legit.docx"), b"x").unwrap();

        let snap = scan_workspace_artifact_mtimes(&dir);
        let names: HashSet<_> = snap
            .keys()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert!(names.contains("legit.docx"), "src/legit.docx must be found");
        assert!(
            !names.contains("leak.docx"),
            "node_modules contents must be skipped"
        );
        assert!(!names.contains("leak.pdf"), ".git contents must be skipped");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `detect_new_or_modified` must surface both new files and bumped mtimes,
    /// and order them newest-first so the per-call cap keeps the most recent.
    #[test]
    fn detect_orders_newest_first() {
        let mut before: HashMap<PathBuf, SystemTime> = HashMap::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let t2 = SystemTime::UNIX_EPOCH + Duration::from_secs(300);
        before.insert(PathBuf::from("/ws/old.docx"), t0);

        let mut after: HashMap<PathBuf, SystemTime> = HashMap::new();
        after.insert(PathBuf::from("/ws/old.docx"), t1); // bumped
        after.insert(PathBuf::from("/ws/brand_new.pdf"), t2); // new

        let changed = detect_new_or_modified(&before, &after);
        assert_eq!(changed.len(), 2);
        assert_eq!(changed[0].0, PathBuf::from("/ws/brand_new.pdf"));
        assert_eq!(changed[1].0, PathBuf::from("/ws/old.docx"));
    }

    /// Files whose mtime is unchanged between snapshots must NOT be reported,
    /// even if their content was overwritten with identical bytes (mtime is
    /// the only signal we use, and that's intentional).
    #[test]
    fn detect_ignores_unchanged() {
        let mut before: HashMap<PathBuf, SystemTime> = HashMap::new();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        before.insert(PathBuf::from("/ws/stable.docx"), t);

        let mut after = before.clone();
        after.insert(PathBuf::from("/ws/added.pdf"), t);

        let changed = detect_new_or_modified(&before, &after);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, PathBuf::from("/ws/added.pdf"));
    }

    /// Per-call cap honoured — even with 20 changed files we never emit more
    /// than `ARTIFACT_MAX_PER_CALL` artifacts (= 5).
    #[test]
    fn build_artifacts_caps_per_call() {
        let dir = std::env::temp_dir().join("zeroclaw_test_shell_artifact_cap");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut changed: Vec<(PathBuf, SystemTime)> = Vec::new();
        for i in 0u64..20 {
            let p = dir.join(format!("f{i}.docx"));
            std::fs::write(&p, b"x").unwrap();
            changed.push((p, SystemTime::UNIX_EPOCH + Duration::from_secs(1000 + i)));
        }
        // newest-first
        changed.sort_by(|a, b| b.1.cmp(&a.1));

        let arts = build_shell_artifacts(&dir, &changed, None);
        assert_eq!(
            arts.len(),
            ARTIFACT_MAX_PER_CALL,
            "must not exceed the per-call cap"
        );
        // Confirm we kept the newest entries
        assert!(arts[0].name.starts_with("f19"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `build_shell_artifacts` with download config must populate `download_url`.
    #[test]
    fn build_artifacts_signs_download_url_when_configured() {
        let dir = std::env::temp_dir().join("zeroclaw_test_shell_artifact_signed");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.docx");
        std::fs::write(&path, b"hello").unwrap();
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1);

        let dl = DownloadUrlConfig {
            base_url: "https://gw.example".into(),
            secret: b"secret-key".to_vec(),
        };
        let arts = build_shell_artifacts(&dir, &[(path, mtime)], Some(&dl));
        assert_eq!(arts.len(), 1);
        let url = arts[0].download_url.as_deref().expect("url must be signed");
        assert!(url.starts_with("https://gw.example/download/out.docx?expires="));
        assert!(url.contains("&sig="));
        assert_eq!(arts[0].size_bytes, 5);
        assert_eq!(
            arts[0].mime.as_deref(),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end: `ShellTool::with_download` running an allowed command that
    /// creates a `.docx` file emits the file as a structured artifact via the
    /// sentinel codec, and the artifact carries a signed download URL.
    ///
    /// Uses `touch` (added to allowed_commands) which is the simplest way to
    /// materialise a file via shell without needing redirect-capable commands.
    #[tokio::test]
    async fn shell_with_download_emits_artifact_for_new_docx() {
        let workspace = std::env::temp_dir().join("zeroclaw_test_shell_e2e_artifact");
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            allowed_commands: vec!["touch".into()],
            require_approval_for_medium_risk: false,
            ..SecurityPolicy::default()
        });
        let dl = DownloadUrlConfig {
            base_url: "https://gw.example".into(),
            secret: b"secret-key".to_vec(),
        };
        let tool = ShellTool::with_download(security, test_runtime(), dl);

        let result = tool
            .execute(json!({"command": "touch report.docx", "approved": true}))
            .await
            .expect("touch must succeed");
        assert!(
            result.success,
            "shell call failed: {:?}",
            result.error.as_deref()
        );

        let (cleaned, artifacts) = crate::tools::artifact::extract_artifacts(&result.output);
        // Sentinel must be stripped from cleaned output
        assert!(
            !cleaned.contains("zeroclaw-artifacts"),
            "sentinel must be removed from cleaned text"
        );
        assert_eq!(artifacts.len(), 1, "exactly one artifact for one new file");
        let art = &artifacts[0];
        assert_eq!(art.path, "report.docx");
        assert_eq!(art.name, "report.docx");
        assert_eq!(
            art.mime.as_deref(),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        );
        assert!(art
            .download_url
            .as_deref()
            .unwrap_or("")
            .contains("/download/report.docx?expires="));

        let _ = tokio::fs::remove_dir_all(&workspace).await;
    }

    /// Failed shell commands must NOT emit artifacts — partial/corrupt files
    /// from a failing run shouldn't be surfaced as deliverables.
    #[tokio::test]
    async fn shell_failed_command_emits_no_artifacts() {
        let workspace = std::env::temp_dir().join("zeroclaw_test_shell_e2e_failure");
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            // Allow `find` but use an arg that makes find fail (nonexistent path).
            allowed_commands: vec!["find".into()],
            require_approval_for_medium_risk: false,
            ..SecurityPolicy::default()
        });
        let dl = DownloadUrlConfig {
            base_url: "https://gw.example".into(),
            secret: b"secret-key".to_vec(),
        };
        let tool = ShellTool::with_download(security, test_runtime(), dl);

        // Pre-create a docx so the workspace isn't empty (the artifact must
        // not be picked up, since its mtime is older than the snapshot moment).
        std::fs::write(workspace.join("preexisting.docx"), b"x").unwrap();

        let result = tool
            .execute(json!({"command": "find /nonexistent_path_zcw_test"}))
            .await
            .expect("execute must complete");
        assert!(!result.success, "find on missing path should fail");

        let (_, artifacts) = crate::tools::artifact::extract_artifacts(&result.output);
        assert!(
            artifacts.is_empty(),
            "failed commands must not emit artifacts (got {:?})",
            artifacts
        );

        let _ = tokio::fs::remove_dir_all(&workspace).await;
    }

    /// Without `with_download`, the scan is short-circuited entirely — no
    /// performance cost when no consumer is configured.
    #[tokio::test]
    async fn shell_without_download_skips_scan() {
        let workspace = std::env::temp_dir().join("zeroclaw_test_shell_no_download");
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            allowed_commands: vec!["touch".into()],
            require_approval_for_medium_risk: false,
            ..SecurityPolicy::default()
        });
        // Note: ShellTool::new (NOT with_download)
        let tool = ShellTool::new(security, test_runtime());

        let result = tool
            .execute(json!({"command": "touch should_not_surface.docx", "approved": true}))
            .await
            .expect("touch must succeed");
        assert!(result.success);

        let (_, artifacts) = crate::tools::artifact::extract_artifacts(&result.output);
        assert!(
            artifacts.is_empty(),
            "no download config = no artifact scan"
        );

        let _ = tokio::fs::remove_dir_all(&workspace).await;
    }
}
