//! Grok Build CLI subprocess model_provider.
//!
//! Spawns the `grok` binary in headless mode (`grok -p`) for each inference
//! request. Authentication uses the CLI's own session / `XAI_API_KEY`.
//!
//! # Usage
//!
//! ```toml
//! [providers.models.grok_cli.default]
//! model = "grok-4.5"
//! # binary_path = "/home/you/.grok/bin/grok"
//!
//! [agents.default]
//! model_provider = "grok_cli.default"
//! ```
//!
//! # Limitations
//!
//! - **Conversation history**: Only the system prompt (if present) and the last
//!   user message are forwarded (same contract as `gemini_cli`).
//! - **Native tool calls**: This provider does not emit ZeroClaw tool_calls.
//! - **Permissions / tools**: ZeroClaw does **not** pass `--always-approve`,
//!   `--permission-mode`, `--disallowed-tools`, or `--max-turns`. Tool
//!   enablement and permission rules belong in Grok project config under the
//!   subprocess working directory (typically the agent workspace):
//!   `.grok/config.toml` (`[permission]`), plus `.grok/skills/`, etc.
//!   See Grok user-guide `05-configuration.md` / `22-permissions-and-safety.md`.
//! - **Temperature**: Only baseline values `0.7` and `1.0` are accepted.
//!
use crate::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

/// Default `grok` binary name (resolved via `PATH`).
const DEFAULT_GROK_CLI_BINARY: &str = "grok";

/// Model name used to signal "use the CLI / config default model".
const DEFAULT_MODEL_MARKER: &str = "default";
/// Bound hung subprocesses. Tools may still run if the workspace `.grok`
/// config allows them; keep a generous but finite budget.
const GROK_CLI_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_GROK_CLI_STDERR_CHARS: usize = 512;
const GROK_CLI_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// ModelProvider that invokes Grok Build CLI headless.
///
/// Permission and tool policy are owned by Grok (workspace `.grok/`), not by
/// ZeroClaw argv.
pub struct GrokCliModelProvider {
    /// `[providers.models.<family>.<alias>]` config-key alias.
    alias: String,
    /// Path to the `grok` binary.
    binary_path: PathBuf,
    /// Subprocess cwd so project `.grok/config.toml` resolves correctly.
    working_directory: Option<PathBuf>,
}

