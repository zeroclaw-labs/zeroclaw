//! Codex CLI provider — shells out to the `codex` binary for inference.
//!
//! This provider wraps the OpenAI Codex CLI (`codex exec`) as a subprocess,
//! parsing its JSONL event stream to extract assistant responses. It enables
//! ZeroClaw to use a locally installed Codex CLI as a model backend without
//! requiring direct API credentials (the CLI manages its own auth).
//!
//! Configuration is via environment variables:
//! - `ZEROCLAW_CODEX_BIN`: path to the `codex` binary (default: `"codex"`)
//! - `ZEROCLAW_CODEX_SANDBOX`: sandbox mode passed to `--sandbox` (default: `"read-only"`)

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::providers::traits::Provider;

/// Provider that delegates inference to the Codex CLI binary via subprocess.
pub struct CodexCliProvider {
    codex_bin: String,
    sandbox_mode: String,
}

impl CodexCliProvider {
    /// Create a new provider, reading configuration from environment variables.
    pub fn new() -> Self {
        Self {
            codex_bin: std::env::var("ZEROCLAW_CODEX_BIN").unwrap_or_else(|_| "codex".into()),
            sandbox_mode: std::env::var("ZEROCLAW_CODEX_SANDBOX")
                .unwrap_or_else(|_| "read-only".into()),
        }
    }

    /// Create a provider with a custom binary path (test-only).
    #[cfg(test)]
    pub fn new_for_test(codex_bin: impl Into<String>) -> Self {
        Self {
            codex_bin: codex_bin.into(),
            sandbox_mode: "read-only".into(),
        }
    }
}

/// Build the argument list for `codex exec`.
///
/// Returns args suitable for spawning: `["exec", "--json", "--skip-git-repo-check",
/// "--sandbox", <sandbox>, "--model", <model>, "-"]`.
/// The trailing `-` tells codex to read the prompt from stdin.
pub(crate) fn build_codex_exec_args(model: &str, sandbox: &str) -> Vec<String> {
    vec![
        "exec".into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        "--sandbox".into(),
        sandbox.into(),
        "--model".into(),
        model.into(),
        "-".into(),
    ]
}

/// Extract the text of the last assistant message from Codex CLI JSONL output.
///
/// The Codex CLI emits newline-delimited JSON events. We look for events matching
/// either of two shapes:
///
/// 1. `{"type":"item.completed","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"..."}]}}`
/// 2. `{"type":"message","role":"assistant","content":[{"type":"output_text","text":"..."}]}`
///
/// Returns the `text` field from the **last** matching event.
pub(crate) fn extract_last_agent_message(output: &str) -> Result<String> {
    let mut last_text: Option<String> = None;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        // Try to extract from an item.completed wrapper first.
        if let Some(text) = try_extract_from_item_completed(&event) {
            last_text = Some(text);
            continue;
        }

        // Try the flat message shape.
        if let Some(text) = try_extract_from_message(&event) {
            last_text = Some(text);
        }
    }

    last_text.context("No assistant message found in Codex CLI output")
}

/// Try to extract text from an `item.completed` event wrapper.
fn try_extract_from_item_completed(event: &Value) -> Option<String> {
    let event_type = event.get("type")?.as_str()?;
    if event_type != "item.completed" {
        return None;
    }

    let item = event.get("item")?;
    extract_assistant_text(item)
}

/// Try to extract text from a top-level message event.
fn try_extract_from_message(event: &Value) -> Option<String> {
    let event_type = event.get("type")?.as_str()?;
    if event_type != "message" {
        return None;
    }

    extract_assistant_text(event)
}

/// Extract the output text from an assistant message object.
///
/// Expects: `{"type":"message","role":"assistant","content":[{"type":"output_text","text":"..."}]}`
fn extract_assistant_text(message: &Value) -> Option<String> {
    let role = message.get("role")?.as_str()?;
    if role != "assistant" {
        return None;
    }

    let content = message.get("content")?.as_array()?;
    for item in content {
        let content_type = item.get("type").and_then(Value::as_str);
        if content_type == Some("output_text") {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }
        }
    }

    None
}

