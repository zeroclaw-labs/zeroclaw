use super::traits::{Tool, ToolResult};
use crate::config::ClaudeCodeRunnerConfig;
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use arc_swap::ArcSwap;
use std::sync::Arc;
use tokio::process::Command;

/// Environment variables safe to pass through to the `claude` subprocess.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Event payload received from Claude Code HTTP hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeCodeHookEvent {
    /// The session identifier (matches the tmux session name suffix).
    pub session_id: String,
    /// Event type from Claude Code (e.g. "tool_use", "tool_result", "completion").
    pub event_type: String,
    /// Tool name when event_type is "tool_use" or "tool_result".
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Human-readable summary of what happened.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Spawns Claude Code inside a tmux session with HTTP hooks that POST tool
/// execution events back to ZeroClaw's gateway endpoint, enabling live Slack
/// progress updates and SSH session handoff.
///
/// Unlike [`ClaudeCodeTool`](super::claude_code::ClaudeCodeTool) which runs
/// `claude -p` inline and waits for completion, this runner:
///
/// 1. Creates a named tmux session (`<prefix><id>`)
/// 2. Launches `claude` inside it with `--hook-url` pointing at the gateway
/// 3. Returns immediately with the session ID and an SSH attach command
/// 4. Receives streamed progress via the `/hooks/claude-code` endpoint
pub struct ClaudeCodeRunnerTool {
    security: Arc<ArcSwap<SecurityPolicy>>,
    config: ClaudeCodeRunnerConfig,
    /// Base URL of the ZeroClaw gateway (e.g. "http://localhost:3000").
    gateway_url: String,
}

impl ClaudeCodeRunnerTool {
    pub fn new(
        security: Arc<ArcSwap<SecurityPolicy>>,
        config: ClaudeCodeRunnerConfig,
        gateway_url: String,
    ) -> Self {
        Self {
            security,
            config,
            gateway_url,
        }
    }

    /// Build the tmux session name from the configured prefix and a unique id.
    fn session_name(&self, id: &str) -> String {
        format!("{}{}", self.config.tmux_prefix, id)
    }

    /// Build the SSH attach command for session handoff.
    fn ssh_attach_command(&self, session_name: &str) -> Option<String> {
        self.config
            .ssh_host
            .as_ref()
            .map(|host| format!("ssh -t {host} tmux attach-session -t {session_name}"))
    }
}

#[async_trait]
impl Tool for ClaudeCodeRunnerTool {
    fn name(&self) -> &str {
        "claude_code_runner"
    }

    fn description(&self) -> &str {
        "Spawn a Claude Code task in a tmux session with live Slack progress updates and SSH handoff. Returns immediately with session ID and attach command."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The coding task to delegate to Claude Code"
                },
                "working_directory": {
                    "type": "string",
                    "description": "Working directory within the workspace (must be inside workspace_dir)"
                },
                "slack_channel": {
                    "type": "string",
                    "description": "Slack channel ID to post progress updates to"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Rate limit check
        if self.security.load().is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // Enforce act policy
        if let Err(error) = self
            .security
            .load()
            .enforce_tool_operation(ToolOperation::Act, "claude_code_runner")
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

        // Validate working directory
        let work_dir = if let Some(wd) = args.get("working_directory").and_then(|v| v.as_str()) {
            let wd_path = std::path::PathBuf::from(wd);
            let workspace = &self.security.load().workspace_dir;
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
            self.security.load().workspace_dir.clone()
        };

        let slack_channel = args
            .get("slack_channel")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Record action budget
        if !self.security.load().record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Generate a unique session ID
        let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let session_name = self.session_name(&session_id);

        // Build the hook URL for Claude Code to POST events to
        let hook_url = format!("{}/hooks/claude-code", self.gateway_url);

        // Build the claude command that will run inside tmux
        let mut claude_args = vec![
            "claude".to_string(),
            "-p".to_string(),
            prompt.to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ];

        // Pass hook URL via environment variable (Claude Code uses
        // CLAUDE_CODE_HOOK_URL when --hook-url is not available).
        // We also append --hook-url for newer CLI versions.
        claude_args.push("--hook-url".to_string());
        claude_args.push(hook_url.clone());

        // Build env string for tmux send-keys
        let mut env_exports = String::new();
        for var in SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                use std::fmt::Write;
                let _ = write!(env_exports, "{}={} ", var, shell_escape(&val));
            }
        }
        // Pass session metadata via env vars so the hook can correlate events
        use std::fmt::Write;
        let _ = write!(env_exports, "CLAUDE_CODE_SESSION_ID={} ", &session_id);
        if let Some(ref ch) = slack_channel {
            let _ = write!(env_exports, "CLAUDE_CODE_SLACK_CHANNEL={} ", ch);
        }
        let _ = write!(env_exports, "CLAUDE_CODE_HOOK_URL={} ", &hook_url);

        // Create tmux session
        let create_result = Command::new("tmux")
            .args(["new-session", "-d", "-s", &session_name])
            .arg("-c")
            .arg(work_dir.to_str().unwrap_or("."))
            .output()
            .await;

        match create_result {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to create tmux session: {stderr}")),
                });
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "tmux not found or failed to execute: {e}. Install tmux to use claude_code_runner."
                    )),
                });
            }
            _ => {}
        }

        // Send the claude command into the tmux session
        let full_command = format!(
            "{env_exports}{cmd}",
            env_exports = env_exports,
            cmd = claude_args
                .iter()
                .map(|a| shell_escape(a))
                .collect::<Vec<_>>()
                .join(" ")
        );

        let send_result = Command::new("tmux")
            .args(["send-keys", "-t", &session_name, &full_command, "Enter"])
            .output()
            .await;

        if let Err(e) = send_result {
            // Clean up the session we just created
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &session_name])
                .output()
                .await;
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to send command to tmux session: {e}")),
            });
        }

        // Schedule session TTL cleanup
        let ttl = self.config.session_ttl;
        let cleanup_session = session_name.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(ttl)).await;
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &cleanup_session])
                .output()
                .await;
            tracing::info!(
                session = cleanup_session,
                "Claude Code runner session TTL expired, cleaned up"
            );
        });

        // Build response
        let mut output_parts = vec![
            format!("Session started: {session_name}"),
            format!("Session ID: {session_id}"),
            format!("Hook URL: {hook_url}"),
        ];

        if let Some(ssh_cmd) = self.ssh_attach_command(&session_name) {
            output_parts.push(format!("SSH attach: {ssh_cmd}"));
        } else {
            output_parts.push(format!(
                "Local attach: tmux attach-session -t {session_name}"
            ));
        }

        if let Some(ref ch) = slack_channel {
            output_parts.push(format!("Slack channel: {ch} (progress updates enabled)"));
        }

        Ok(ToolResult {
            success: true,
            output: output_parts.join("\n"),
            error: None,
        })
    }
}

