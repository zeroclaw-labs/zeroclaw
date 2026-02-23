//! Codex CLI provider — shells out to the `codex` binary for inference.
//!
//! This provider wraps the OpenAI Codex CLI (`codex exec`) as a subprocess,
//! parsing its JSONL event stream to extract assistant responses. It enables
//! ZeroClaw to use a locally installed Codex CLI as a model backend without
//! requiring direct API credentials (the CLI manages its own auth).
//!
//! ## Session resume
//!
//! When the provider receives multi-turn history via `chat_with_history`,
//! it captures the `thread_id` from the first Codex CLI invocation and
//! uses `codex exec resume <thread_id>` for subsequent turns. This gives
//! Codex server-side session state (token caching, richer context) without
//! re-serializing the full conversation each time.
//!
//! If a resume fails, the provider falls back to a fresh `codex exec` with
//! the full conversation serialized as formatted text.
//!
//! Configuration is via environment variables:
//! - `ZEROCLAW_CODEX_BIN`: path to the `codex` binary (default: `"codex"`)
//! - `ZEROCLAW_CODEX_SANDBOX`: sandbox mode passed to `--sandbox` (default: `"read-only"`)

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::process::Stdio;
use std::sync::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::providers::traits::{ChatMessage, Provider};

/// Maximum number of tracked sessions to prevent unbounded memory growth.
const MAX_SESSIONS: usize = 256;

/// Provider that delegates inference to the Codex CLI binary via subprocess.
///
/// Maintains an in-memory map of session keys to Codex thread IDs for
/// session resume across conversation turns.
pub struct CodexCliProvider {
    codex_bin: String,
    sandbox_mode: String,
    /// Map of session key -> (thread_id, model).
    /// Session key is derived from the system prompt hash.
    /// Model is stored to invalidate sessions on model change.
    sessions: Mutex<HashMap<u64, SessionEntry>>,
}

/// A stored Codex CLI session.
#[derive(Debug, Clone)]
struct SessionEntry {
    thread_id: String,
    model: String,
}

impl CodexCliProvider {
    /// Create a new provider, reading configuration from environment variables.
    pub fn new() -> Self {
        Self {
            codex_bin: std::env::var("ZEROCLAW_CODEX_BIN").unwrap_or_else(|_| "codex".into()),
            sandbox_mode: std::env::var("ZEROCLAW_CODEX_SANDBOX")
                .unwrap_or_else(|_| "read-only".into()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Create a provider with a custom binary path (for testing).
    pub fn new_for_test(codex_bin: impl Into<String>) -> Self {
        Self {
            codex_bin: codex_bin.into(),
            sandbox_mode: "read-only".into(),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Run a Codex CLI subprocess and return (stdout, exit_success).
    async fn run_codex(&self, args: &[String], prompt: &str) -> Result<(String, bool)> {
        let mut child = Command::new(&self.codex_bin)
            .args(args)
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

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .context("Failed to write prompt to Codex CLI stdin")?;
        }

        let output = child
            .wait_with_output()
            .await
            .context("Failed to wait for Codex CLI process")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            tracing::warn!(
                code,
                stderr = %stderr.chars().take(200).collect::<String>(),
                "Codex CLI exited with non-zero status"
            );
            return Ok((stdout, false));
        }

        Ok((stdout, true))
    }

    /// Store a session entry, evicting oldest if at capacity.
    fn store_session(&self, key: u64, entry: SessionEntry) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.len() >= MAX_SESSIONS && !sessions.contains_key(&key) {
            // Evict an arbitrary entry to stay bounded.
            if let Some(&oldest_key) = sessions.keys().next() {
                sessions.remove(&oldest_key);
            }
        }
        sessions.insert(key, entry);
    }

    /// Look up a session entry for the given key and model.
    /// Returns None if no session exists or if the model has changed.
    fn get_session(&self, key: u64, model: &str) -> Option<String> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.get(&key).and_then(|entry| {
            if entry.model == model {
                Some(entry.thread_id.clone())
            } else {
                None
            }
        })
    }

    /// Clear a session entry (e.g., on resume failure).
    fn clear_session(&self, key: u64) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.remove(&key);
    }

