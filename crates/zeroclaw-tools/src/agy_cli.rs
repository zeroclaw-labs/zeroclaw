use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_config::schema::AgyCliConfig;

use crate::coding_cli::{
    CodingCliCommand, CodingCliExecutionError, CodingCliExecutor, DirectCodingCliExecutor,
    add_coding_cli_env,
};

/// Seconds reserved between agy's own `--print-timeout` and ZeroClaw's hard
/// kill, so agy can flush its partial-result JSON before the process is
/// killed.
const AGY_PRINT_TIMEOUT_MARGIN_SECS: u64 = 10;

/// The `status` value agy reports for a completed turn.
const AGY_STATUS_SUCCESS: &str = "SUCCESS";

/// The subset of agy's `--output-format json` result that decides success.
///
/// agy exits 0 even when a turn failed or every tool call was auto-denied, so
/// this object is the only reliable completion signal.
#[derive(Debug, Deserialize)]
struct AgyPrintResult {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    response: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    denied_actions: Vec<AgyDeniedAction>,
}

#[derive(Debug, Deserialize)]
struct AgyDeniedAction {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

impl AgyDeniedAction {
    fn label(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.action.as_deref())
            .unwrap_or("unknown")
    }
}

pub struct AgyCliTool {
    security: Arc<SecurityPolicy>,
    config: AgyCliConfig,
    executor: Arc<dyn CodingCliExecutor>,
}

impl AgyCliTool {
    /// Construct a standalone tool that executes directly on the host.
    ///
    /// Runtime registries should use `new_with_executor` so the configured
    /// runtime and sandbox own process execution.
    pub fn new(security: Arc<SecurityPolicy>, config: AgyCliConfig) -> Self {
        Self::new_with_executor(security, config, DirectCodingCliExecutor::shared())
    }

    /// Construct the tool with an injected process executor.
    pub fn new_with_executor(
        security: Arc<SecurityPolicy>,
        config: AgyCliConfig,
        executor: Arc<dyn CodingCliExecutor>,
    ) -> Self {
        Self {
            security,
            config,
            executor,
        }
    }
}

/// agy's own print-mode limit, kept below ZeroClaw's hard kill so the
/// partial-result JSON still reaches stdout.
fn agy_print_timeout_secs(timeout_secs: u64) -> u64 {
    timeout_secs
        .saturating_sub(AGY_PRINT_TIMEOUT_MARGIN_SECS)
        .max(1)
}

/// Build `agy [extra_args...] --output-format=json --disable-slash-commands
/// --print-timeout=<N>s [--model=<id>] --print=<prompt>`.
///
/// Operator `extra_args` come first so ZeroClaw's flags win if they repeat
/// one. The prompt is attached to `--print=` so a model-supplied prompt that
/// starts with `-` can never be parsed as a flag.
fn agy_print_args(config: &AgyCliConfig, prompt: &str) -> Vec<String> {
    let mut args: Vec<String> = config
        .effective_extra_args()
        .map(|(_, arg)| arg.to_string())
        .collect();

    args.push("--output-format=json".into());
    args.push("--disable-slash-commands".into());
    args.push(format!(
        "--print-timeout={}s",
        agy_print_timeout_secs(config.timeout_secs)
    ));
    if let Some(model) = config
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        args.push(format!("--model={model}"));
    }
    args.push(format!("--print={prompt}"));

    args
}

/// Truncate to `max_bytes` on a char boundary, marking the cut.
fn truncate_output(mut text: String, max_bytes: usize) -> String {
    if text.len() > max_bytes {
        let mut b = max_bytes.min(text.len());
        while b > 0 && !text.is_char_boundary(b) {
            b -= 1;
        }
        text.truncate(b);
        text.push_str("\n... [output truncated]");
    }
    text
}