#[async_trait]
impl Provider for CodexCliProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        _temperature: f64,
    ) -> Result<String> {
        // Build prompt: prepend system prompt as context if provided.
        let prompt = match system_prompt {
            Some(sys) if !sys.trim().is_empty() => {
                format!("[System instructions]\n{sys}\n\n[User message]\n{message}")
            }
            _ => message.to_string(),
        };

        let args = build_codex_exec_args(model, &self.sandbox_mode);

        let mut child = Command::new(&self.codex_bin)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn Codex CLI binary '{}'. Is it installed and in PATH?",
                    self.codex_bin
                )
            })?;

        // Write prompt to stdin, then close it.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .context("Failed to write prompt to Codex CLI stdin")?;
            // stdin is dropped here, closing the pipe.
        }

        let output = child
            .wait_with_output()
            .await
            .context("Failed to wait for Codex CLI process")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            bail!("Codex CLI exited with code {code}: {stderr}",);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        extract_last_agent_message(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_last_agent_message_from_jsonl_events() {
        let output = r#"{"type":"item.created","item":{"type":"message","role":"assistant"}}
{"type":"item.completed","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"first answer"}]}}
{"type":"item.completed","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"final answer"}]}}"#;
        let result = extract_last_agent_message(output).unwrap();
        assert_eq!(result, "final answer");
    }

    #[test]
    fn extract_returns_error_when_no_agent_message() {
        let output = r#"{"type":"item.created","item":{"type":"message","role":"user"}}"#;
        let err = extract_last_agent_message(output);
        assert!(err.is_err());
    }

    #[test]
    fn builds_codex_exec_args_with_model_and_json() {
        let args = build_codex_exec_args("gpt-5.3-codex-spark", "read-only");
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gpt-5.3-codex-spark".to_string()));
        assert!(args.contains(&"--skip-git-repo-check".to_string()));
        assert!(args.contains(&"--sandbox".to_string()));
        assert!(args.contains(&"read-only".to_string()));
    }

    #[test]
    fn builds_args_with_custom_sandbox() {
        let args = build_codex_exec_args("gpt-5.3-codex", "networking");
        assert!(args.contains(&"networking".to_string()));
    }

    #[test]
    fn new_uses_env_defaults() {
        let provider = CodexCliProvider::new();
        assert_eq!(
            provider.codex_bin,
            std::env::var("ZEROCLAW_CODEX_BIN").unwrap_or_else(|_| "codex".into())
        );
    }

    #[test]
    fn extract_handles_empty_output() {
        let err = extract_last_agent_message("");
        assert!(err.is_err());
    }

    #[test]
    fn extract_skips_non_completed_events() {
        let output = r#"{"type":"item.created","item":{"type":"message","role":"assistant"}}
{"type":"response.in_progress"}
{"type":"item.completed","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"the answer"}]}}"#;
        let result = extract_last_agent_message(output).unwrap();
        assert_eq!(result, "the answer");
    }

    #[test]
    fn extract_handles_flat_message_format() {
        let output = r#"{"type":"message","role":"assistant","content":[{"type":"output_text","text":"flat response"}]}"#;
        let result = extract_last_agent_message(output).unwrap();
        assert_eq!(result, "flat response");
    }

    #[test]
    fn build_codex_exec_args_has_stdin_dash() {
        let args = build_codex_exec_args("test-model", "read-only");
        assert_eq!(args.last().unwrap(), "-");
    }

    #[test]
    fn new_for_test_uses_custom_bin() {
        let provider = CodexCliProvider::new_for_test("/usr/local/bin/codex-dev");
        assert_eq!(provider.codex_bin, "/usr/local/bin/codex-dev");
        assert_eq!(provider.sandbox_mode, "read-only");
    }
}
