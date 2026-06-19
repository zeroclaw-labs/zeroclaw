use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_config::schema::ClaudeCodeConfig;

/// Environment variables safe to pass through to the `claude` subprocess.
const SAFE_ENV_VARS: &[&str] = &[
    // Windows system-level variables required for subprocess execution
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "SystemRoot",
    "PATH",
    "HOME",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "USER",
    "SHELL",
    "TMPDIR",
];

/// Delegates coding tasks to the Claude Code CLI (`claude -p`).
///
/// This creates a two-tier agent architecture: ZeroClaw orchestrates high-level
/// tasks and delegates complex coding work to Claude Code, which has its own
/// agent loop with Read/Edit/Bash tools.
///
/// Authentication uses the `claude` binary's own OAuth session (Max subscription)
/// by default. No API key is needed unless `env_passthrough` includes
/// `ANTHROPIC_API_KEY` for API-key billing.
pub struct ClaudeCodeTool {
    security: Arc<SecurityPolicy>,
    config: ClaudeCodeConfig,
}

impl ClaudeCodeTool {
    pub fn new(security: Arc<SecurityPolicy>, config: ClaudeCodeConfig) -> Self {
        Self { security, config }
    }
}

#[async_trait]
impl Tool for ClaudeCodeTool {
    fn name(&self) -> &str {
        "claude_code"
    }

