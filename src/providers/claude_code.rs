//! Claude Code headless CLI provider.
//!
//! Integrates with the Claude Code CLI, spawning the `claude` binary
//! as a subprocess for each inference request. This allows using Claude's AI
//! models without an interactive UI session.
//!
//! # Usage
//!
//! The `claude` binary must be available in `PATH`, or its location must be
//! set via the `CLAUDE_CODE_PATH` environment variable.
//!
//! Claude Code is invoked as:
//! ```text
//! claude --print -
//! ```
//! with prompt content written to stdin.
//!
//! # Limitations
//!
//! - **System prompt**: The system prompt is prepended to the user message with a
//!   blank-line separator, as the CLI does not provide a dedicated system-prompt flag.
//! - **Temperature**: The CLI does not expose a temperature parameter.
//!   Only default values are accepted; custom values return an explicit error.
//!
//! # Authentication
//!
//! Authentication is handled by Claude Code itself (its own credential store).
//! No explicit API key is required by this provider.
//!
//! # Environment variables
//!
//! - `CLAUDE_CODE_PATH` — override the path to the `claude` binary (default: `"claude"`)

use crate::providers::traits::{ChatMessage, ChatRequest, ChatResponse, Provider, TokenUsage};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Environment variable for overriding the path to the `claude` binary.
pub const CLAUDE_CODE_PATH_ENV: &str = "CLAUDE_CODE_PATH";

/// Default `claude` binary name (resolved via `PATH`).
const DEFAULT_CLAUDE_CODE_BINARY: &str = "claude";

/// Model name used to signal "use the provider's own default model".
const DEFAULT_MODEL_MARKER: &str = "default";
/// Claude Code requests are bounded to avoid hung subprocesses.
const CLAUDE_CODE_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Avoid leaking oversized stderr payloads.
const MAX_CLAUDE_CODE_STDERR_CHARS: usize = 512;
/// The CLI does not support sampling controls; allow only baseline defaults.
const CLAUDE_CODE_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// Provider that invokes the Claude Code CLI as a subprocess.
///
/// Each inference request spawns a fresh `claude` process. This is the
/// non-interactive approach: the process handles the prompt and exits.
pub struct ClaudeCodeProvider {
    /// Path to the `claude` binary.
    binary_path: PathBuf,
}

impl ClaudeCodeProvider {
    /// Create a new `ClaudeCodeProvider`.
    ///
    /// The binary path is resolved from `CLAUDE_CODE_PATH` env var if set,
    /// otherwise defaults to `"claude"` (found via `PATH`).
    pub fn new() -> Self {
        let binary_path = std::env::var(CLAUDE_CODE_PATH_ENV)
            .ok()
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CLAUDE_CODE_BINARY));

        Self { binary_path }
    }

    /// Returns true if the model argument should be forwarded to the CLI.
    fn should_forward_model(model: &str) -> bool {
        let trimmed = model.trim();
        !trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER
    }

    fn supports_temperature(temperature: f64) -> bool {
        CLAUDE_CODE_SUPPORTED_TEMPERATURES
            .iter()
            .any(|v| (temperature - v).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("Claude Code provider received non-finite temperature value");
        }
        if !Self::supports_temperature(temperature) {
            anyhow::bail!(
                "temperature unsupported by Claude Code CLI: {temperature}. \
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
        if trimmed.chars().count() <= MAX_CLAUDE_CODE_STDERR_CHARS {
            return trimmed.to_string();
        }
        let clipped: String = trimmed.chars().take(MAX_CLAUDE_CODE_STDERR_CHARS).collect();
        format!("{clipped}...")
    }

    /// Invoke the claude binary with the given prompt and optional model.
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
                "Failed to spawn Claude Code binary at {}: {err}. \
                 Ensure `claude` is installed and in PATH, or set CLAUDE_CODE_PATH.",
                self.binary_path.display()
            )
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(message.as_bytes()).await.map_err(|err| {
                anyhow::anyhow!("Failed to write prompt to Claude Code stdin: {err}")
            })?;
            stdin.shutdown().await.map_err(|err| {
                anyhow::anyhow!("Failed to finalize Claude Code stdin stream: {err}")
            })?;
        }

        let output = timeout(CLAUDE_CODE_REQUEST_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Claude Code request timed out after {:?} (binary: {})",
                    CLAUDE_CODE_REQUEST_TIMEOUT,
                    self.binary_path.display()
                )
            })?
            .map_err(|err| anyhow::anyhow!("Claude Code process failed: {err}"))?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr_excerpt = Self::redact_stderr(&output.stderr);
            let stderr_note = if stderr_excerpt.is_empty() {
                String::new()
            } else {
                format!(" Stderr: {stderr_excerpt}")
            };
            anyhow::bail!(
                "Claude Code exited with non-zero status {code}. \
                 Check that Claude Code is authenticated and the CLI is supported.{stderr_note}"
            );
        }

        let text = String::from_utf8(output.stdout)
            .map_err(|err| anyhow::anyhow!("Claude Code produced non-UTF-8 output: {err}"))?;

        Ok(text.trim().to_string())
    }
}

