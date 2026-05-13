//! Gemini CLI subprocess model_provider.
//!
//! Integrates with the Gemini CLI, spawning the `gemini` binary
//! as a subprocess for each inference request. This allows using Google's
//! Gemini models via the CLI without an interactive UI session.
//!
//! # Usage
//!
//! The `gemini` binary must be available in `PATH`, or its location can be
//! set via the typed alias's `binary_path` field.
//!
//! Gemini CLI is invoked as:
//! ```text
//! gemini --prompt ""
//! ```
//! with prompt content written to stdin. The empty `--prompt ""` is the
//! headless-mode trigger; the CLI appends stdin to it, so the actual prompt
//! is never visible in `ps` / `/proc/<pid>/cmdline`. Older CLI builds used
//! `--print -` for this; that flag was removed in Gemini CLI v0.40.x.
//!
//! # Limitations
//!
//! - **Conversation history**: Only the system prompt (if present) and the last
//!   user message are forwarded. Full multi-turn history is not preserved because
//!   the CLI accepts a single prompt per invocation.
//! - **System prompt**: The system prompt is prepended to the user message with a
//!   blank-line separator, as the CLI does not provide a dedicated system-prompt flag.
//! - **Temperature**: The CLI does not expose a temperature parameter.
//!   Only default values are accepted; custom values return an explicit error.
//!
//! # Authentication
//!
//! Authentication is handled by the Gemini CLI itself (its own credential store).
//! No explicit API key is required by this model_provider.
//!
use crate::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

/// Default `gemini` binary name (resolved via `PATH`).
const DEFAULT_GEMINI_CLI_BINARY: &str = "gemini";

/// Model name used to signal "use the model_provider's own default model".
const DEFAULT_MODEL_MARKER: &str = "default";
/// Gemini CLI requests are bounded to avoid hung subprocesses.
const GEMINI_CLI_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Avoid leaking oversized stderr payloads.
const MAX_GEMINI_CLI_STDERR_CHARS: usize = 512;
/// The CLI does not support sampling controls; allow only baseline defaults.
const GEMINI_CLI_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// ModelProvider that invokes the Gemini CLI as a subprocess.
///
/// Each inference request spawns a fresh `gemini` process. This is the
/// non-interactive approach: the process handles the prompt and exits.
pub struct GeminiCliModelProvider {
    /// `[model_providers.<family>.<alias>]` config-key alias.
    alias: String,
    /// Path to the `gemini` binary.
    binary_path: PathBuf,
}

impl GeminiCliModelProvider {
    /// Create a new `GeminiCliModelProvider`. Pass `None` to use the default
    /// `"gemini"` (PATH lookup); pass an explicit path to override.
    pub fn new(alias: &str, binary_path: Option<&str>) -> Self {
        let binary_path = binary_path
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_GEMINI_CLI_BINARY));
        Self {
            alias: alias.to_string(),
            binary_path,
        }
    }
    /// Returns true if the model argument should be forwarded to the CLI.
    fn should_forward_model(model: &str) -> bool {
        let trimmed = model.trim();
        !trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER
    }

    fn supports_temperature(temperature: f64) -> bool {
        GEMINI_CLI_SUPPORTED_TEMPERATURES
            .iter()
            .any(|v| (temperature - v).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("Gemini CLI model_provider received non-finite temperature value");
        }
        if !Self::supports_temperature(temperature) {
            anyhow::bail!(
                "temperature unsupported by Gemini CLI: {temperature}. \
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
        if trimmed.chars().count() <= MAX_GEMINI_CLI_STDERR_CHARS {
            return trimmed.to_string();
        }
        let clipped: String = trimmed.chars().take(MAX_GEMINI_CLI_STDERR_CHARS).collect();
        format!("{clipped}...")
    }

    /// Build the argv tail (excluding the binary itself) for a CLI invocation.
    ///
    /// `--prompt ""` is the headless-mode trigger; the CLI appends stdin to
    /// it, so the actual prompt stays out of `ps` / `/proc/<pid>/cmdline`.
    /// The pre-v0.40 syntax `--print -` was removed upstream in #6520.
    fn build_cli_args(model: &str) -> Vec<String> {
        let mut args = vec!["--prompt".to_string(), String::new()];
        if Self::should_forward_model(model) {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
        args
    }

    /// Invoke the gemini binary with the given prompt and optional model.
    /// Returns the trimmed stdout output as the assistant response.
    async fn invoke_cli(&self, message: &str, model: &str) -> anyhow::Result<String> {
        let mut cmd = Command::new(&self.binary_path);
        cmd.args(Self::build_cli_args(model));

        cmd.kill_on_drop(true);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|err| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "binary": self.binary_path.display().to_string(),
                        "phase": "spawn",
                        "error": format!("{}", err),
                    })),
                "gemini_cli: failed to spawn binary"
            );
            anyhow::Error::msg(format!(
                "Failed to spawn Gemini CLI binary at {}: {err}. \
                 Ensure `gemini` is installed and in PATH, or set GEMINI_CLI_PATH.",
                self.binary_path.display()
            ))
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(message.as_bytes()).await.map_err(|err| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "stdin_write",
                            "error": format!("{}", err),
                        })),
                    "gemini_cli: failed to write prompt to stdin"
                );
                anyhow::Error::msg(format!("Failed to write prompt to Gemini CLI stdin: {err}"))
            })?;
            stdin.shutdown().await.map_err(|err| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "stdin_shutdown",
                            "error": format!("{}", err),
                        })),
                    "gemini_cli: failed to finalize stdin stream"
                );
                anyhow::Error::msg(format!("Failed to finalize Gemini CLI stdin stream: {err}"))
            })?;
        }

        let output = timeout(GEMINI_CLI_REQUEST_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "binary": self.binary_path.display().to_string(),
                            "timeout": format!("{:?}", GEMINI_CLI_REQUEST_TIMEOUT),
                        })),
                    "gemini_cli: request timed out"
                );
                anyhow::Error::msg(format!(
                    "Gemini CLI request timed out after {:?} (binary: {})",
                    GEMINI_CLI_REQUEST_TIMEOUT,
                    self.binary_path.display()
                ))
            })?
            .map_err(|err| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "process_wait",
                            "error": format!("{}", err),
                        })),
                    "gemini_cli: process wait failed"
                );
                anyhow::Error::msg(format!("Gemini CLI process failed: {err}"))
            })?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr_excerpt = Self::redact_stderr(&output.stderr);
            let stderr_note = if stderr_excerpt.is_empty() {
                String::new()
            } else {
                format!(" Stderr: {stderr_excerpt}")
            };
            anyhow::bail!(
                "Gemini CLI exited with non-zero status {code}. \
                 Check that Gemini CLI is authenticated and the CLI is supported.{stderr_note}"
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
                "gemini_cli: non-UTF-8 stdout"
            );
            anyhow::Error::msg(format!("Gemini CLI produced non-UTF-8 output: {err}"))
        })?;

        Ok(text.trim().to_string())
    }
}

