use super::traits::{Tool, ToolResult};
use crate::config::CodexRunnerConfig;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::process::Command;

/// Environment variables safe to pass through to the `codex` subprocess.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Spawns Codex inside a tmux session for long-running coding tasks and
/// operator handoff.
///
/// Unlike [`CodexCliTool`](super::codex_cli::CodexCliTool), which runs
/// `codex exec` inline and waits for completion, this runner:
///
/// 1. Creates a named tmux session (`<prefix><id>`)
/// 2. Launches `codex exec --json` inside it with persisted session files
/// 3. Returns immediately with the session ID and an attach command
/// 4. Leaves Codex's own session state on disk so the harness can be resumed
pub struct CodexRunnerTool {
    security: Arc<SecurityPolicy>,
    config: CodexRunnerConfig,
}

impl CodexRunnerTool {
    pub fn new(security: Arc<SecurityPolicy>, config: CodexRunnerConfig) -> Self {
        Self { security, config }
    }

    fn session_name(&self, id: &str) -> String {
        format!("{}{}", self.config.tmux_prefix, id)
    }

    fn ssh_attach_command(&self, session_name: &str) -> Option<String> {
        self.config
            .ssh_host
            .as_ref()
            .map(|host| format!("ssh -t {host} tmux attach-session -t {session_name}"))
    }
}

#[async_trait]
impl Tool for CodexRunnerTool {
    fn name(&self) -> &str {
        "codex_runner"
    }

    fn description(&self) -> &str {
        "Spawn a Codex task in a tmux session with session handoff. Returns immediately with a runner session ID and attach command."
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
                },
                "model": {
                    "type": "string",
                    "description": "Optional Codex model override"
                },
                "profile": {
                    "type": "string",
                    "description": "Optional Codex profile to use"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "codex_runner")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

        let work_dir = if let Some(wd) = args.get("working_directory").and_then(|v| v.as_str()) {
            let workspace = &self.security.workspace_dir;
            let wd_path = {
                let raw = std::path::PathBuf::from(wd);
                if raw.is_absolute() {
                    raw
                } else {
                    workspace.join(raw)
                }
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

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let session_name = self.session_name(&session_id);
        let log_path = work_dir.join(format!(".codex-runner-{session_id}.jsonl"));

        let mut codex_args = vec![
            "codex".to_string(),
            "exec".to_string(),
            "--skip-git-repo-check".to_string(),
            "--json".to_string(),
            prompt.to_string(),
        ];

        if let Some(model) = args.get("model").and_then(|v| v.as_str()) {
            codex_args.push("--model".to_string());
            codex_args.push(model.to_string());
        }

        if let Some(profile) = args.get("profile").and_then(|v| v.as_str()) {
            codex_args.push("--profile".to_string());
            codex_args.push(profile.to_string());
        }

        let mut env_exports = String::new();
        for var in SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                use std::fmt::Write;
                let _ = write!(env_exports, "{}={} ", var, shell_escape(&val));
            }
        }
        for var in &self.config.env_passthrough {
            let trimmed = var.trim();
            if !trimmed.is_empty() {
                if let Ok(val) = std::env::var(trimmed) {
                    use std::fmt::Write;
                    let _ = write!(env_exports, "{}={} ", trimmed, shell_escape(&val));
                }
            }
        }
        {
            use std::fmt::Write;
            let _ = write!(env_exports, "CODEX_RUNNER_SESSION_ID={} ", &session_id);
        }

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
                        "tmux not found or failed to execute: {e}. Install tmux to use codex_runner."
                    )),
                });
            }
            _ => {}
        }

        // Keep raw JSONL so operators can inspect Codex event output after handoff.
        let full_command = format!(
            "{env_exports}{cmd} | tee {log_path}",
            env_exports = env_exports,
            cmd = codex_args
                .iter()
                .map(|a| shell_escape(a))
                .collect::<Vec<_>>()
                .join(" "),
            log_path = shell_escape(log_path.to_str().unwrap_or(".codex-runner.jsonl")),
        );

        let send_result = Command::new("tmux")
            .args(["send-keys", "-t", &session_name, &full_command, "Enter"])
            .output()
            .await;

        if let Err(e) = send_result {
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
                "Codex runner session TTL expired, cleaned up"
            );
        });

        let mut output_parts = vec![
            format!("Session started: {session_name}"),
            format!("Session ID: {session_id}"),
            format!("Working directory: {}", work_dir.display()),
            format!("JSONL log: {}", log_path.display()),
            "Codex session files are persisted by default, so the harness can be resumed from the attached tmux session.".to_string(),
        ];

        if let Some(ssh_cmd) = self.ssh_attach_command(&session_name) {
            output_parts.push(format!("SSH attach: {ssh_cmd}"));
        } else {
            output_parts.push(format!(
                "Local attach: tmux attach-session -t {session_name}"
            ));
        }

        Ok(ToolResult {
            success: true,
            output: output_parts.join("\n"),
            error: None,
        })
    }
}

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
    use crate::config::CodexRunnerConfig;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_config() -> CodexRunnerConfig {
        CodexRunnerConfig {
            enabled: true,
            ssh_host: Some("dev.example.com".into()),
            tmux_prefix: "zc-test-codex-".into(),
            session_ttl: 3600,
            env_passthrough: vec!["OPENAI_API_KEY".into()],
        }
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn tool_name() {
        let tool = CodexRunnerTool::new(test_security(AutonomyLevel::Supervised), test_config());
        assert_eq!(tool.name(), "codex_runner");
    }

    #[test]
    fn tool_schema_has_prompt() {
        let tool = CodexRunnerTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["prompt"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .expect("required should be an array")
                .contains(&json!("prompt"))
        );
    }

    #[test]
    fn session_name_uses_prefix() {
        let tool = CodexRunnerTool::new(test_security(AutonomyLevel::Supervised), test_config());
        assert_eq!(tool.session_name("abc123"), "zc-test-codex-abc123");
    }

    #[test]
    fn ssh_attach_command_with_host() {
        let tool = CodexRunnerTool::new(test_security(AutonomyLevel::Supervised), test_config());
        assert_eq!(
            tool.ssh_attach_command("zc-test-codex-abc123").as_deref(),
            Some("ssh -t dev.example.com tmux attach-session -t zc-test-codex-abc123")
        );
    }

    #[test]
    fn shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "hello");
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[tokio::test]
    async fn blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = CodexRunnerTool::new(security, test_config());
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("rate-limited should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace() {
        let tool = CodexRunnerTool::new(test_security(AutonomyLevel::Full), test_config());
        let result = tool
            .execute(json!({"prompt": "hello", "working_directory": "/etc"}))
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
}
