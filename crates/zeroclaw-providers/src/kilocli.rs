//! KiloCLI subprocess model_provider.
//!
//! Integrates with the KiloCLI tool, spawning the `kilo` binary
//! as a subprocess for each inference request. This allows using KiloCLI's AI
//! models without an interactive UI session.
//!
//! # Usage
//!
//! The `kilo` binary must be available in `PATH`, or its location must be
//! set via the `KILO_CLI_PATH` environment variable.
//!
//! KiloCLI is invoked as:
//! ```text
//! kilo --print -
//! ```
//! with prompt content written to stdin.
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
//! Authentication is handled by KiloCLI itself (its own credential store).
//! No explicit API key is required by this model_provider.
//!
//! # Environment variables
//!
//! - `KILO_CLI_PATH` — override the path to the `kilo` binary (default: `"kilo"`)

use crate::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

/// Environment variable for overriding the path to the `kilo` binary.
pub const KILO_CLI_PATH_ENV: &str = "KILO_CLI_PATH";

/// Default `kilo` binary name (resolved via `PATH`).
const DEFAULT_KILO_CLI_BINARY: &str = "kilo";

/// Model name used to signal "use the model_provider's own default model".
const DEFAULT_MODEL_MARKER: &str = "default";
/// KiloCLI requests are bounded to avoid hung subprocesses.
const KILO_CLI_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Avoid leaking oversized stderr payloads.
const MAX_KILO_CLI_STDERR_CHARS: usize = 512;
/// The CLI does not support sampling controls; allow only baseline defaults.
const KILO_CLI_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// ModelProvider that invokes the KiloCLI as a subprocess.
///
/// Each inference request spawns a fresh `kilo` process. This is the
/// non-interactive approach: the process handles the prompt and exits.
pub struct KiloCliModelProvider {
    /// Path to the `kilo` binary.
    binary_path: PathBuf,
}

impl KiloCliModelProvider {
    /// Create a new `KiloCliModelProvider`.
    ///
    /// The binary path is resolved from `KILO_CLI_PATH` env var if set,
    /// otherwise defaults to `"kilo"` (found via `PATH`).
    pub fn new() -> Self {
        let binary_path = std::env::var(KILO_CLI_PATH_ENV)
            .ok()
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_KILO_CLI_BINARY));

        Self { binary_path }
    }

    /// Returns true if the model argument should be forwarded to the CLI.
    fn should_forward_model(model: &str) -> bool {
        let trimmed = model.trim();
        !trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER
    }

    fn supports_temperature(temperature: f64) -> bool {
        KILO_CLI_SUPPORTED_TEMPERATURES
            .iter()
            .any(|v| (temperature - v).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("KiloCLI model_provider received non-finite temperature value");
        }
        if !Self::supports_temperature(temperature) {
            anyhow::bail!(
                "temperature unsupported by KiloCLI: {temperature}. \
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
        if trimmed.chars().count() <= MAX_KILO_CLI_STDERR_CHARS {
            return trimmed.to_string();
        }
        let clipped: String = trimmed.chars().take(MAX_KILO_CLI_STDERR_CHARS).collect();
        format!("{clipped}...")
    }

    /// Invoke the kilo binary with the given prompt and optional model.
    /// Returns the trimmed stdout output as the assistant response.
    async fn invoke_cli(&self, message: &str, model: &str) -> anyhow::Result<String> {
        let mut cmd = Command::new(&self.binary_path);
        cmd.arg("--print");

        if Self::should_forward_model(model) {
            cmd.arg("--model").arg(model);
        }

        // Read prompt from stdin to avoid exposing sensitive content in process args.
        cmd.arg("-");
        cmd.kill_on_drop(true);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|err| {
            anyhow::anyhow!(
                "Failed to spawn KiloCLI binary at {}: {err}. \
                 Ensure `kilo` is installed and in PATH, or set KILO_CLI_PATH.",
                self.binary_path.display()
            )
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(message.as_bytes())
                .await
                .map_err(|err| anyhow::anyhow!("Failed to write prompt to KiloCLI stdin: {err}"))?;
            stdin
                .shutdown()
                .await
                .map_err(|err| anyhow::anyhow!("Failed to finalize KiloCLI stdin stream: {err}"))?;
        }

        let output = timeout(KILO_CLI_REQUEST_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "KiloCLI request timed out after {:?} (binary: {})",
                    KILO_CLI_REQUEST_TIMEOUT,
                    self.binary_path.display()
                )
            })?
            .map_err(|err| anyhow::anyhow!("KiloCLI process failed: {err}"))?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr_excerpt = Self::redact_stderr(&output.stderr);
            let stderr_note = if stderr_excerpt.is_empty() {
                String::new()
            } else {
                format!(" Stderr: {stderr_excerpt}")
            };
            anyhow::bail!(
                "KiloCLI exited with non-zero status {code}. \
                 Check that KiloCLI is authenticated and the CLI is supported.{stderr_note}"
            );
        }

        let text = String::from_utf8(output.stdout)
            .map_err(|err| anyhow::anyhow!("KiloCLI produced non-UTF-8 output: {err}"))?;

        Ok(text.trim().to_string())
    }
}