#[async_trait]
impl ModelProvider for GeminiCliModelProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let temperature = temperature.unwrap_or(self.default_temperature());
        Self::validate_temperature(temperature)?;

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
            reasoning_field: None,
        })
    }
}

impl ::zeroclaw_api::attribution::Attributable for GeminiCliModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::GeminiCli,
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
        let p = GeminiCliModelProvider::new("test", Some("/usr/local/bin/gemini"));
        assert_eq!(p.binary_path, PathBuf::from("/usr/local/bin/gemini"));
    }

    #[test]
    fn new_defaults_to_gemini() {
        let p = GeminiCliModelProvider::new("test", None);
        assert_eq!(p.binary_path, PathBuf::from("gemini"));
    }

    #[test]
    fn new_ignores_blank_binary_path() {
        let p = GeminiCliModelProvider::new("test", Some("   "));
        assert_eq!(p.binary_path, PathBuf::from("gemini"));
    }

    #[test]
    fn should_forward_model_standard() {
        assert!(GeminiCliModelProvider::should_forward_model(
            "gemini-2.5-pro"
        ));
        assert!(GeminiCliModelProvider::should_forward_model(
            "gemini-2.5-flash"
        ));
    }

    #[test]
    fn should_not_forward_default_model() {
        assert!(!GeminiCliModelProvider::should_forward_model(
            DEFAULT_MODEL_MARKER
        ));
        assert!(!GeminiCliModelProvider::should_forward_model(""));
        assert!(!GeminiCliModelProvider::should_forward_model("   "));
    }

    #[test]
    fn validate_temperature_allows_defaults() {
        assert!(GeminiCliModelProvider::validate_temperature(0.7).is_ok());
        assert!(GeminiCliModelProvider::validate_temperature(1.0).is_ok());
    }

    #[test]
    fn validate_temperature_rejects_custom_value() {
        let err = GeminiCliModelProvider::validate_temperature(0.2).unwrap_err();
        assert!(
            err.to_string()
                .contains("temperature unsupported by Gemini CLI")
        );
    }

    // Regression for #6520. Gemini CLI v0.40.x removed `--print`; the
    // headless-mode flag is now `--prompt`. The argv must (a) carry
    // `--prompt ""` so stdin still feeds the prompt content (keeping the
    // user message out of `ps`) and (b) NEVER carry `--print` or `-`.
    #[test]
    fn build_cli_args_uses_prompt_flag_with_empty_token_default_model() {
        let args = GeminiCliModelProvider::build_cli_args(DEFAULT_MODEL_MARKER);
        assert_eq!(args, vec!["--prompt".to_string(), String::new()]);
        assert!(!args.iter().any(|a| a == "--print"));
        assert!(!args.iter().any(|a| a == "-"));
    }

    #[test]
    fn build_cli_args_forwards_explicit_model_after_prompt() {
        let args = GeminiCliModelProvider::build_cli_args("gemini-2.5-pro");
        assert_eq!(
            args,
            vec![
                "--prompt".to_string(),
                String::new(),
                "--model".to_string(),
                "gemini-2.5-pro".to_string(),
            ]
        );
        assert!(!args.iter().any(|a| a == "--print"));
    }

    #[tokio::test]
    async fn invoke_missing_binary_returns_error() {
        let model_provider = GeminiCliModelProvider {
            alias: "test".to_string(),
            binary_path: PathBuf::from("/nonexistent/path/to/gemini"),
        };
        let result = model_provider.invoke_cli("hello", "default").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn Gemini CLI binary"),
            "unexpected error message: {msg}"
        );
    }
}