impl GrokCliModelProvider {
    /// Create a new provider. Pass `None` for `binary_path` to use `"grok"`
    /// from `PATH`. Optional `working_directory` sets the subprocess cwd
    /// (Grok loads project `.grok/` relative to that path).
    pub fn new(
        alias: &str,
        binary_path: Option<&str>,
        working_directory: Option<&str>,
    ) -> Self {
        let binary_path = binary_path
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_GROK_CLI_BINARY));
        let working_directory = working_directory
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from);
        Self {
            alias: alias.to_string(),
            binary_path,
            working_directory,
        }
    }

    fn should_forward_model(model: &str) -> bool {
        let trimmed = model.trim();
        !trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER
    }

    fn supports_temperature(temperature: f64) -> bool {
        GROK_CLI_SUPPORTED_TEMPERATURES
            .iter()
            .any(|v| (temperature - v).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("Grok CLI model_provider received non-finite temperature value");
        }
        if !Self::supports_temperature(temperature) {
            anyhow::bail!(
                "temperature unsupported by Grok CLI model_provider: {temperature}. \
                 Supported values: 0.7 or 1.0"
            );
        }
        Ok(())
    }

    fn redact_stderr(stderr: &[u8]) -> String {
        let text = String::from_utf8_lossy(stderr);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed.chars().count() <= MAX_GROK_CLI_STDERR_CHARS {
            return trimmed.to_string();
        }
        let clipped: String = trimmed.chars().take(MAX_GROK_CLI_STDERR_CHARS).collect();
        format!("{clipped}...")
    }

    /// Build argv after the binary. Prompt is always passed via
    /// `--prompt-file` so large system prompts never hit OS ARG_MAX.
    ///
    /// Only headless plumbing flags — no permission / tool / max-turns policy.
    /// Those are configured in the workspace `.grok/` tree (see module docs).
    fn build_cli_args(model: &str, prompt_file: &std::path::Path) -> Vec<String> {
        let mut args = vec![
            "--prompt-file".to_string(),
            prompt_file.display().to_string(),
            "--output-format".to_string(),
            "plain".to_string(),
            "--no-auto-update".to_string(),
        ];
        if Self::should_forward_model(model) {
            args.push("-m".to_string());
            args.push(model.to_string());
        }
        args
    }

    async fn invoke_cli(&self, message: &str, model: &str) -> anyhow::Result<String> {
        // Write prompt to a temp file — ZeroClaw system prompts routinely
        // exceed ARG_MAX when passed as a `-p` argv token. Use std only
        // (no extra crate dep): unique path under the process temp dir.
        let prompt_path = std::env::temp_dir().join(format!(
            "zeroclaw-grok-prompt-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        tokio::fs::write(&prompt_path, message.as_bytes())
            .await
            .map_err(|err| {
                anyhow::Error::msg(format!(
                    "Failed to write Grok CLI prompt file {}: {err}",
                    prompt_path.display()
                ))
            })?;

        let mut cmd = Command::new(&self.binary_path);
        cmd.args(Self::build_cli_args(model, &prompt_path));
        if let Some(cwd) = self.working_directory.as_ref() {
            cmd.current_dir(cwd);
        }
        cmd.kill_on_drop(true);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let spawn_result = cmd.spawn();
        let child = match spawn_result {
            Ok(child) => child,
            Err(err) => {
                let _ = tokio::fs::remove_file(&prompt_path).await;
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "binary": self.binary_path.display().to_string(),
                            "phase": "spawn",
                            "error": format!("{}", err),
                        })),
                    "grok_cli: failed to spawn binary"
                );
                return Err(anyhow::Error::msg(format!(
                    "Failed to spawn Grok Build CLI binary at {}: {err}. \
                     Ensure `grok` is installed and on PATH \
                     (e.g. ~/.grok/bin), or set binary_path on the provider alias.",
                    self.binary_path.display()
                )));
            }
        };

        let wait_result = timeout(GROK_CLI_REQUEST_TIMEOUT, child.wait_with_output()).await;
        let _ = tokio::fs::remove_file(&prompt_path).await;

        let output = match wait_result {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "process_wait",
                            "error": format!("{}", err),
                        })),
                    "grok_cli: process wait failed"
                );
                return Err(anyhow::Error::msg(format!(
                    "Grok Build CLI process failed: {err}"
                )));
            }
            Err(_) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "binary": self.binary_path.display().to_string(),
                            "timeout": format!("{:?}", GROK_CLI_REQUEST_TIMEOUT),
                        })),
                    "grok_cli: request timed out"
                );
                return Err(anyhow::Error::msg(format!(
                    "Grok Build CLI request timed out after {:?} (binary: {})",
                    GROK_CLI_REQUEST_TIMEOUT,
                    self.binary_path.display()
                )));
            }
        };

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr_excerpt = Self::redact_stderr(&output.stderr);
            let stderr_note = if stderr_excerpt.is_empty() {
                String::new()
            } else {
                format!(" Stderr: {stderr_excerpt}")
            };
            anyhow::bail!(
                "Grok Build CLI exited with non-zero status {code}. \
                 Check that Grok is authenticated (`grok login` or XAI_API_KEY).{stderr_note}"
            );
        }

        let text = String::from_utf8(output.stdout).map_err(|err| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "utf8_decode",
                        "error": format!("{}", err),
                    })),
                "grok_cli: non-UTF-8 stdout"
            );
            anyhow::Error::msg(format!("Grok Build CLI produced non-UTF-8 output: {err}"))
        })?;

        Ok(text.trim().to_string())
    }
}

#[async_trait]
impl ModelProvider for GrokCliModelProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        if let Some(t) = temperature {
            Self::validate_temperature(t)?;
        }

        let full_message = match system_prompt {
            Some(system) if !system.is_empty() => {
                format!("{system}\n\n{message}")
            }
            _ => message.to_string(),
        };

        self.invoke_cli(&full_message, model).await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let text = self
            .chat_with_history(request.messages, model, temperature)
            .await?;

        Ok(ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            usage: Some(TokenUsage::default()),
            reasoning_content: None,
        })
    }
}