/// Map agy's JSON result to a tool result.
///
/// Success requires a parseable result with `status == "SUCCESS"` and no
/// `denied_actions`. Anything else fails closed, whatever the exit code.
fn map_agy_output(
    exit_success: bool,
    stdout: &str,
    stderr: &str,
    max_output_bytes: usize,
) -> ToolResult {
    let stderr = stderr.trim();
    let with_stderr = |message: String| -> String {
        if stderr.is_empty() {
            message
        } else {
            format!("{message}\n{stderr}")
        }
    };

    let parsed = match serde_json::from_str::<AgyPrintResult>(stdout.trim()) {
        Ok(parsed) => parsed,
        Err(_) => {
            return ToolResult {
                success: false,
                output: truncate_output(stdout.to_string(), max_output_bytes).into(),
                error: Some(with_stderr(
                    "Antigravity CLI did not return a JSON result; treating the run as failed"
                        .into(),
                )),
            };
        }
    };

    let response = truncate_output(parsed.response, max_output_bytes);
    let status = parsed.status.as_deref().unwrap_or("");

    if status != AGY_STATUS_SUCCESS {
        let detail = parsed
            .error
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(|e| format!(": {e}"))
            .unwrap_or_default();
        let status = if status.is_empty() { "missing" } else { status };
        return ToolResult {
            success: false,
            output: response.into(),
            error: Some(with_stderr(format!(
                "Antigravity CLI reported status {status}{detail}"
            ))),
        };
    }

    if !parsed.denied_actions.is_empty() {
        let denied = parsed
            .denied_actions
            .iter()
            .map(AgyDeniedAction::label)
            .collect::<Vec<_>>()
            .join(", ");
        return ToolResult {
            success: false,
            output: response.into(),
            error: Some(with_stderr(format!(
                "Antigravity CLI auto-denied tool permissions in headless mode ({denied}). \
                 To let the task proceed, allow them under permissions.allow in agy's settings.json, \
                 or have the ZeroClaw operator adjust agy_cli.extra_args in the ZeroClaw config \
                 (remove --sandbox, which denies every shell command, or add \
                 --dangerously-skip-permissions to auto-approve all tools)."
            ))),
        };
    }

    if !exit_success {
        return ToolResult {
            success: false,
            output: response.into(),
            error: Some(with_stderr(
                "Antigravity CLI exited with a failure status".into(),
            )),
        };
    }

    ToolResult {
        success: true,
        output: response.into(),
        error: if stderr.is_empty() {
            None
        } else {
            Some(stderr.to_string())
        },
    }
}

#[async_trait]
impl Tool for AgyCliTool {
    fn name(&self) -> &str {
        "agy_cli"
    }

    fn description(&self) -> &str {
        "Delegate a coding task to Google's Antigravity CLI (agy --print). Supports file editing and command execution within the permissions allowed in agy's own settings. Use for complex coding work that benefits from Antigravity's full agent loop."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The coding task to delegate to Antigravity CLI"
                },
                "working_directory": {
                    "type": "string",
                    "description": "Working directory within the workspace (must be inside workspace_dir)"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Rate limiting is applied by the RateLimitedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod).

