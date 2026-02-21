use super::traits::{Tool, ToolResult};
use crate::runtime::RuntimeAdapter;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Maximum shell command execution time before kill (sync mode).
const SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum background task execution time before kill.
const BACKGROUND_TIMEOUT_SECS: u64 = 3600;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;
/// Maximum concurrent background tasks in the registry.
const MAX_BACKGROUND_TASKS: usize = 50;
/// Environment variables safe to pass to shell commands.
/// Only functional variables are included — never API keys or secrets.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

// ── Background task registry ────────────────────────────────────────

/// Status of a background shell task.
#[derive(Debug, Clone)]
pub enum BackgroundTaskStatus {
    Running,
    Completed { exit_code: i32 },
    Failed { error: String },
}

/// Metadata for a single background shell task.
#[derive(Debug, Clone)]
pub struct BackgroundTask {
    pub id: String,
    pub command: String,
    pub log_path: PathBuf,
    pub pid: u32,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub status: BackgroundTaskStatus,
}

/// In-memory registry of background shell tasks.
/// Shared between `ShellTool` and `ShellStatusTool` via `Arc`.
pub struct BackgroundTaskRegistry {
    tasks: parking_lot::Mutex<HashMap<String, BackgroundTask>>,
}

impl Default for BackgroundTaskRegistry {
    fn default() -> Self {
        Self {
            tasks: parking_lot::Mutex::new(HashMap::new()),
        }
    }
}

impl BackgroundTaskRegistry {
    /// Insert a new task. If at capacity, remove oldest completed tasks first.
    pub fn insert(&self, task: BackgroundTask) {
        let mut tasks = self.tasks.lock();
        // Evict oldest completed tasks if at capacity.
        while tasks.len() >= MAX_BACKGROUND_TASKS {
            let oldest_completed = tasks
                .iter()
                .filter(|(_, t)| !matches!(t.status, BackgroundTaskStatus::Running))
                .min_by_key(|(_, t)| t.started_at)
                .map(|(id, _)| id.clone());
            match oldest_completed {
                Some(id) => {
                    tasks.remove(&id);
                }
                None => break, // all tasks are running, can't evict
            }
        }
        tasks.insert(task.id.clone(), task);
    }

    /// Update a task's status.
    pub fn update_status(&self, task_id: &str, status: BackgroundTaskStatus) {
        let mut tasks = self.tasks.lock();
        if let Some(task) = tasks.get_mut(task_id) {
            task.status = status;
        }
    }

    /// Get a snapshot of a single task.
    pub fn get(&self, task_id: &str) -> Option<BackgroundTask> {
        self.tasks.lock().get(task_id).cloned()
    }

    /// List all tasks (snapshot).
    pub fn list(&self) -> Vec<BackgroundTask> {
        let tasks = self.tasks.lock();
        let mut list: Vec<BackgroundTask> = tasks.values().cloned().collect();
        list.sort_by_key(|t| t.started_at);
        list
    }

    /// Check if the registry is full (all slots occupied by running tasks).
    pub fn is_full(&self) -> bool {
        let tasks = self.tasks.lock();
        tasks.len() >= MAX_BACKGROUND_TASKS
            && tasks
                .values()
                .all(|t| matches!(t.status, BackgroundTaskStatus::Running))
    }
}

// ── Shell tool ──────────────────────────────────────────────────────

/// Shell command execution tool with sandboxing and optional background mode.
pub struct ShellTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    bg_registry: Arc<BackgroundTaskRegistry>,
}

impl ShellTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        bg_registry: Arc<BackgroundTaskRegistry>,
    ) -> Self {
        Self {
            security,
            runtime,
            bg_registry,
        }
    }
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
        "Execute a shell command in the workspace directory. Set background=true for long-running commands (git clone, pip install, etc.) to avoid timeouts; then report to the user that the task is running and poll with shell_status at least 10 seconds apart."
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
                },
                "background": {
                    "type": "boolean",
                    "description": "Run the command in the background. Returns a task_id immediately. For long-running tasks, report the task_id to the user first, then use shell_status to check progress — wait at least 10 seconds between polls.",
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
        let background = args
            .get("background")
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

        if background {
            return self.execute_background(command).await;
        }

        self.execute_sync(command).await
    }
}