    /// Run a fresh codex exec with the given prompt. Returns (response_text, thread_id).
    async fn exec_fresh(&self, prompt: &str, model: &str) -> Result<(String, Option<String>)> {
        let args = build_codex_exec_args(model, &self.sandbox_mode);
        let (stdout, success) = self.run_codex(&args, prompt).await?;

        if !success {
            bail!("Codex CLI exited with non-zero status");
        }

        let thread_id = extract_thread_id(&stdout);
        let text = extract_last_agent_message(&stdout)?;
        Ok((text, thread_id))
    }

    /// Resume an existing codex session with a new prompt.
    /// Returns Ok(response_text) on success, Err on failure.
    async fn exec_resume(
        &self,
        thread_id: &str,
        prompt: &str,
        model: &str,
    ) -> Result<String> {
        let args = build_codex_resume_args(thread_id, model, &self.sandbox_mode);
        let (stdout, success) = self.run_codex(&args, prompt).await?;

        if !success {
            bail!("Codex CLI resume exited with non-zero status");
        }

        extract_last_agent_message(&stdout)
    }
}

/// Build the argument list for `codex exec`.
pub(crate) fn build_codex_exec_args(model: &str, sandbox: &str) -> Vec<String> {
    vec![
        "exec".into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        "--full-auto".into(),
        "--sandbox".into(),
        sandbox.into(),
        "--model".into(),
        model.into(),
        "-".into(),
    ]
}

/// Build the argument list for `codex exec resume <thread_id>`.
pub(crate) fn build_codex_resume_args(
    thread_id: &str,
    model: &str,
    sandbox: &str,
) -> Vec<String> {
    vec![
        "exec".into(),
        "resume".into(),
        thread_id.into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        "--full-auto".into(),
        "--sandbox".into(),
        sandbox.into(),
        "--model".into(),
        model.into(),
        "-".into(),
    ]
}

/// Extract the thread_id from Codex CLI JSONL output.
///
/// Scans all lines for `{"type":"thread.started","thread_id":"..."}`.
pub(crate) fn extract_thread_id(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) == Some("thread.started") {
            if let Some(tid) = event.get("thread_id").and_then(Value::as_str) {
                return Some(tid.to_string());
            }
        }
    }
    None
}

/// Extract the text of the last assistant message from Codex CLI JSONL output.
///
/// The Codex CLI emits newline-delimited JSON events. We look for events matching
/// either of two shapes:
///
/// 1. `{"type":"item.completed","item":{"type":"agent_message","text":"..."}}`
/// 2. `{"type":"item.completed","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"..."}]}}`
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
///
/// Handles two Codex CLI output formats:
/// 1. `agent_message`: `{"type":"item.completed","item":{"type":"agent_message","text":"..."}}`
/// 2. `message`: `{"type":"item.completed","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"..."}]}}`
fn try_extract_from_item_completed(event: &Value) -> Option<String> {
    let event_type = event.get("type")?.as_str()?;
    if event_type != "item.completed" {
        return None;
    }

    let item = event.get("item")?;

    // Format 1: agent_message with flat text field (current Codex CLI default).
    if let Some(item_type) = item.get("type").and_then(Value::as_str) {
        if item_type == "agent_message" {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }
        }
    }

    // Format 2: message with role + content array (legacy/alternative format).
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

/// Derive a session key from the system prompt and model.
///
/// Uses a hash so the key is compact and deterministic. The system prompt
/// is unique per conversation in ZeroClaw (it contains user/channel context).
fn session_key(system_prompt: Option<&str>, model: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    system_prompt.unwrap_or("").hash(&mut hasher);
    model.hash(&mut hasher);
    hasher.finish()
}