        // The production wrapper owns accounting; the adapter owns authorization.
        if let Err(error) = self
            .security
            .authorize_tool_operation(ToolOperation::Act, "agy_cli")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        // Extract prompt (required)
        let prompt = args.get("prompt").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "prompt"})),
                "agy_cli: missing prompt parameter"
            );
            anyhow::Error::msg("Missing 'prompt' parameter")
        })?;

        // Validate working directory — require both paths to exist (reject
        // non-existent paths instead of falling back to the raw value, which
        // could bypass the workspace containment check via symlinks or
        // specially-crafted path components).
        let work_dir = if let Some(wd) = args.get("working_directory").and_then(|v| v.as_str()) {
            let wd_path = std::path::PathBuf::from(wd);
            let wd_path = if wd_path.is_relative() {
                self.security.workspace_dir.join(&wd_path)
            } else {
                wd_path
            };
            let workspace = &self.security.workspace_dir;
            let canonical_wd = match wd_path.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!(
                            "working_directory '{}' does not exist or is not accessible",
                            wd
                        )),
                    });
                }
            };
            let canonical_ws = match workspace.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!(
                            "workspace directory '{}' does not exist or is not accessible",
                            workspace.display()
                        )),
                    });
                }
            };
            if !canonical_wd.starts_with(&canonical_ws) {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "working_directory '{}' is outside the workspace '{}'",
                        wd,
                        workspace.display()
                    )),
                });
            }
            canonical_wd
        } else {
            self.security.workspace_dir.clone()
        };

        let mut cmd = CodingCliCommand::new("agy", work_dir, self.config.timeout_secs);
        cmd.args(agy_print_args(&self.config, prompt));

        add_coding_cli_env(&mut cmd, &self.config.env_passthrough);

        match self.executor.output(cmd).await {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                Ok(map_agy_output(
                    output.status.success(),
                    &stdout,
                    &stderr,
                    self.config.max_output_bytes,
                ))
            }
            Err(CodingCliExecutionError::Io(e)) => {
                let err_msg = e.to_string();
                let msg = if err_msg.contains("No such file or directory")
                    || err_msg.contains("not found")
                    || err_msg.contains("cannot find")
                {
                    "Antigravity CLI ('agy') not found in PATH. Install with: curl -fsSL https://antigravity.google/cli/install.sh | bash".into()
                } else {
                    format!("Failed to execute agy: {e}")
                };
                Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(msg),
                })
            }
            Err(CodingCliExecutionError::Timeout) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Antigravity CLI timed out after {}s and was killed",
                    self.config.timeout_secs
                )),
            }),
            Err(CodingCliExecutionError::Prepare(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to prepare agy execution: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{ExitStatus, Output};
    use std::sync::Mutex;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::AgyCliConfig;

    fn test_config() -> AgyCliConfig {
        AgyCliConfig::default()
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code as u32)
    }

    /// Records every command and replies with a fixed process result.
    struct ScriptedExecutor {
        commands: Mutex<Vec<CodingCliCommand>>,
        exit_code: i32,
        stdout: &'static str,
        stderr: &'static str,
    }

    impl ScriptedExecutor {
        fn new(exit_code: i32, stdout: &'static str, stderr: &'static str) -> Arc<Self> {
            Arc::new(Self {
                commands: Mutex::new(Vec::new()),
                exit_code,
                stdout,
                stderr,
            })
        }

        fn only_command(&self) -> CodingCliCommand {
            let commands = self.commands.lock().expect("command lock");
            assert_eq!(commands.len(), 1, "expected exactly one agy invocation");
            commands[0].clone()
        }
    }

    #[async_trait]
    impl CodingCliExecutor for ScriptedExecutor {
        async fn output(
            &self,
            command: CodingCliCommand,
        ) -> Result<Output, CodingCliExecutionError> {
            self.commands.lock().expect("command lock").push(command);
            Ok(Output {
                status: exit_status(self.exit_code),
                stdout: self.stdout.as_bytes().to_vec(),
                stderr: self.stderr.as_bytes().to_vec(),
            })
        }
    }

    fn scripted_tool(config: AgyCliConfig, executor: Arc<ScriptedExecutor>) -> AgyCliTool {
        AgyCliTool::new_with_executor(test_security(AutonomyLevel::Full), config, executor)
    }

    fn args_of(command: &CodingCliCommand) -> Vec<String> {
        command
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    // Result shapes observed from agy 1.2.9 `--output-format json`.
    const AGY_SUCCESS: &str = r#"{"conversation_id":"c1","status":"SUCCESS","response":"ALPHA\n","duration_seconds":1.9,"num_turns":1,"usage":{"input_tokens":11828,"output_tokens":114,"thinking_tokens":113,"cache_read_tokens":0,"total_tokens":11942}}"#;
    const AGY_DENIED: &str = r#"{"conversation_id":"c2","status":"SUCCESS","response":"","duration_seconds":4.3,"num_turns":1,"usage":{"input_tokens":12572,"output_tokens":817,"thinking_tokens":760,"cache_read_tokens":0,"total_tokens":13389},"denied_actions":[{"action":"command","display_name":"RunCommand"}]}"#;
    const AGY_DENIED_STDERR: &str = "jetski: no output produced — a tool required the \"command\" permission that headless mode cannot prompt for, so it was auto-denied.";
    const AGY_ERROR: &str = r#"{"conversation_id":"","status":"ERROR","response":"","error":"invalid model selection (--model \"no-such-model\")","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0}}"#;

    #[test]
    fn agy_cli_tool_name() {
        let tool = AgyCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        assert_eq!(tool.name(), "agy_cli");
    }

    #[test]
    fn agy_cli_tool_schema_has_prompt() {
        let tool = AgyCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["prompt"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .expect("schema required should be an array")
                .contains(&json!("prompt"))
        );
        assert!(schema["properties"]["working_directory"].is_object());
    }

    #[test]
    fn agy_cli_default_config_values() {
        let config = AgyCliConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.timeout_secs, 600);
        assert_eq!(config.max_output_bytes, 2_097_152);
        assert!(config.env_passthrough.is_empty());
        assert!(config.model.is_none());
        assert!(config.extra_args.is_empty());
    }

    #[test]
    fn agy_print_args_default_shape() {
        assert_eq!(
            agy_print_args(&test_config(), "fix the bug"),
            vec![
                "--output-format=json",
                "--disable-slash-commands",
                "--print-timeout=590s",
                "--print=fix the bug",
            ]
        );
    }

    #[test]
    fn agy_print_args_place_operator_args_first_and_pin_model() {
        let config = AgyCliConfig {
            model: Some("  gemini-3.8-flash-high ".into()),
            extra_args: vec!["--sandbox".into(), "  ".into()],
            ..test_config()
        };
        assert_eq!(
            agy_print_args(&config, "task"),
            vec![
                "--sandbox",
                "--output-format=json",
                "--disable-slash-commands",
                "--print-timeout=590s",
                "--model=gemini-3.8-flash-high",
                "--print=task",
            ]
        );
    }

    #[test]
    fn agy_print_args_skip_blank_model() {
        let config = AgyCliConfig {
            model: Some("   ".into()),
            ..test_config()
        };
        assert!(
            !agy_print_args(&config, "task")
                .iter()
                .any(|a| a.starts_with("--model"))
        );
    }

    #[test]
    fn agy_print_args_keep_dash_prompt_attached_to_print() {
        let args = agy_print_args(&test_config(), "--dangerously-skip-permissions");
        assert_eq!(
            args.last().map(String::as_str),
            Some("--print=--dangerously-skip-permissions")
        );
        assert!(
            !args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "a model-supplied prompt must never become a standalone flag"
        );
    }

    #[test]
    fn agy_print_timeout_stays_below_the_hard_kill() {
        assert_eq!(agy_print_timeout_secs(600), 590);
        assert_eq!(agy_print_timeout_secs(11), 1);
        assert_eq!(agy_print_timeout_secs(5), 1);
        assert_eq!(agy_print_timeout_secs(0), 1);
    }

    #[test]
    fn map_success_returns_response_only() {
        let result = map_agy_output(true, AGY_SUCCESS, "", 1024);
        assert!(result.success, "{result:?}");
        assert_eq!(result.output.to_string(), "ALPHA\n");
        assert!(result.error.is_none());
    }

    #[test]
    fn map_denied_actions_fail_despite_success_status_and_exit_zero() {
        let result = map_agy_output(true, AGY_DENIED, AGY_DENIED_STDERR, 1024);
        assert!(!result.success, "denied actions must not report success");
        let error = result.error.expect("denial error");
        assert!(error.contains("RunCommand"), "{error}");
        assert!(error.contains("permissions.allow"), "{error}");
        assert!(error.contains("agy_cli.extra_args"), "{error}");
        assert!(error.contains("auto-denied"), "{error}");
    }

    #[test]
    fn map_error_status_fails_with_agy_error_detail() {
        let result = map_agy_output(true, AGY_ERROR, "", 1024);
        assert!(!result.success);
        let error = result.error.expect("status error");
        assert!(error.contains("status ERROR"), "{error}");
        assert!(error.contains("invalid model selection"), "{error}");
    }

    #[test]
    fn map_missing_status_fails_closed() {
        let result = map_agy_output(true, r#"{"response":"looks fine"}"#, "", 1024);
        assert!(!result.success);
        assert!(result.error.expect("error").contains("status missing"));
    }

    #[test]
    fn map_error_status_surfaces_stderr_diagnostics() {
        let result = map_agy_output(false, AGY_ERROR, "auth token expired", 1024);
        assert!(!result.success);
        let error = result.error.expect("status error");
        assert!(error.contains("status ERROR"), "{error}");
        assert!(error.contains("auth token expired"), "{error}");
    }

    #[test]
    fn map_non_json_output_fails_closed() {
        let result = map_agy_output(true, "plain text answer", "boom", 1024);
        assert!(!result.success);
        let error = result.error.expect("parse error");
        assert!(error.contains("did not return a JSON result"), "{error}");
        assert!(error.contains("boom"), "{error}");
        assert_eq!(result.output.to_string(), "plain text answer");
    }

    #[test]
    fn map_success_json_with_failed_exit_still_fails() {
        let result = map_agy_output(false, AGY_SUCCESS, "", 1024);
        assert!(!result.success);
        assert!(result.error.expect("exit error").contains("failure status"));
    }

    #[test]
    fn map_truncates_response_on_char_boundary() {
        let json = r#"{"status":"SUCCESS","response":"ääääää"}"#;
        let result = map_agy_output(true, json, "", 5);
        assert!(result.success);
        assert_eq!(result.output.to_string(), "ää\n... [output truncated]");
    }

    #[tokio::test]
    async fn agy_cli_runs_in_validated_workspace_and_maps_success() {
        let executor = ScriptedExecutor::new(0, AGY_SUCCESS, "");
        let tool = scripted_tool(test_config(), executor.clone());
        let result = tool
            .execute(json!({"prompt": "say alpha"}))
            .await
            .expect("tool result");
        assert!(result.success, "{result:?}");
        assert_eq!(result.output.to_string(), "ALPHA\n");

        let command = executor.only_command();
        assert_eq!(command.program, "agy");
        assert_eq!(command.working_dir, std::env::temp_dir());
        assert_eq!(command.timeout_secs, 600);
        assert_eq!(
            args_of(&command).last().map(String::as_str),
            Some("--print=say alpha")
        );
    }

    #[tokio::test]
    async fn agy_cli_reports_denials_as_failure_end_to_end() {
        let executor = ScriptedExecutor::new(0, AGY_DENIED, AGY_DENIED_STDERR);
        let tool = scripted_tool(test_config(), executor);
        let result = tool
            .execute(json!({"prompt": "run ls"}))
            .await
            .expect("tool result");
        assert!(!result.success);
        assert!(result.error.expect("error").contains("RunCommand"));
    }

    #[tokio::test]
    async fn agy_cli_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = crate::wrappers::RateLimitedTool::new(
            AgyCliTool::new(security.clone(), test_config()),
            security,
        );
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("rate-limited should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn agy_cli_blocks_readonly() {
        let executor = ScriptedExecutor::new(0, AGY_SUCCESS, "");
        let tool = AgyCliTool::new_with_executor(
            test_security(AutonomyLevel::ReadOnly),
            test_config(),
            executor.clone(),
        );
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("readonly should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("read-only mode")
        );
        assert!(
            executor.commands.lock().expect("command lock").is_empty(),
            "read-only mode must not launch agy"
        );
    }

    #[tokio::test]
    async fn agy_cli_missing_prompt_param() {
        let tool = AgyCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("prompt"));
    }

    #[tokio::test]
    async fn agy_cli_resolves_relative_working_directory_under_workspace() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        std::fs::create_dir(workspace.path().join("sub")).expect("relative working directory");
        let executor = ScriptedExecutor::new(0, AGY_SUCCESS, "");
        let tool = AgyCliTool::new_with_executor(
            Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Full,
                workspace_dir: workspace.path().to_path_buf(),
                ..SecurityPolicy::default()
            }),
            test_config(),
            executor.clone(),
        );
        let result = tool
            .execute(json!({"prompt": "hello", "working_directory": "sub"}))
            .await
            .expect("relative working directory inside the workspace should run");
        assert!(result.success, "{:?}", result.error);
        assert_eq!(
            executor.only_command().working_dir,
            workspace
                .path()
                .join("sub")
                .canonicalize()
                .expect("canonical sub")
        );
    }

    #[tokio::test]
    async fn agy_cli_rejects_path_outside_workspace() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let outside = tempfile::TempDir::new().expect("temp directory outside workspace");
        let executor = ScriptedExecutor::new(0, AGY_SUCCESS, "");
        let tool = AgyCliTool::new_with_executor(
            Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Full,
                workspace_dir: workspace.path().to_path_buf(),
                ..SecurityPolicy::default()
            }),
            test_config(),
            executor.clone(),
        );
        let result = tool
            .execute(json!({
                "prompt": "hello",
                "working_directory": outside.path()
            }))
            .await
            .expect("should return a result for path validation");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("outside the workspace")
        );
        assert!(executor.commands.lock().expect("command lock").is_empty());
    }

    #[tokio::test]
    async fn agy_cli_reports_timeout() {
        struct TimeoutExecutor;
        #[async_trait]
        impl CodingCliExecutor for TimeoutExecutor {
            async fn output(
                &self,
                _command: CodingCliCommand,
            ) -> Result<Output, CodingCliExecutionError> {
                Err(CodingCliExecutionError::Timeout)
            }
        }
        let tool = AgyCliTool::new_with_executor(
            test_security(AutonomyLevel::Full),
            AgyCliConfig {
                timeout_secs: 42,
                ..test_config()
            },
            Arc::new(TimeoutExecutor),
        );
        let result = tool
            .execute(json!({"prompt": "slow"}))
            .await
            .expect("tool result");
        assert!(!result.success);
        assert!(
            result
                .error
                .expect("timeout error")
                .contains("timed out after 42s")
        );
    }

    #[tokio::test]
    async fn agy_cli_reports_missing_binary() {
        struct MissingExecutor;
        #[async_trait]
        impl CodingCliExecutor for MissingExecutor {
            async fn output(
                &self,
                _command: CodingCliCommand,
            ) -> Result<Output, CodingCliExecutionError> {
                Err(CodingCliExecutionError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "No such file or directory (os error 2)",
                )))
            }
        }
        let tool = AgyCliTool::new_with_executor(
            test_security(AutonomyLevel::Full),
            test_config(),
            Arc::new(MissingExecutor),
        );
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("tool result");
        assert!(!result.success);
        assert!(
            result
                .error
                .expect("not-found error")
                .contains("'agy') not found in PATH")
        );
    }
}
