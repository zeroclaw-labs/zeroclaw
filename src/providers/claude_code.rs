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
use tokio::time::{Duration, timeout};

/// Environment variable for overriding the path to the `claude` binary.
pub const CLAUDE_CODE_PATH_ENV: &str = "CLAUDE_CODE_PATH";

/// Default `claude` binary name (resolved via `PATH`).
const DEFAULT_CLAUDE_CODE_BINARY: &str = "claude";

/// Model name used to signal "use the provider's own default model".
const DEFAULT_MODEL_MARKER: &str = "default";
/// Claude Code requests are bounded to avoid hung subprocesses.
/// Set higher than typical API timeouts to accommodate multi-turn tool loops.
const CLAUDE_CODE_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Avoid leaking oversized stderr payloads.
const MAX_CLAUDE_CODE_STDERR_CHARS: usize = 512;

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

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("Claude Code provider received non-finite temperature value");
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

    /// Invoke the claude binary with the given prompt, optional model, and optional
    /// system prompt override. Returns the trimmed stdout output as the assistant
    /// response.
    ///
    /// When `agent_mode` is true, enables `--dangerously-skip-permissions` so
    /// Claude Code can execute its built-in tools (Bash, Read, Edit, WebSearch,
    /// etc.) autonomously.  The response is extracted from the JSON `result`
    /// field when possible, falling back to raw stdout.
    async fn invoke_cli(
        &self,
        message: &str,
        model: &str,
        system_prompt: Option<&str>,
        agent_mode: bool,
    ) -> anyhow::Result<(String, Option<TokenUsage>)> {
        let mut cmd = Command::new(&self.binary_path);
        cmd.arg("--print");

        if agent_mode {
            cmd.arg("--dangerously-skip-permissions");
            cmd.arg("--output-format").arg("json");
        }

        if Self::should_forward_model(model) {
            cmd.arg("--model").arg(model);
        }

        if let Some(sp) = system_prompt {
            if !sp.is_empty() {
                cmd.arg("--append-system-prompt").arg(sp);
            }
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

        let raw = String::from_utf8(output.stdout)
            .map_err(|err| anyhow::anyhow!("Claude Code produced non-UTF-8 output: {err}"))?;

        if agent_mode {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
                let text = json
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();

                let usage = json.get("usage").map(|u| TokenUsage {
                    input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()),
                    output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()),
                    cached_input_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
                });

                return Ok((text, usage));
            }
        }

        Ok((raw.trim().to_string(), None))
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

        let (text, _usage) = self.invoke_cli(message, model, system_prompt, true).await?;
        Ok(text)
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        Self::validate_temperature(temperature)?;

        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let turns: Vec<&ChatMessage> = messages.iter().filter(|m| m.role != "system").collect();

        let user_message = if turns.len() <= 1 {
            turns.first().map(|m| m.content.clone()).unwrap_or_default()
        } else {
            let mut parts = Vec::new();
            for msg in &turns {
                let label = match msg.role.as_str() {
                    "user" => "[user]",
                    "assistant" => "[assistant]",
                    other => other,
                };
                parts.push(format!("{label}\n{}", msg.content));
            }
            parts.push("[assistant]".to_string());
            parts.join("\n\n")
        };

        let (text, _usage) = self.invoke_cli(&user_message, model, system, true).await?;
        Ok(text)
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        Self::validate_temperature(temperature)?;

        let system = request
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let turns: Vec<&ChatMessage> = request
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .collect();

        let user_message = if turns.len() <= 1 {
            turns.first().map(|m| m.content.clone()).unwrap_or_default()
        } else {
            let mut parts = Vec::new();
            for msg in &turns {
                let label = match msg.role.as_str() {
                    "user" => "[user]",
                    "assistant" => "[assistant]",
                    other => other,
                };
                parts.push(format!("{label}\n{}", msg.content));
            }
            parts.push("[assistant]".to_string());
            parts.join("\n\n")
        };

        let (text, usage) = self.invoke_cli(&user_message, model, system, true).await?;

        Ok(ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            usage: Some(usage.unwrap_or_default()),
            reasoning_content: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_util::env_lock;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;

    /// Serialize tests that spawn the echo-provider script.
    ///
    /// On Linux, writing a shell script and exec'ing it from parallel threads
    /// can trigger `ETXTBSY` ("Text file busy") even with unique file paths,
    /// because the kernel briefly holds `deny_write_access` on the interpreter
    /// page cache. Serializing these tests eliminates the race.
    ///
    /// Uses `tokio::sync::Mutex` so the guard can be held across `.await`.
    fn script_mutex() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[test]
    fn new_uses_env_override() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var(CLAUDE_CODE_PATH_ENV, "/usr/local/bin/claude") };
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("/usr/local/bin/claude"));
        match orig {
            // SAFETY: test-only, single-threaded test runner.
            Some(v) => unsafe { std::env::set_var(CLAUDE_CODE_PATH_ENV, v) },
            // SAFETY: test-only, single-threaded test runner.
            None => unsafe { std::env::remove_var(CLAUDE_CODE_PATH_ENV) },
        }
    }

    #[test]
    fn new_defaults_to_claude() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var(CLAUDE_CODE_PATH_ENV) };
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("claude"));
        if let Some(v) = orig {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var(CLAUDE_CODE_PATH_ENV, v) };
        }
    }

    #[test]
    fn new_ignores_blank_env_override() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var(CLAUDE_CODE_PATH_ENV, "   ") };
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("claude"));
        match orig {
            // SAFETY: test-only, single-threaded test runner.
            Some(v) => unsafe { std::env::set_var(CLAUDE_CODE_PATH_ENV, v) },
            // SAFETY: test-only, single-threaded test runner.
            None => unsafe { std::env::remove_var(CLAUDE_CODE_PATH_ENV) },
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
    fn validate_temperature_allows_any_finite_value() {
        assert!(ClaudeCodeProvider::validate_temperature(0.1).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(0.7).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(1.0).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(1.5).is_ok());
    }

    #[test]
    fn validate_temperature_rejects_non_finite() {
        assert!(ClaudeCodeProvider::validate_temperature(f64::NAN).is_err());
        assert!(ClaudeCodeProvider::validate_temperature(f64::INFINITY).is_err());
    }

    #[tokio::test]
    async fn invoke_missing_binary_returns_error() {
        let provider = ClaudeCodeProvider {
            binary_path: PathBuf::from("/nonexistent/path/to/claude"),
        };
        let result = provider.invoke_cli("hello", "default", None, false).await;
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
    /// Uses write-to-temp-then-rename to avoid ETXTBSY ("Text file busy")
    /// races: the final path is never open for writing when `execve()` runs.
    fn echo_provider() -> ClaudeCodeProvider {
        static SCRIPT_ID: AtomicUsize = AtomicUsize::new(0);
        let script_id = SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
        let final_path = dir.join(format!(
            "fake_claude_{}_{}.sh",
            std::process::id(),
            script_id
        ));
        // Write to a temporary file, then rename. This ensures the final
        // path was never opened for writing in this process, preventing
        // ETXTBSY when the kernel still holds an inode write reference.
        let tmp_path = dir.join(format!(
            ".tmp_fake_claude_{}_{}.sh",
            std::process::id(),
            script_id
        ));
        {
            let mut f = std::fs::File::create(&tmp_path).unwrap();
            writeln!(f, "#!/bin/sh\ncat /dev/stdin").unwrap();
            f.sync_all().unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::rename(&tmp_path, &final_path).unwrap();
        ClaudeCodeProvider {
            binary_path: final_path,
        }
    }

    #[test]
    fn echo_provider_uses_unique_script_paths() {
        let first = echo_provider();
        let second = echo_provider();
        assert_ne!(first.binary_path, second.binary_path);
    }

    #[tokio::test]
    async fn chat_with_history_single_user_message() {
        let _lock = script_mutex().lock().await;
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
        let _lock = script_mutex().lock().await;
        let provider = echo_provider();
        let messages = vec![
            ChatMessage::system("You are helpful."),
            ChatMessage::user("hello"),
        ];
        let result = provider
            .chat_with_history(&messages, "default", 1.0)
            .await
            .unwrap();
        // System prompt is passed via --append-system-prompt flag (not in stdin),
        // so the echo script only sees the user message.
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn chat_with_history_multi_turn_includes_all_messages() {
        let _lock = script_mutex().lock().await;
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
        // System prompt is passed via --append-system-prompt flag, not in stdin.
        assert!(!result.contains("[system]"));
        assert!(result.contains("[user]\nWhat is 2+2?"));
        assert!(result.contains("[assistant]\n4"));
        assert!(result.contains("[user]\nAnd 3+3?"));
        assert!(result.ends_with("[assistant]"));
    }

    #[tokio::test]
    async fn chat_with_history_multi_turn_without_system() {
        let _lock = script_mutex().lock().await;
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
    async fn chat_with_history_rejects_non_finite_temperature() {
        let _lock = script_mutex().lock().await;
        let provider = echo_provider();
        let messages = vec![ChatMessage::user("test")];
        let result = provider
            .chat_with_history(&messages, "default", f64::NAN)
            .await;
        assert!(result.is_err());
    }
}