impl ShellTool {
    /// Execute a shell command synchronously with timeout.
    async fn execute_sync(&self, command: &str) -> anyhow::Result<ToolResult> {
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

        let result =
            tokio::time::timeout(Duration::from_secs(SHELL_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate output to prevent OOM
                if stdout.len() > MAX_OUTPUT_BYTES {
                    stdout.truncate(stdout.floor_char_boundary(MAX_OUTPUT_BYTES));
                    stdout.push_str("\n... [output truncated at 1MB]");
                }
                if stderr.len() > MAX_OUTPUT_BYTES {
                    stderr.truncate(stderr.floor_char_boundary(MAX_OUTPUT_BYTES));
                    stderr.push_str("\n... [stderr truncated at 1MB]");
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

    /// Spawn a background shell command. Returns immediately with task_id.
    async fn execute_background(&self, command: &str) -> anyhow::Result<ToolResult> {
        if self.bg_registry.is_full() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Background task limit reached ({MAX_BACKGROUND_TASKS}). \
                     Kill or wait for existing tasks before starting new ones."
                )),
            });
        }

        let task_id = uuid::Uuid::new_v4().to_string();

        // Create log directory and file.
        let log_dir = self.security.workspace_dir.join("background_tasks");
        if let Err(e) = tokio::fs::create_dir_all(&log_dir).await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to create background_tasks directory: {e}")),
            });
        }
        let log_path = log_dir.join(format!("{task_id}.log"));

        let log_file = match std::fs::File::create(&log_path) {
            Ok(f) => f,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to create log file: {e}")),
                });
            }
        };
        let stderr_file = match log_file.try_clone() {
            Ok(f) => f,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to clone log file handle: {e}")),
                });
            }
        };

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
        for var in SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }
        cmd.stdout(log_file);
        cmd.stderr(stderr_file);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to spawn background command: {e}")),
                });
            }
        };

        let pid = child.id().unwrap_or(0);

        let task = BackgroundTask {
            id: task_id.clone(),
            command: command.to_string(),
            log_path: log_path.clone(),
            pid,
            started_at: chrono::Utc::now(),
            status: BackgroundTaskStatus::Running,
        };
        self.bg_registry.insert(task);

        // Spawn a monitoring task to update status when the process completes.
        let registry = Arc::clone(&self.bg_registry);
        let monitor_task_id = task_id.clone();
        tokio::spawn(async move {
            let result =
                tokio::time::timeout(Duration::from_secs(BACKGROUND_TIMEOUT_SECS), child.wait())
                    .await;

            match result {
                Ok(Ok(status)) => {
                    let code = status.code().unwrap_or(-1);
                    registry.update_status(
                        &monitor_task_id,
                        BackgroundTaskStatus::Completed { exit_code: code },
                    );
                }
                Ok(Err(e)) => {
                    registry.update_status(
                        &monitor_task_id,
                        BackgroundTaskStatus::Failed {
                            error: format!("Process error: {e}"),
                        },
                    );
                }
                Err(_) => {
                    // Timeout — kill the child.
                    let _ = child.kill().await;
                    registry.update_status(
                        &monitor_task_id,
                        BackgroundTaskStatus::Failed {
                            error: format!(
                                "Background task timed out after {BACKGROUND_TIMEOUT_SECS}s"
                            ),
                        },
                    );
                }
            }
        });

        Ok(ToolResult {
            success: true,
            output: json!({
                "task_id": task_id,
                "pid": pid,
                "log_path": log_path.display().to_string(),
                "status": "running"
            })
            .to_string(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{NativeRuntime, RuntimeAdapter};
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_registry() -> Arc<BackgroundTaskRegistry> {
        Arc::new(BackgroundTaskRegistry::default())
    }

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
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(schema["required"]
            .as_array()
            .expect("schema required field should be an array")
            .contains(&json!("command")));
        assert!(schema["properties"]["approved"].is_object());
        assert!(schema["properties"]["background"].is_object());
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
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
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
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
        let tool = ShellTool::new(
            test_security(AutonomyLevel::ReadOnly),
            test_runtime(),
            test_registry(),
        );
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
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
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

        let tool = ShellTool::new(
            test_security_with_env_cmd(),
            test_runtime(),
            test_registry(),
        );
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
        let tool = ShellTool::new(
            test_security_with_env_cmd(),
            test_runtime(),
            test_registry(),
        );

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
            test_registry(),
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

        let tool = ShellTool::new(security.clone(), test_runtime(), test_registry());
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
        let tool = ShellTool::new(security, test_runtime(), test_registry());
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
        let tool = ShellTool::new(security, test_runtime(), test_registry());
        let result = tool
            .execute(json!({"command": "nonexistent_binary_xyz_12345"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_captures_stderr_output() {
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Full),
            test_runtime(),
            test_registry(),
        );
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
        let tool = ShellTool::new(security, test_runtime(), test_registry());

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

    // ── Background task tests ────────────────────────────────

    #[tokio::test]
    async fn shell_background_returns_task_id() {
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            test_registry(),
        );
        let result = tool
            .execute(json!({"command": "echo background_test", "background": true}))
            .await
            .expect("background command should return a result");
        assert!(result.success);
        let parsed: serde_json::Value =
            serde_json::from_str(&result.output).expect("output should be valid JSON");
        assert!(parsed["task_id"].is_string());
        assert_eq!(parsed["status"], "running");
    }

    #[tokio::test]
    async fn shell_background_task_completes() {
        let registry = test_registry();
        let tool = ShellTool::new(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            Arc::clone(&registry),
        );
        let result = tool
            .execute(json!({"command": "echo done", "background": true}))
            .await
            .expect("background command should return a result");
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let task_id = parsed["task_id"].as_str().unwrap().to_string();

        // Wait a moment for the monitor task to update status.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let task = registry
            .get(&task_id)
            .expect("task should exist in registry");
        assert!(
            matches!(
                task.status,
                BackgroundTaskStatus::Completed { exit_code: 0 }
            ),
            "task should have completed with exit code 0, got {:?}",
            task.status
        );
    }

    #[test]
    fn background_registry_evicts_oldest_completed() {
        let registry = BackgroundTaskRegistry::default();
        // Fill registry with completed tasks.
        for i in 0..MAX_BACKGROUND_TASKS {
            let task = BackgroundTask {
                id: format!("task-{i}"),
                command: format!("cmd-{i}"),
                log_path: PathBuf::from("/dev/null"),
                #[allow(clippy::cast_possible_truncation)]
                pid: 1000 + (i as u32),
                started_at: chrono::Utc::now() + chrono::Duration::seconds(i as i64),
                status: BackgroundTaskStatus::Completed { exit_code: 0 },
            };
            registry.insert(task);
        }
        assert_eq!(registry.list().len(), MAX_BACKGROUND_TASKS);

        // Adding one more should evict the oldest.
        let new_task = BackgroundTask {
            id: "new-task".to_string(),
            command: "new-cmd".to_string(),
            log_path: PathBuf::from("/dev/null"),
            pid: 9999,
            started_at: chrono::Utc::now() + chrono::Duration::seconds(100),
            status: BackgroundTaskStatus::Running,
        };
        registry.insert(new_task);
        assert_eq!(registry.list().len(), MAX_BACKGROUND_TASKS);
        assert!(
            registry.get("task-0").is_none(),
            "oldest task should be evicted"
        );
        assert!(registry.get("new-task").is_some());
    }
}
