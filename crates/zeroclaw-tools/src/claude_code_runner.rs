use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use tokio::process::Command;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_config::schema::ClaudeCodeRunnerConfig;

const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];
const MAX_SLACK_CHANNEL_ID_LEN: usize = 64;

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

pub struct ClaudeCodeRunnerTool {
    security: Arc<SecurityPolicy>,
    config: ClaudeCodeRunnerConfig,
    /// Base URL of the ZeroClaw gateway (e.g. `"http://localhost:3000"`).
    gateway_url: String,
    tmux_binary: PathBuf,
}

impl ClaudeCodeRunnerTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        config: ClaudeCodeRunnerConfig,
        gateway_url: String,
    ) -> Self {
        Self {
            security,
            config,
            gateway_url,
            tmux_binary: PathBuf::from("tmux"),
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn with_tmux_binary(mut self, tmux_binary: PathBuf) -> Self {
        self.tmux_binary = tmux_binary;
        self
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
                    "description": "Slack conversation ID to post progress updates to",
                    "pattern": "^[CGD][A-Z0-9]{1,63}$",
                    "minLength": 2,
                    "maxLength": MAX_SLACK_CHANNEL_ID_LEN
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
            .authorize_tool_operation(ToolOperation::Act, "claude_code_runner")
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
                "claude_code_runner: missing prompt parameter"
            );
            anyhow::Error::msg("Missing 'prompt' parameter")
        })?;

        // Validate working directory
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

        let slack_channel = match args.get("slack_channel") {
            None => None,
            Some(serde_json::Value::String(channel)) => Some(channel.clone()),
            Some(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("slack_channel must be a string".to_string()),
                });
            }
        };
        if let Some(channel) = slack_channel.as_deref()
            && !is_valid_slack_channel_id(channel)
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "slack_channel must be a Slack conversation ID starting with C, G, or D and containing at most {MAX_SLACK_CHANNEL_ID_LEN} uppercase ASCII letters or digits"
                )),
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

        // Build the command sent through tmux.
        let mut full_command = String::new();
        for var in SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                append_env_assignment(&mut full_command, var, &val);
            }
        }
        // Pass session metadata via env vars so the hook can correlate events
        append_env_assignment(&mut full_command, "CLAUDE_CODE_SESSION_ID", &session_id);
        if let Some(ref ch) = slack_channel {
            append_env_assignment(&mut full_command, "CLAUDE_CODE_SLACK_CHANNEL", ch);
        }
        append_env_assignment(&mut full_command, "CLAUDE_CODE_HOOK_URL", &hook_url);
        append_shell_arguments(&mut full_command, &claude_args);

        // Create tmux session
        let create_result = Command::new(&self.tmux_binary)
            .args(["new-session", "-d", "-s", &session_name])
            .arg("-c")
            .arg(
                crate::util_helpers::clean_verbatim_path(&work_dir)
                    .to_str()
                    .unwrap_or("."),
            )
            .output()
            .await;

        match create_result {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to create tmux session: {stderr}")),
                });
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "tmux not found or failed to execute: {e}. Install tmux to use claude_code_runner."
                    )),
                });
            }
            _ => {}
        }

        // Send the claude command into the tmux session
        let send_result = Command::new(&self.tmux_binary)
            .args(["send-keys", "-t", &session_name, &full_command, "Enter"])
            .output()
            .await;

        let send_error = match send_result {
            Ok(output) => tmux_command_failure("send command", &output),
            Err(error) => Some(format!("Failed to send command to tmux session: {error}")),
        };
        if let Some(error) = send_error {
            let error = match kill_tmux_session(&self.tmux_binary, &session_name).await {
                Ok(()) => error,
                Err(cleanup_error) => format!("{error}; cleanup also failed: {cleanup_error}"),
            };
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        // Schedule session TTL cleanup
        let ttl = self.config.session_ttl;
        let cleanup_session = session_name.clone();
        let tmux_binary = self.tmux_binary.clone();
        zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(std::time::Duration::from_secs(ttl)).await;
            let _ = cleanup_expired_session(&tmux_binary, &cleanup_session).await;
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
            output: output_parts.join("\n").into(),
            error: None,
        })
    }
}