/// Minimal shell escaping for values embedded in tmux send-keys.
fn shell_escape(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '+'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClaudeCodeRunnerConfig;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_config() -> ClaudeCodeRunnerConfig {
        ClaudeCodeRunnerConfig {
            enabled: true,
            ssh_host: Some("dev.example.com".into()),
            tmux_prefix: "zc-test-".into(),
            session_ttl: 3600,
        }
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<ArcSwap<SecurityPolicy>> {
        Arc::new(ArcSwap::from_pointee(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        }))
    }

    #[test]
    fn tool_name() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Supervised),
            test_config(),
            "http://localhost:3000".into(),
        );
        assert_eq!(tool.name(), "claude_code_runner");
    }

    #[test]
    fn tool_schema_has_prompt() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Supervised),
            test_config(),
            "http://localhost:3000".into(),
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["required"]
            .as_array()
            .expect("required should be an array")
            .contains(&json!("prompt")));
    }

    #[test]
    fn session_name_uses_prefix() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Supervised),
            test_config(),
            "http://localhost:3000".into(),
        );
        let name = tool.session_name("abc123");
        assert_eq!(name, "zc-test-abc123");
    }

    #[test]
    fn ssh_attach_command_with_host() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Supervised),
            test_config(),
            "http://localhost:3000".into(),
        );
        let cmd = tool.ssh_attach_command("zc-test-abc123");
        assert_eq!(
            cmd.as_deref(),
            Some("ssh -t dev.example.com tmux attach-session -t zc-test-abc123")
        );
    }

    #[test]
    fn ssh_attach_command_without_host() {
        let mut config = test_config();
        config.ssh_host = None;
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Supervised),
            config,
            "http://localhost:3000".into(),
        );
        assert!(tool.ssh_attach_command("session").is_none());
    }

    #[tokio::test]
    async fn blocks_rate_limited() {
        let security = Arc::new(ArcSwap::from_pointee(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        }));
        let tool =
            ClaudeCodeRunnerTool::new(security, test_config(), "http://localhost:3000".into());
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("rate-limited should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn blocks_readonly() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::ReadOnly),
            test_config(),
            "http://localhost:3000".into(),
        );
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("readonly should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("read-only mode"));
    }

    #[tokio::test]
    async fn missing_prompt() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Supervised),
            test_config(),
            "http://localhost:3000".into(),
        );
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("prompt"));
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Full),
            test_config(),
            "http://localhost:3000".into(),
        );
        let result = tool
            .execute(json!({
                "prompt": "hello",
                "working_directory": "/etc"
            }))
            .await
            .expect("should return a result for path validation");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("outside the workspace"));
    }

    #[test]
    fn shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "hello");
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn hook_event_deserialization() {
        let json = r#"{
            "session_id": "abc123",
            "event_type": "tool_use",
            "tool_name": "Edit",
            "summary": "Editing file.rs"
        }"#;
        let event: ClaudeCodeHookEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.session_id, "abc123");
        assert_eq!(event.event_type, "tool_use");
        assert_eq!(event.tool_name.as_deref(), Some("Edit"));
    }
}