    fn description(&self) -> &str {
        "Delegate a coding task to Claude Code (claude -p). Supports file editing, bash execution, structured output, and multi-turn sessions. Use for complex coding work that benefits from Claude Code's full agent loop."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The coding task to delegate to Claude Code"
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Override the default tool allowlist (e.g. [\"Read\", \"Edit\", \"Bash\", \"Write\"])"
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Override or append a system prompt for this invocation"
                },
                "session_id": {
                    "type": "string",
                    "description": "Resume a previous Claude Code session by its ID"
                },
                "json_schema": {
                    "type": "object",
                    "description": "Request structured output conforming to this JSON Schema"
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

        // Enforce act policy
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "claude_code")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
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
                "claude_code: missing prompt parameter"
            );
            anyhow::Error::msg("Missing 'prompt' parameter")
        })?;

        // Extract optional params
        let allowed_tools: Vec<String> = args
            .get("allowed_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| self.config.allowed_tools.clone());

        let system_prompt = args
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| self.config.system_prompt.clone());

        let session_id = args.get("session_id").and_then(|v| v.as_str());

        let json_schema = args.get("json_schema").filter(|v| v.is_object());

        // Validate working directory — require both paths to exist (reject
        // non-existent paths instead of falling back to the raw value, which
        // could bypass the workspace containment check via symlinks or
        // specially-crafted path components).
        //
        // Relative paths are resolved against workspace_dir (not the process
        // cwd) so that "." or "src" always refers to the configured workspace.
        let work_dir = if let Some(wd) = args.get("working_directory").and_then(|v| v.as_str()) {
            let workspace = &self.security.workspace_dir;
            // Join with workspace_dir if the path is relative; use it as-is if
            // already absolute.  This matches the expected behaviour where a
            // relative working_directory is workspace-relative.
            let wd_path = if std::path::Path::new(wd).is_absolute() {
                std::path::PathBuf::from(wd)
            } else {
                workspace.join(wd)
            };
            let canonical_wd = match wd_path.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
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
                        output: String::new(),
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
                    output: String::new(),
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

        // Build CLI command
        let claude_bin = which::which("claude").unwrap_or_else(|_| {
            if cfg!(target_os = "windows") {
                "claude.cmd".into()
            } else {
                "claude".into()
            }
        });
        let mut cmd = Command::new(claude_bin);
        cmd.arg("-p").arg(prompt);
        cmd.arg("--output-format").arg("json");

        if !allowed_tools.is_empty() {
            for tool in &allowed_tools {
                cmd.arg("--allowedTools").arg(tool);
            }
        }

        if let Some(ref sp) = system_prompt {
            cmd.arg("--append-system-prompt").arg(sp);
        }

        if let Some(sid) = session_id {
            cmd.arg("--resume").arg(sid);
        }

        if let Some(schema) = json_schema {
            let schema_str = serde_json::to_string(schema).unwrap_or_else(|_| "{}".to_string());
            cmd.arg("--json-schema").arg(schema_str);
        }

        // Environment: clear everything, pass only safe vars + configured passthrough.
        // HOME is critical so `claude` finds its OAuth session in ~/.claude/
        cmd.env_clear();
        for var in SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }
        for var in &self.config.env_passthrough {
            let trimmed = var.trim();
            if !trimmed.is_empty()
                && let Ok(val) = std::env::var(trimmed)
            {
                cmd.env(trimmed, val);
            }
        }

        cmd.current_dir(crate::util_helpers::clean_verbatim_path(&work_dir));
        // Execute with timeout — use kill_on_drop(true) so the child process
        // is automatically killed when the future is dropped on timeout,
        // preventing zombie processes.
        let timeout = Duration::from_secs(self.config.timeout_secs);
        cmd.kill_on_drop(true);

        let result = tokio::time::timeout(timeout, cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate to max_output_bytes with char-boundary safety
                if stdout.len() > self.config.max_output_bytes {
                    let mut b = self.config.max_output_bytes.min(stdout.len());
                    while b > 0 && !stdout.is_char_boundary(b) {
                        b -= 1;
                    }
                    stdout.truncate(b);
                    stdout.push_str("\n... [output truncated]");
                }

                // Try to parse JSON response and extract result + session_id
                if let Ok(json_resp) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    let result_text = json_resp
                        .get("result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let resp_session_id = json_resp
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let mut formatted = String::new();
                    if result_text.is_empty() {
                        // Fall back to full JSON if no "result" key
                        formatted.push_str(&stdout);
                    } else {
                        formatted.push_str(result_text);
                    }
                    if !resp_session_id.is_empty() {
                        use std::fmt::Write;
                        let _ = write!(formatted, "\n\n[session_id: {}]", resp_session_id);
                    }

                    Ok(ToolResult {
                        success: output.status.success(),
                        output: formatted,
                        error: if stderr.is_empty() {
                            None
                        } else {
                            Some(stderr)
                        },
                    })
                } else {
                    // JSON parse failed — return raw stdout (defensive)
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
            }
            Ok(Err(e)) => {
                let err_msg = e.to_string();
                let msg = if err_msg.contains("No such file or directory")
                    || err_msg.contains("not found")
                    || err_msg.contains("cannot find")
                {
                    "Claude Code CLI ('claude') not found in PATH. Install with: npm install -g @anthropic-ai/claude-code".into()
                } else {
                    format!("Failed to execute claude: {e}")
                };
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(msg),
                })
            }
            Err(_) => {
                // Timeout — kill_on_drop(true) ensures the child is killed
                // when the future is dropped.
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Claude Code timed out after {}s and was killed",
                        self.config.timeout_secs
                    )),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::ClaudeCodeConfig;

    fn test_config() -> ClaudeCodeConfig {
        ClaudeCodeConfig::default()
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn claude_code_tool_name() {
        let tool = ClaudeCodeTool::new(test_security(AutonomyLevel::Supervised), test_config());
        assert_eq!(tool.name(), "claude_code");
    }

    #[test]
    fn claude_code_tool_schema_has_prompt() {
        let tool = ClaudeCodeTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["prompt"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .expect("schema required should be an array")
                .contains(&json!("prompt"))
        );
        // Optional params exist in properties
        assert!(schema["properties"]["allowed_tools"].is_object());
        assert!(schema["properties"]["system_prompt"].is_object());
        assert!(schema["properties"]["session_id"].is_object());
        assert!(schema["properties"]["json_schema"].is_object());
        assert!(schema["properties"]["working_directory"].is_object());
    }

    #[tokio::test]
    async fn claude_code_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ClaudeCodeTool::new(security, test_config());
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("rate-limited should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn claude_code_blocks_readonly() {
        let tool = ClaudeCodeTool::new(test_security(AutonomyLevel::ReadOnly), test_config());
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
    }

    #[tokio::test]
    async fn claude_code_missing_prompt_param() {
        let tool = ClaudeCodeTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("prompt"));
    }

    #[tokio::test]
    async fn claude_code_rejects_path_outside_workspace() {
        let tool = ClaudeCodeTool::new(test_security(AutonomyLevel::Full), test_config());
        let result = tool
            .execute(json!({
                "prompt": "hello",
                "working_directory": "/etc"
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
    }

    #[tokio::test]
    async fn claude_code_relative_working_directory_resolves_against_workspace() {
        // Create a real subdirectory inside the test workspace so canonicalize
        // succeeds.  The fix ensures the path is joined with workspace_dir first,
        // not resolved from the process cwd.
        let workspace = std::env::temp_dir();
        let subdir = workspace.join("claude-code-test-subdir");
        std::fs::create_dir_all(&subdir).ok();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        });
        let tool = ClaudeCodeTool::new(security, test_config());

        // "relative" is resolved against workspace, not cwd — so
        // workspace/relative exists (after we canonicalise it).
        let result = tool
            .execute(json!({
                "prompt": "hello",
                // subdirectory name that exists inside workspace
                "working_directory": subdir.file_name().unwrap().to_str().unwrap()
            }))
            .await;
        // We expect either success (tool ran) or an error about the binary
        // not being found — NOT "does not exist or is not accessible", which
        // would indicate it looked in the wrong place.
        match result {
            Ok(r) if !r.success => {
                let err = r.error.as_deref().unwrap_or("");
                // Binary-not-found is acceptable; path-not-found is the bug.
                assert!(
                    !err.contains("does not exist or is not accessible"),
                    "relative working_directory was resolved against cwd instead of workspace: {err}"
                );
            }
            Err(e) => {
                // Execution error (binary not found) is fine; canonicalize failure
                // would indicate the wrong base directory.
                assert!(
                    !e.to_string().contains("does not exist"),
                    "relative working_directory was resolved against cwd instead of workspace"
                );
            }
            _ => {}
        }
    }

    #[test]
    fn claude_code_env_passthrough_defaults() {
        let config = ClaudeCodeConfig::default();
        assert!(
            config.env_passthrough.is_empty(),
            "env_passthrough should default to empty (Max subscription needs no API key)"
        );
    }

    #[test]
    fn claude_code_default_config_values() {
        let config = ClaudeCodeConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.timeout_secs, 600);
        assert_eq!(config.max_output_bytes, 2_097_152);
        assert!(config.system_prompt.is_none());
        assert_eq!(config.allowed_tools, vec!["Read", "Edit", "Bash", "Write"]);
    }

    #[test]
    fn safe_env_vars_includes_windows_system_variables() {
        // Verify that SAFE_ENV_VARS contains the Windows system-level
        // variables required for subprocess execution. This test ensures
        // the Windows-specific environment allowlist is not accidentally
        // removed during future cleanup.
        let safe_vars: Vec<&str> = SAFE_ENV_VARS.to_vec();

        // Windows system variables must be present
        assert!(
            safe_vars.contains(&"USERPROFILE"),
            "USERPROFILE must be in SAFE_ENV_VARS for Windows subprocess execution"
        );
        assert!(
            safe_vars.contains(&"APPDATA"),
            "APPDATA must be in SAFE_ENV_VARS for Windows subprocess execution"
        );
        assert!(
            safe_vars.contains(&"LOCALAPPDATA"),
            "LOCALAPPDATA must be in SAFE_ENV_VARS for Windows subprocess execution"
        );
        assert!(
            safe_vars.contains(&"SystemRoot"),
            "SystemRoot must be in SAFE_ENV_VARS for Windows subprocess execution"
        );

        // Standard Unix/common variables must also be present
        assert!(safe_vars.contains(&"PATH"));
        assert!(safe_vars.contains(&"HOME"));
        assert!(safe_vars.contains(&"TERM"));
        assert!(safe_vars.contains(&"LANG"));
        assert!(safe_vars.contains(&"USER"));
    }
}