fn is_valid_slack_channel_id(value: &str) -> bool {
    value.len() >= 2
        && value.len() <= MAX_SLACK_CHANNEL_ID_LEN
        && matches!(value.as_bytes()[0], b'C' | b'G' | b'D')
        && value.as_bytes()[1..]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn append_env_assignment(command: &mut String, name: &str, value: &str) {
    command.push_str(name);
    command.push('=');
    command.push_str(&shell_escape(value));
    command.push(' ');
}

fn append_shell_arguments(command: &mut String, args: &[String]) {
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            command.push(' ');
        }
        command.push_str(&shell_escape(arg));
    }
}

fn tmux_command_failure(operation: &str, output: &Output) -> Option<String> {
    if output.status.success() {
        return None;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Some(format!(
        "Failed to {operation} in tmux ({}): {}",
        output.status,
        stderr.trim()
    ))
}

async fn kill_tmux_session(tmux_binary: &Path, session_name: &str) -> Result<(), String> {
    let output = Command::new(tmux_binary)
        .args(["kill-session", "-t", session_name])
        .output()
        .await
        .map_err(|error| format!("Failed to start tmux cleanup: {error}"))?;
    match tmux_command_failure("kill session", &output) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn cleanup_expired_session(tmux_binary: &Path, session_name: &str) -> Result<(), String> {
    let result = kill_tmux_session(tmux_binary, session_name).await;
    match &result {
        Ok(()) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"session": session_name})),
                "Claude Code runner session TTL expired, cleaned up"
            );
        }
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"session": session_name, "error": error})),
                "Claude Code runner session TTL cleanup failed"
            );
        }
    }
    result
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
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::ClaudeCodeRunnerConfig;

    fn test_config() -> ClaudeCodeRunnerConfig {
        ClaudeCodeRunnerConfig {
            enabled: true,
            ssh_host: Some("dev.example.com".into()),
            tmux_prefix: "zc-test-".into(),
            session_ttl: 3600,
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
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Supervised),
            test_config(),
            "http://localhost:3000".into(),
        );
        assert_eq!(tool.name(), "claude_code_runner");
    }

    #[test]
    fn tool_schema_has_prompt_and_slack_contract() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Supervised),
            test_config(),
            "http://localhost:3000".into(),
        );
        let schema = tool.parameters_schema();
        assert_eq!(
            schema["properties"]["slack_channel"]["pattern"],
            "^[CGD][A-Z0-9]{1,63}$"
        );
        assert_eq!(schema["properties"]["slack_channel"]["minLength"], 2);
        assert_eq!(
            schema["properties"]["slack_channel"]["maxLength"],
            MAX_SLACK_CHANNEL_ID_LEN
        );
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
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = crate::wrappers::RateLimitedTool::new(
            ClaudeCodeRunnerTool::new(
                security.clone(),
                test_config(),
                "http://localhost:3000".into(),
            ),
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
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("read-only mode")
        );
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
        let workspace = tempfile::TempDir::new().expect("temp workspace");
        let outside = tempfile::TempDir::new().expect("temp directory outside workspace");
        let tool = ClaudeCodeRunnerTool::new(
            Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Full,
                workspace_dir: workspace.path().to_path_buf(),
                ..SecurityPolicy::default()
            }),
            test_config(),
            "http://localhost:3000".into(),
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

    #[tokio::test]
    async fn rejects_shell_syntax_in_slack_channel_before_starting_tmux() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Full),
            test_config(),
            "http://localhost:3000".into(),
        );
        let result = tool
            .execute(json!({
                "prompt": "hello",
                "slack_channel": "C123; touch /tmp/owned"
            }))
            .await
            .expect("invalid channel should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Slack conversation ID")
        );
    }

    #[tokio::test]
    async fn rejects_non_string_slack_channel_before_starting_tmux() {
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Full),
            test_config(),
            "http://localhost:3000".into(),
        );
        let result = tool
            .execute(json!({"prompt": "hello", "slack_channel": 42}))
            .await
            .expect("invalid channel type should return a result");
        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some("slack_channel must be a string")
        );
    }

    #[test]
    fn environment_assignments_escape_every_value() {
        let mut command = String::new();
        append_env_assignment(&mut command, "CLAUDE_CODE_HOOK_URL", "https://host/a b'c");
        assert_eq!(command, "CLAUDE_CODE_HOOK_URL='https://host/a b'\\''c' ");
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, contents: String) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, contents).expect("write executable fixture");
        let mut permissions = std::fs::metadata(path)
            .expect("executable fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make fixture executable");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_command_preserves_untrusted_values_through_shell_parsing() {
        let temp = tempfile::TempDir::new().expect("shell fixture directory");
        let recorder = temp.path().join("record-command");
        let env_log = temp.path().join("env.log");
        let args_log = temp.path().join("args.log");
        let sentinel = temp.path().join("injected");
        write_executable(
            &recorder,
            format!(
                "#!/bin/sh\nprintf '%s' \"$TEST_UNTRUSTED\" > {}\nprintf '%s\\n' \"$@\" > {}\n",
                shell_escape(env_log.to_str().expect("env log path is UTF-8")),
                shell_escape(args_log.to_str().expect("args log path is UTF-8")),
            ),
        );

        let prompt = format!("review $(touch {}) and 'quote'", sentinel.display());
        let hook_url = format!("https://host/a b'; touch {}", sentinel.display());
        let env_value = format!("$(touch {}) ; 'value'", sentinel.display());
        let args = vec![
            recorder.to_string_lossy().into_owned(),
            "-p".to_string(),
            prompt.clone(),
            "--hook-url".to_string(),
            hook_url.clone(),
        ];
        let mut command = String::new();
        append_env_assignment(&mut command, "TEST_UNTRUSTED", &env_value);
        append_shell_arguments(&mut command, &args);
        let output = Command::new("/bin/sh")
            .args(["-c", &command])
            .output()
            .await
            .expect("execute assembled command through a shell");

        assert!(output.status.success());
        assert_eq!(
            std::fs::read_to_string(env_log).expect("read recorded environment"),
            env_value
        );
        assert_eq!(
            std::fs::read_to_string(args_log)
                .expect("read recorded arguments")
                .lines()
                .collect::<Vec<_>>(),
            vec!["-p", prompt.as_str(), "--hook-url", hook_url.as_str()]
        );
        assert!(!sentinel.exists());
    }

    #[test]
    fn slack_channel_validation_accepts_ids_and_rejects_shell_input() {
        assert!(is_valid_slack_channel_id("C0123456789"));
        assert!(is_valid_slack_channel_id("GABC123"));
        assert!(is_valid_slack_channel_id("D123"));
        assert!(!is_valid_slack_channel_id("U123"));
        assert!(!is_valid_slack_channel_id("W123"));
        assert!(!is_valid_slack_channel_id(""));
        assert!(!is_valid_slack_channel_id("channel"));
        assert!(!is_valid_slack_channel_id("C123;whoami"));
        assert!(!is_valid_slack_channel_id(
            &"C".repeat(MAX_SLACK_CHANNEL_ID_LEN + 1)
        ));
    }

    #[cfg(unix)]
    fn fake_tmux(send_exit: u8, kill_exit: u8) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::TempDir::new().expect("fake tmux directory");
        let binary = temp.path().join("tmux");
        let log = temp.path().join("tmux.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$1\" in\n  new-session) exit 0 ;;\n  send-keys) exit {send_exit} ;;\n  kill-session) exit {kill_exit} ;;\n  *) exit 64 ;;\nesac\n",
            shell_escape(log.to_str().expect("log path is UTF-8"))
        );
        write_executable(&binary, script);
        (temp, binary, log)
    }

    #[cfg(unix)]
    async fn wait_for_log_event(
        rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>,
        expected_message: &str,
    ) -> serde_json::Value {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(event) = rx.recv().await
                    && event.get("message").and_then(|value| value.as_str())
                        == Some(expected_message)
                {
                    return event;
                }
            }
        })
        .await
        .expect("expected log event before timeout")
    }

    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn successful_session_runs_ttl_cleanup() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let (_temp, binary, log) = fake_tmux(0, 0);
        let mut config = test_config();
        config.session_ttl = 0;
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Full),
            config,
            "http://localhost:3000".into(),
        )
        .with_tmux_binary(binary);

        let result = tool
            .execute(json!({"prompt": "hello", "slack_channel": "C123"}))
            .await
            .expect("fake tmux should return a result");
        assert!(result.success);
        let event = wait_for_log_event(
            &mut rx,
            "Claude Code runner session TTL expired, cleaned up",
        )
        .await;
        assert_eq!(event["event"]["action"], "note");
        let calls = std::fs::read_to_string(log).expect("read fake tmux calls");
        assert!(calls.lines().any(|line| line.starts_with("kill-session ")));
    }

    #[cfg(unix)]
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn expired_session_cleanup_propagates_tmux_failure() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let (_temp, binary, log) = fake_tmux(0, 8);
        let error = cleanup_expired_session(&binary, "zc-test-expired")
            .await
            .expect_err("failed cleanup should remain observable");
        assert!(error.contains("Failed to kill session"));
        let event =
            wait_for_log_event(&mut rx, "Claude Code runner session TTL cleanup failed").await;
        assert_eq!(event["event"]["action"], "reject");
        assert!(
            event["attributes"]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Failed to kill session")
        );
        let calls = std::fs::read_to_string(log).expect("read fake tmux calls");
        assert!(calls.lines().any(|line| line.starts_with("kill-session ")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_tmux_send_cleans_up_and_returns_failure() {
        let (_temp, binary, log) = fake_tmux(7, 0);
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Full),
            test_config(),
            "http://localhost:3000".into(),
        )
        .with_tmux_binary(binary);
        let result = tool
            .execute(json!({"prompt": "hello", "slack_channel": "C123"}))
            .await
            .expect("tmux failure should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to send command")
        );
        let calls = std::fs::read_to_string(log).expect("read fake tmux calls");
        assert!(calls.lines().any(|line| line.starts_with("new-session ")));
        assert!(calls.lines().any(|line| line.starts_with("send-keys ")));
        assert!(calls.lines().any(|line| line.starts_with("kill-session ")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn send_failure_reports_cleanup_failure() {
        let (_temp, binary, _log) = fake_tmux(7, 8);
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Full),
            test_config(),
            "http://localhost:3000".into(),
        )
        .with_tmux_binary(binary);
        let result = tool
            .execute(json!({"prompt": "hello", "slack_channel": "C123"}))
            .await
            .expect("tmux failure should return a result");
        let error = result.error.as_deref().unwrap_or("");
        assert!(error.contains("cleanup also failed"));
        assert!(error.contains("Failed to kill session"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn assembled_tmux_command_quotes_untrusted_values() {
        let (_temp, binary, log) = fake_tmux(0, 0);
        let gateway_url = "http://localhost/a b';touch";
        let prompt = "review $(touch /tmp/not-owned) and 'quote'";
        let tool = ClaudeCodeRunnerTool::new(
            test_security(AutonomyLevel::Full),
            test_config(),
            gateway_url.into(),
        )
        .with_tmux_binary(binary);
        let result = tool
            .execute(json!({"prompt": prompt, "slack_channel": "C123"}))
            .await
            .expect("fake tmux should return a result");
        assert!(result.success);
        let calls = std::fs::read_to_string(log).expect("read fake tmux calls");
        let send_call = calls
            .lines()
            .find(|line| line.starts_with("send-keys "))
            .expect("send-keys call");
        assert!(send_call.contains(&shell_escape(prompt)));
        assert!(send_call.contains(&shell_escape(&format!("{gateway_url}/hooks/claude-code"))));
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
