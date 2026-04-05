use super::traits::{Tool, ToolResult};
use crate::config::CodexCliConfig;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Environment variables safe to pass through to the `codex` subprocess.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Delegates coding tasks to the Codex CLI (`codex exec`).
///
/// This creates a two-tier agent architecture: ZeroClaw orchestrates high-level
/// tasks and delegates complex coding work to Codex, which has its own
/// agent loop with file editing and shell tools.
///
/// Authentication uses the `codex` binary's own session (`~/.codex/auth.json`)
/// by default, supporting both API key and ChatGPT OAuth flows. No API key
/// is needed unless `env_passthrough` includes `OPENAI_API_KEY`.
pub struct CodexCliTool {
    security: Arc<SecurityPolicy>,
    config: CodexCliConfig,
}

impl CodexCliTool {
    pub fn new(security: Arc<SecurityPolicy>, config: CodexCliConfig) -> Self {
        Self { security, config }
    }
}

#[async_trait]
impl Tool for CodexCliTool {
    fn name(&self) -> &str {
        "codex_cli"
    }

    fn description(&self) -> &str {
        "Delegate a coding task to Codex CLI (codex exec). Supports file editing and bash execution. Use for complex coding work that benefits from Codex's full agent loop."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The coding task to delegate to Codex"
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
        // Rate limit check
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // Enforce act policy
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "codex_cli")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // Extract prompt (required)
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

        // Validate working directory — require both paths to exist (reject
        // non-existent paths instead of falling back to the raw value, which
        // could bypass the workspace containment check via symlinks or
        // specially-crafted path components).
        let work_dir = if let Some(wd) = args.get("working_directory").and_then(|v| v.as_str()) {
            let wd_path = std::path::PathBuf::from(wd);
            let workspace = &self.security.workspace_dir;
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

        // Record action budget
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Build CLI command: `codex exec [extra_args...] <prompt>`
        let codex_bin = if cfg!(target_os = "windows") {
            "codex.cmd"
        } else {
            "codex"
        };
        let mut cmd = Command::new(codex_bin);
        cmd.arg("exec");

        // Append user-configured extra arguments (e.g. --sandbox, --skip-git-repo-check)
        for arg in &self.config.extra_args {
            let trimmed = arg.trim();
            if !trimmed.is_empty() {
                cmd.arg(trimmed);
            }
        }

        cmd.arg(prompt);

        // Environment: clear everything, pass only safe vars + configured passthrough.
        cmd.env_clear();
        for var in SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }
        for var in &self.config.env_passthrough {
            let trimmed = var.trim();
            if !trimmed.is_empty() {
                if let Ok(val) = std::env::var(trimmed) {
                    cmd.env(trimmed, val);
                }
            }
        }

        cmd.current_dir(&work_dir);
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
            Ok(Err(e)) => {
                let err_msg = e.to_string();
                let msg = if err_msg.contains("No such file or directory")
                    || err_msg.contains("not found")
                    || err_msg.contains("cannot find")
                {
                    "Codex CLI ('codex') not found in PATH. Install with: npm install -g @openai/codex".into()
                } else {
                    format!("Failed to execute codex: {e}")
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
                        "Codex CLI timed out after {}s and was killed",
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
    use crate::config::CodexCliConfig;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_config() -> CodexCliConfig {
        CodexCliConfig::default()
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn codex_cli_tool_name() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        assert_eq!(tool.name(), "codex_cli");
    }

    #[test]
    fn codex_cli_tool_schema_has_prompt() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
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

    #[tokio::test]
    async fn codex_cli_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = CodexCliTool::new(security, test_config());
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("rate-limited should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn codex_cli_blocks_readonly() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::ReadOnly), test_config());
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
    async fn codex_cli_missing_prompt_param() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("prompt"));
    }

    #[tokio::test]
    async fn codex_cli_rejects_path_outside_workspace() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::Full), test_config());
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

    #[test]
    fn codex_cli_env_passthrough_defaults() {
        let config = CodexCliConfig::default();
        assert!(
            config.env_passthrough.is_empty(),
            "env_passthrough should default to empty"
        );
    }

    #[test]
    fn codex_cli_extra_args_defaults() {
        let config = CodexCliConfig::default();
        assert!(
            config.extra_args.is_empty(),
            "extra_args should default to empty"
        );
    }

    #[test]
    fn codex_cli_default_config_values() {
        let config = CodexCliConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.timeout_secs, 600);
        assert_eq!(config.max_output_bytes, 2_097_152);
    }
}
