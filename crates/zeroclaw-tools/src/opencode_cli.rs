use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_config::schema::OpenCodeCliConfig;

use crate::coding_cli::{
    CodingCliCommand, CodingCliExecutionError, CodingCliExecutor, DirectCodingCliExecutor,
    add_coding_cli_env,
};

pub struct OpenCodeCliTool {
    security: Arc<SecurityPolicy>,
    config: OpenCodeCliConfig,
    executor: Arc<dyn CodingCliExecutor>,
}

impl OpenCodeCliTool {
    /// Construct a standalone tool that executes directly on the host.
    ///
    /// Runtime registries should use `new_with_executor` so the configured
    /// runtime and sandbox own process execution.
    pub fn new(security: Arc<SecurityPolicy>, config: OpenCodeCliConfig) -> Self {
        Self::new_with_executor(security, config, DirectCodingCliExecutor::shared())
    }

    /// Construct the tool with an injected process executor.
    pub fn new_with_executor(
        security: Arc<SecurityPolicy>,
        config: OpenCodeCliConfig,
        executor: Arc<dyn CodingCliExecutor>,
    ) -> Self {
        Self {
            security,
            config,
            executor,
        }
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
        // Rate limiting is applied by the RateLimitedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod).

        // The production wrapper owns accounting; the adapter owns authorization.
        if let Err(error) = self
            .security
            .authorize_tool_operation(ToolOperation::Act, "opencode_cli")
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
                "opencode_cli: missing prompt parameter"
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

        // Build CLI command
        let mut cmd = CodingCliCommand::new("opencode", work_dir.clone(), self.config.timeout_secs);
        cmd.arg("run").arg(prompt);

        add_coding_cli_env(&mut cmd, &self.config.env_passthrough);

        match self.executor.output(cmd).await {
            Ok(output) => {
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
                    output: stdout.into(),
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Err(CodingCliExecutionError::Io(e)) => {
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
                    output: ToolOutput::default(),
                    error: Some(msg),
                })
            }
            Err(CodingCliExecutionError::Timeout) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "OpenCode CLI timed out after {}s and was killed",
                    self.config.timeout_secs
                )),
            }),
            Err(CodingCliExecutionError::Prepare(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to prepare opencode execution: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::OpenCodeCliConfig;

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
        let tool = crate::wrappers::RateLimitedTool::new(
            OpenCodeCliTool::new(security.clone(), test_config()),
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
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("prompt"));
    }

    #[tokio::test]
    async fn opencode_cli_rejects_path_outside_workspace() {
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let outside = tempfile::TempDir::new().expect("temp directory outside workspace");
        let tool = OpenCodeCliTool::new(
            Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Full,
                workspace_dir: workspace.path().to_path_buf(),
                ..SecurityPolicy::default()
            }),
            test_config(),
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
