use super::traits::{Tool, ToolResult};
use crate::config::OpenCodeCliConfig;
use crate::require_str;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Environment variables safe to pass through to the `opencode` subprocess.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Delegates coding tasks to the OpenCode CLI (`opencode run`).
///
/// This creates a two-tier agent architecture: ZeroClaw orchestrates high-level
/// tasks and delegates complex coding work to OpenCode, which has its own
/// agent loop with file editing and shell tools.
///
/// Authentication uses the `opencode` binary's own session by default. No API
/// key is needed unless `env_passthrough` includes provider-specific keys.
pub struct OpenCodeCliTool {
    security: Arc<SecurityPolicy>,
    config: OpenCodeCliConfig,
}

impl OpenCodeCliTool {
    pub fn new(security: Arc<SecurityPolicy>, config: OpenCodeCliConfig) -> Self {
        Self { security, config }
    }
}

#[async_trait]
impl Tool for OpenCodeCliTool {
    fn name(&self) -> &str {
        "opencode_cli"
    }

    fn description(&self) -> &str {
        "Delegate a coding task to OpenCode CLI (opencode run). Supports file editing and bash execution. Use for complex coding work that benefits from OpenCode's full agent loop."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The coding task to delegate to OpenCode"
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
            .enforce_tool_operation(ToolOperation::Act, "opencode_cli")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // Extract prompt (required)
        let prompt = require_str!(args, "prompt");

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

        // Build CLI command
        let mut cmd = Command::new("opencode");
        cmd.arg("run").arg(prompt);

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
                    "OpenCode CLI ('opencode') not found in PATH. Install with: go install github.com/opencode-ai/opencode@latest".into()
                } else {
                    format!("Failed to execute opencode: {e}")
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
                        "OpenCode CLI timed out after {}s and was killed",
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
    use crate::config::OpenCodeCliConfig;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_config() -> OpenCodeCliConfig {
        OpenCodeCliConfig::default()
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn opencode_cli_tool_name() {
        let tool = OpenCodeCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        assert_eq!(tool.name(), "opencode_cli");
    }

    #[test]
    fn opencode_cli_tool_schema_has_prompt() {
        let tool = OpenCodeCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
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
    async fn opencode_cli_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = OpenCodeCliTool::new(security, test_config());
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("rate-limited should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn opencode_cli_blocks_readonly() {
        let tool = OpenCodeCliTool::new(test_security(AutonomyLevel::ReadOnly), test_config());
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
    async fn opencode_cli_missing_prompt_param() {
        let tool = OpenCodeCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("prompt"));
    }

    #[tokio::test]
    async fn opencode_cli_rejects_path_outside_workspace() {
        let tool = OpenCodeCliTool::new(test_security(AutonomyLevel::Full), test_config());
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
    fn opencode_cli_env_passthrough_defaults() {
        let config = OpenCodeCliConfig::default();
        assert!(
            config.env_passthrough.is_empty(),
            "env_passthrough should default to empty"
        );
    }

    #[test]
    fn opencode_cli_default_config_values() {
        let config = OpenCodeCliConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.timeout_secs, 600);
        assert_eq!(config.max_output_bytes, 2_097_152);
    }
}