impl ::zeroclaw_api::attribution::Attributable for GrokCliModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::GrokCli,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_explicit_binary_path() {
        let p = GrokCliModelProvider::new("test", Some("/home/user/.grok/bin/grok"), None);
        assert_eq!(
            p.binary_path,
            PathBuf::from("/home/user/.grok/bin/grok")
        );
        assert!(p.working_directory.is_none());
    }

    #[test]
    fn new_defaults_to_grok() {
        let p = GrokCliModelProvider::new("test", None, None);
        assert_eq!(p.binary_path, PathBuf::from("grok"));
    }

    #[test]
    fn new_ignores_blank_binary_path() {
        let p = GrokCliModelProvider::new("test", Some("   "), None);
        assert_eq!(p.binary_path, PathBuf::from("grok"));
    }

    #[test]
    fn new_sets_working_directory() {
        let p = GrokCliModelProvider::new("test", None, Some("/tmp/agent-ws"));
        assert_eq!(p.working_directory, Some(PathBuf::from("/tmp/agent-ws")));
    }

    #[test]
    fn should_forward_model_standard() {
        assert!(GrokCliModelProvider::should_forward_model("grok-4.5"));
        assert!(GrokCliModelProvider::should_forward_model("grok-build"));
    }

    #[test]
    fn should_not_forward_default_model() {
        assert!(!GrokCliModelProvider::should_forward_model(
            DEFAULT_MODEL_MARKER
        ));
        assert!(!GrokCliModelProvider::should_forward_model(""));
        assert!(!GrokCliModelProvider::should_forward_model("   "));
    }

    #[test]
    fn validate_temperature_allows_defaults() {
        assert!(GrokCliModelProvider::validate_temperature(0.7).is_ok());
        assert!(GrokCliModelProvider::validate_temperature(1.0).is_ok());
    }

    #[test]
    fn validate_temperature_rejects_custom_value() {
        let err = GrokCliModelProvider::validate_temperature(0.2).unwrap_err();
        assert!(
            err.to_string()
                .contains("temperature unsupported by Grok CLI")
        );
    }

    #[test]
    fn build_cli_args_is_permission_agnostic_headless_plumbing() {
        let path = PathBuf::from("/tmp/prompt.md");
        let args = GrokCliModelProvider::build_cli_args(DEFAULT_MODEL_MARKER, &path);
        assert_eq!(args[0], "--prompt-file");
        assert_eq!(args[1], "/tmp/prompt.md");
        assert!(args.iter().any(|a| a == "--output-format"));
        assert!(args.iter().any(|a| a == "plain"));
        // ZeroClaw must not inject permission / tool policy via argv.
        for forbidden in [
            "--always-approve",
            "bypassPermissions",
            "--permission-mode",
            "--max-turns",
            "--disallowed-tools",
            "--tools",
            "--no-subagents",
            "--no-plan",
        ] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "unexpected policy flag in argv: {forbidden}"
            );
        }
        assert!(!args.iter().any(|a| a == "-m"));
        assert!(!args.iter().any(|a| a == "-p"));
    }

    #[test]
    fn build_cli_args_forwards_explicit_model() {
        let path = PathBuf::from("/tmp/prompt.md");
        let args = GrokCliModelProvider::build_cli_args("grok-4.5", &path);
        let m_idx = args.iter().position(|a| a == "-m").expect("-m flag");
        assert_eq!(args[m_idx + 1], "grok-4.5");
    }

    #[tokio::test]
    async fn invoke_missing_binary_returns_error() {
        let model_provider = GrokCliModelProvider {
            alias: "test".to_string(),
            binary_path: PathBuf::from("/nonexistent/path/to/grok"),
            working_directory: None,
        };
        let result = model_provider.invoke_cli("hello", "default").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn Grok Build CLI binary"),
            "unexpected error message: {msg}"
        );
    }
}