impl Default for KiloCliModelProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ModelProvider for KiloCliModelProvider {
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::env_lock;

    #[test]
    fn new_uses_env_override() {
        let _guard = env_lock();
        let orig = std::env::var(KILO_CLI_PATH_ENV).ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var(KILO_CLI_PATH_ENV, "/usr/local/bin/kilo") };
        let model_provider = KiloCliModelProvider::new();
        assert_eq!(
            model_provider.binary_path,
            PathBuf::from("/usr/local/bin/kilo")
        );
        match orig {
            // SAFETY: test-only, single-threaded test runner.
            Some(v) => unsafe { std::env::set_var(KILO_CLI_PATH_ENV, v) },
            // SAFETY: test-only, single-threaded test runner.
            None => unsafe { std::env::remove_var(KILO_CLI_PATH_ENV) },
        }
    }

    #[test]
    fn new_defaults_to_kilo() {
        let _guard = env_lock();
        let orig = std::env::var(KILO_CLI_PATH_ENV).ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var(KILO_CLI_PATH_ENV) };
        let model_provider = KiloCliModelProvider::new();
        assert_eq!(model_provider.binary_path, PathBuf::from("kilo"));
        if let Some(v) = orig {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var(KILO_CLI_PATH_ENV, v) };
        }
    }

    #[test]
    fn new_ignores_blank_env_override() {
        let _guard = env_lock();
        let orig = std::env::var(KILO_CLI_PATH_ENV).ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var(KILO_CLI_PATH_ENV, "   ") };
        let model_provider = KiloCliModelProvider::new();
        assert_eq!(model_provider.binary_path, PathBuf::from("kilo"));
        match orig {
            // SAFETY: test-only, single-threaded test runner.
            Some(v) => unsafe { std::env::set_var(KILO_CLI_PATH_ENV, v) },
            // SAFETY: test-only, single-threaded test runner.
            None => unsafe { std::env::remove_var(KILO_CLI_PATH_ENV) },
        }
    }

    #[test]
    fn should_forward_model_standard() {
        assert!(KiloCliModelProvider::should_forward_model("some-model"));
        assert!(KiloCliModelProvider::should_forward_model("gpt-4o"));
    }

    #[test]
    fn should_not_forward_default_model() {
        assert!(!KiloCliModelProvider::should_forward_model(
            DEFAULT_MODEL_MARKER
        ));
        assert!(!KiloCliModelProvider::should_forward_model(""));
        assert!(!KiloCliModelProvider::should_forward_model("   "));
    }

    #[test]
    fn validate_temperature_allows_defaults() {
        assert!(KiloCliModelProvider::validate_temperature(0.7).is_ok());
        assert!(KiloCliModelProvider::validate_temperature(1.0).is_ok());
    }

    #[test]
    fn validate_temperature_rejects_custom_value() {
        let err = KiloCliModelProvider::validate_temperature(0.2).unwrap_err();
        assert!(
            err.to_string()
                .contains("temperature unsupported by KiloCLI")
        );
    }

    #[tokio::test]
    async fn invoke_missing_binary_returns_error() {
        let model_provider = KiloCliModelProvider {
            binary_path: PathBuf::from("/nonexistent/path/to/kilo"),
        };
        let result = model_provider.invoke_cli("hello", "default").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn KiloCLI binary"),
            "unexpected error message: {msg}"
        );
    }
}