impl Default for ClaudeCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ClaudeCodeProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        Self::validate_temperature(temperature)?;

        let full_message = match system_prompt {
            Some(system) if !system.is_empty() => {
                format!("{system}\n\n{message}")
            }
            _ => message.to_string(),
        };

        self.invoke_cli(&full_message, model).await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        Self::validate_temperature(temperature)?;

        // Separate system prompt from conversation messages.
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        // Build conversation turns (skip system messages).
        let turns: Vec<&ChatMessage> = messages.iter().filter(|m| m.role != "system").collect();

        // If there's only one user message, use the simple path.
        if turns.len() <= 1 {
            let last_user = turns.first().map(|m| m.content.as_str()).unwrap_or("");
            let full_message = match system {
                Some(s) if !s.is_empty() => format!("{s}\n\n{last_user}"),
                _ => last_user.to_string(),
            };
            return self.invoke_cli(&full_message, model).await;
        }

        // Format multi-turn conversation into a single prompt.
        let mut parts = Vec::new();
        if let Some(s) = system {
            if !s.is_empty() {
                parts.push(format!("[system]\n{s}"));
            }
        }
        for msg in &turns {
            let label = match msg.role.as_str() {
                "user" => "[user]",
                "assistant" => "[assistant]",
                other => other,
            };
            parts.push(format!("{label}\n{}", msg.content));
        }
        parts.push("[assistant]".to_string());

        let full_message = parts.join("\n\n");
        self.invoke_cli(&full_message, model).await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
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
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    #[test]
    fn new_uses_env_override() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        std::env::set_var(CLAUDE_CODE_PATH_ENV, "/usr/local/bin/claude");
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("/usr/local/bin/claude"));
        match orig {
            Some(v) => std::env::set_var(CLAUDE_CODE_PATH_ENV, v),
            None => std::env::remove_var(CLAUDE_CODE_PATH_ENV),
        }
    }

    #[test]
    fn new_defaults_to_claude() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        std::env::remove_var(CLAUDE_CODE_PATH_ENV);
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("claude"));
        if let Some(v) = orig {
            std::env::set_var(CLAUDE_CODE_PATH_ENV, v);
        }
    }

    #[test]
    fn new_ignores_blank_env_override() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        std::env::set_var(CLAUDE_CODE_PATH_ENV, "   ");
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("claude"));
        match orig {
            Some(v) => std::env::set_var(CLAUDE_CODE_PATH_ENV, v),
            None => std::env::remove_var(CLAUDE_CODE_PATH_ENV),
        }
    }

    #[test]
    fn should_forward_model_standard() {
        assert!(ClaudeCodeProvider::should_forward_model(
            "claude-sonnet-4-20250514"
        ));
        assert!(ClaudeCodeProvider::should_forward_model(
            "claude-3.5-sonnet"
        ));
    }

    #[test]
    fn should_not_forward_default_model() {
        assert!(!ClaudeCodeProvider::should_forward_model(
            DEFAULT_MODEL_MARKER
        ));
        assert!(!ClaudeCodeProvider::should_forward_model(""));
        assert!(!ClaudeCodeProvider::should_forward_model("   "));
    }

    #[test]
    fn validate_temperature_allows_defaults() {
        assert!(ClaudeCodeProvider::validate_temperature(0.7).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(1.0).is_ok());
    }

    #[test]
    fn validate_temperature_rejects_custom_value() {
        let err = ClaudeCodeProvider::validate_temperature(0.2).unwrap_err();
        assert!(err
            .to_string()
            .contains("temperature unsupported by Claude Code CLI"));
    }

    #[tokio::test]
    async fn invoke_missing_binary_returns_error() {
        let provider = ClaudeCodeProvider {
            binary_path: PathBuf::from("/nonexistent/path/to/claude"),
        };
        let result = provider.invoke_cli("hello", "default").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn Claude Code binary"),
            "unexpected error message: {msg}"
        );
    }

    /// Helper: create a provider that uses a shell script echoing stdin back.
    /// The script ignores CLI flags (`--print`, `--model`, `-`) and just cats stdin.
    ///
    /// Creates a unique temp file per invocation to avoid race conditions when
    /// tests run in parallel. Each test gets its own script file, eliminating
    /// "Text file busy" (ETXTBSY) and "Broken pipe" (EPIPE) errors.
    fn echo_provider() -> ClaudeCodeProvider {
        use std::io::Write;

        let dir = std::env::temp_dir().join("zeroclaw_test_claude_code");
        std::fs::create_dir_all(&dir).unwrap();

        // Use thread ID + high-resolution timestamp for uniqueness
        let thread_id = std::thread::current().id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = dir.join(format!("fake_claude_{:?}_{}.sh", thread_id, nanos));

        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh\nexec cat /dev/stdin").unwrap();
        // Force sync to disk before marking executable
        f.sync_all().unwrap();
        drop(f);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        ClaudeCodeProvider { binary_path: path }
    }

    #[tokio::test]
    async fn chat_with_history_single_user_message() {
        let provider = echo_provider();
        let messages = vec![ChatMessage::user("hello")];
        let result = provider
            .chat_with_history(&messages, "default", 1.0)
            .await
            .unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn chat_with_history_single_user_with_system() {
        let provider = echo_provider();
        let messages = vec![
            ChatMessage::system("You are helpful."),
            ChatMessage::user("hello"),
        ];
        let result = provider
            .chat_with_history(&messages, "default", 1.0)
            .await
            .unwrap();
        assert_eq!(result, "You are helpful.\n\nhello");
    }

    #[tokio::test]
    async fn chat_with_history_multi_turn_includes_all_messages() {
        let provider = echo_provider();
        let messages = vec![
            ChatMessage::system("Be concise."),
            ChatMessage::user("What is 2+2?"),
            ChatMessage::assistant("4"),
            ChatMessage::user("And 3+3?"),
        ];
        let result = provider
            .chat_with_history(&messages, "default", 1.0)
            .await
            .unwrap();
        assert!(result.contains("[system]\nBe concise."));
        assert!(result.contains("[user]\nWhat is 2+2?"));
        assert!(result.contains("[assistant]\n4"));
        assert!(result.contains("[user]\nAnd 3+3?"));
        assert!(result.ends_with("[assistant]"));
    }

    #[tokio::test]
    async fn chat_with_history_multi_turn_without_system() {
        let provider = echo_provider();
        let messages = vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello"),
            ChatMessage::user("bye"),
        ];
        let result = provider
            .chat_with_history(&messages, "default", 1.0)
            .await
            .unwrap();
        assert!(!result.contains("[system]"));
        assert!(result.contains("[user]\nhi"));
        assert!(result.contains("[assistant]\nhello"));
        assert!(result.contains("[user]\nbye"));
    }

    #[tokio::test]
    async fn chat_with_history_rejects_bad_temperature() {
        let provider = echo_provider();
        let messages = vec![ChatMessage::user("test")];
        let result = provider.chat_with_history(&messages, "default", 0.5).await;
        assert!(result.is_err());
    }
}