/// Serialize a multi-turn conversation as formatted text for a fresh codex exec.
///
/// Produces a deterministic text representation of the conversation history
/// suitable for passing as a single prompt to the Codex CLI.
fn serialize_history(messages: &[ChatMessage]) -> String {
    let mut parts = Vec::new();

    for msg in messages {
        let label = match msg.role.as_str() {
            "system" => "[System]",
            "user" => "[User]",
            "assistant" => "[Assistant]",
            "tool" => "[Tool Result]",
            _ => "[Unknown]",
        };
        parts.push(format!("{label}\n{}", msg.content));
    }

    parts.join("\n\n")
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

        let (text, thread_id) = self.exec_fresh(&prompt, model).await?;

        // Store thread_id for potential future resume.
        if let Some(tid) = thread_id {
            let key = session_key(system_prompt, model);
            self.store_session(
                key,
                SessionEntry {
                    thread_id: tid,
                    model: model.to_string(),
                },
            );
        }

        Ok(text)
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        _temperature: f64,
    ) -> Result<String> {
        // Extract system prompt for session key derivation.
        let system_prompt = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let key = session_key(system_prompt, model);

        // Extract the last user message for resume mode.
        let last_user = messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        // Try session resume if we have a stored thread_id.
        if let Some(thread_id) = self.get_session(key, model) {
            tracing::info!(
                thread_id = %thread_id,
                "Codex CLI: resuming session"
            );

            match self.exec_resume(&thread_id, last_user, model).await {
                Ok(text) => return Ok(text),
                Err(err) => {
                    tracing::warn!(
                        thread_id = %thread_id,
                        error = %err,
                        "Codex CLI: resume failed, falling back to fresh exec"
                    );
                    self.clear_session(key);
                    // Fall through to fresh exec below.
                }
            }
        }

        // Fresh exec: serialize full conversation history.
        let prompt = serialize_history(messages);
        let (text, thread_id) = self.exec_fresh(&prompt, model).await?;

        // Store thread_id for next turn.
        if let Some(tid) = thread_id {
            self.store_session(
                key,
                SessionEntry {
                    thread_id: tid,
                    model: model.to_string(),
                },
            );
        }

        Ok(text)
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
    fn parses_agent_message_format() {
        // This is the actual format the Codex CLI produces.
        let output = r#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"Hello! How can I help?"}}
{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20}}"#;
        let result = extract_last_agent_message(output).unwrap();
        assert_eq!(result, "Hello! How can I help?");
    }

    #[test]
    fn parses_multiple_agent_messages_returns_last() {
        let output = r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"First thought"}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Final answer"}}"#;
        let result = extract_last_agent_message(output).unwrap();
        assert_eq!(result, "Final answer");
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

    // --- Session resume tests ---

    #[test]
    fn extract_thread_id_from_first_event() {
        let output = r#"{"type":"thread.started","thread_id":"019c883b-52fd-71d1-bad2-24433d944f25"}
{"type":"turn.started"}
{"type":"item.completed","item":{"type":"agent_message","text":"Hi"}}"#;
        let tid = extract_thread_id(output).unwrap();
        assert_eq!(tid, "019c883b-52fd-71d1-bad2-24433d944f25");
    }

    #[test]
    fn extract_thread_id_not_first_line() {
        let output = r#"some garbage line
{"type":"turn.started"}
{"type":"thread.started","thread_id":"abc-123"}
{"type":"item.completed","item":{"type":"agent_message","text":"Hi"}}"#;
        let tid = extract_thread_id(output).unwrap();
        assert_eq!(tid, "abc-123");
    }

    #[test]
    fn extract_thread_id_missing() {
        let output = r#"{"type":"turn.started"}
{"type":"item.completed","item":{"type":"agent_message","text":"Hi"}}"#;
        assert!(extract_thread_id(output).is_none());
    }

    #[test]
    fn session_key_deterministic() {
        let k1 = session_key(Some("Be helpful"), "gpt-5.3-codex-spark");
        let k2 = session_key(Some("Be helpful"), "gpt-5.3-codex-spark");
        assert_eq!(k1, k2);
    }

    #[test]
    fn session_key_differs_by_model() {
        let k1 = session_key(Some("Be helpful"), "gpt-5.3-codex-spark");
        let k2 = session_key(Some("Be helpful"), "gpt-5.3-codex");
        assert_ne!(k1, k2);
    }

    #[test]
    fn session_key_differs_by_prompt() {
        let k1 = session_key(Some("You are agent A"), "model");
        let k2 = session_key(Some("You are agent B"), "model");
        assert_ne!(k1, k2);
    }

    #[test]
    fn session_key_none_prompt() {
        let k1 = session_key(None, "model");
        let k2 = session_key(Some(""), "model");
        assert_eq!(k1, k2);
    }

    #[test]
    fn session_store_and_retrieve() {
        let provider = CodexCliProvider::new_for_test("fake");
        let key = session_key(Some("sys"), "model-a");
        provider.store_session(
            key,
            SessionEntry {
                thread_id: "tid-1".into(),
                model: "model-a".into(),
            },
        );
        assert_eq!(provider.get_session(key, "model-a"), Some("tid-1".into()));
    }

    #[test]
    fn session_invalidated_on_model_change() {
        let provider = CodexCliProvider::new_for_test("fake");
        let key = session_key(Some("sys"), "model-a");
        provider.store_session(
            key,
            SessionEntry {
                thread_id: "tid-1".into(),
                model: "model-a".into(),
            },
        );
        // Same key but different model — should return None.
        assert_eq!(provider.get_session(key, "model-b"), None);
    }

    #[test]
    fn session_clear() {
        let provider = CodexCliProvider::new_for_test("fake");
        let key = session_key(Some("sys"), "model-a");
        provider.store_session(
            key,
            SessionEntry {
                thread_id: "tid-1".into(),
                model: "model-a".into(),
            },
        );
        provider.clear_session(key);
        assert_eq!(provider.get_session(key, "model-a"), None);
    }

    #[test]
    fn session_bounded_eviction() {
        let provider = CodexCliProvider::new_for_test("fake");
        // Fill to MAX_SESSIONS.
        for i in 0..MAX_SESSIONS {
            provider.store_session(
                i as u64,
                SessionEntry {
                    thread_id: format!("tid-{i}"),
                    model: "m".into(),
                },
            );
        }
        // Adding one more should evict one.
        provider.store_session(
            9999,
            SessionEntry {
                thread_id: "tid-new".into(),
                model: "m".into(),
            },
        );
        let sessions = provider.sessions.lock().unwrap();
        assert_eq!(sessions.len(), MAX_SESSIONS);
        assert!(sessions.contains_key(&9999));
    }

    #[test]
    fn serialize_history_basic() {
        let messages = vec![
            ChatMessage::system("Be helpful"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there!"),
            ChatMessage::user("How are you?"),
        ];
        let serialized = serialize_history(&messages);
        assert!(serialized.contains("[System]\nBe helpful"));
        assert!(serialized.contains("[User]\nHello"));
        assert!(serialized.contains("[Assistant]\nHi there!"));
        assert!(serialized.contains("[User]\nHow are you?"));
    }

    #[test]
    fn serialize_history_empty() {
        let serialized = serialize_history(&[]);
        assert!(serialized.is_empty());
    }

    #[test]
    fn build_resume_args_includes_thread_id() {
        let args = build_codex_resume_args("tid-123", "gpt-5.3-codex-spark", "read-only");
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"tid-123".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gpt-5.3-codex-spark".to_string()));
        assert!(args.contains(&"-".to_string()));
    }
}
