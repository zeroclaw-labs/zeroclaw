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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::providers::traits::{ChatMessage, ChatRequest, ChatResponse, Provider};

/// Maximum number of tracked sessions to prevent unbounded memory growth.
const MAX_SESSIONS: usize = 256;

/// Provider that delegates inference to the Codex CLI binary via subprocess.
///
/// Maintains a map of session keys to Codex thread IDs for session resume
/// across conversation turns. Sessions are persisted to disk so they survive
/// service restarts (the Codex server retains full conversation context
/// for resumed threads, avoiding misinterpretation from stale memory recall).
pub struct CodexCliProvider {
    codex_bin: String,
    sandbox_mode: String,
    /// Map of session key -> (thread_id, model).
    /// Session key is a stable conversation identity (e.g. "telegram/user123")
    /// passed via `ChatRequest.session_id` from the channel layer.
    /// Model is stored to invalidate sessions on model change.
    sessions: Mutex<HashMap<String, SessionEntry>>,
    /// Path to the sessions persistence file. `None` disables persistence.
    sessions_path: Option<PathBuf>,
}

/// A stored Codex CLI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionEntry {
    thread_id: String,
    model: String,
}

impl CodexCliProvider {
    /// Resolve the default sessions persistence path.
    ///
    /// Uses `ZEROCLAW_CODEX_SESSIONS` env var if set, otherwise falls back to
    /// `~/.zeroclaw/state/codex-sessions.json`.
    fn default_sessions_path() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("ZEROCLAW_CODEX_SESSIONS") {
            if path.is_empty() {
                return None; // Explicitly disabled.
            }
            return Some(PathBuf::from(path));
        }
        directories::UserDirs::new()
            .map(|u| u.home_dir().join(".zeroclaw").join("state").join("codex-sessions.json"))
    }

    /// Load sessions from a JSON file on disk. Returns empty map on any error.
    fn load_sessions_from_disk(path: &PathBuf) -> HashMap<String, SessionEntry> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => HashMap::new(),
        }
    }

    /// Create a new provider, reading configuration from environment variables.
    ///
    /// Loads persisted sessions from disk if available, enabling session resume
    /// across service restarts.
    pub fn new() -> Self {
        let sessions_path = Self::default_sessions_path();
        let sessions = sessions_path
            .as_ref()
            .map(Self::load_sessions_from_disk)
            .unwrap_or_default();

        if !sessions.is_empty() {
            tracing::info!(
                count = sessions.len(),
                "Codex CLI: loaded persisted sessions from disk"
            );
        }

        Self {
            codex_bin: std::env::var("ZEROCLAW_CODEX_BIN").unwrap_or_else(|_| "codex".into()),
            sandbox_mode: std::env::var("ZEROCLAW_CODEX_SANDBOX")
                .unwrap_or_else(|_| "read-only".into()),
            sessions: Mutex::new(sessions),
            sessions_path,
        }
    }

    /// Create a provider with a custom binary path (for testing, no persistence).
    pub fn new_for_test(codex_bin: impl Into<String>) -> Self {
        Self {
            codex_bin: codex_bin.into(),
            sandbox_mode: "read-only".into(),
            sessions: Mutex::new(HashMap::new()),
            sessions_path: None,
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

    /// Run a Codex CLI subprocess with streaming stdout, emitting progress
    /// events through the sender as they arrive.
    ///
    /// Returns (stdout_collected, exit_success) just like `run_codex`, but
    /// also sends formatted intermediate events to `on_progress`.
    async fn run_codex_streaming(
        &self,
        args: &[String],
        prompt: &str,
        on_progress: &tokio::sync::mpsc::Sender<String>,
    ) -> Result<(String, bool)> {
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

        // Write prompt to stdin.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .context("Failed to write prompt to Codex CLI stdin")?;
        }

        // Drain stderr concurrently to prevent pipe blocking.
        let stderr = child.stderr.take();
        let stderr_task = tokio::spawn(async move {
            let Some(stderr) = stderr else {
                return String::new();
            };
            let mut buf = String::new();
            let _ =
                tokio::io::AsyncReadExt::read_to_string(&mut BufReader::new(stderr), &mut buf)
                    .await;
            buf
        });

        // Read stdout line by line, emit progress for intermediate events.
        let mut collected = String::new();
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                // Collect for final parsing.
                collected.push_str(&line);
                collected.push('\n');

                // Try to parse and emit progress.
                if let Some(progress) = format_codex_event(&line) {
                    let _ = on_progress.send(progress).await;
                }
            }
        }

        // Wait for stderr drain.
        let stderr_output = stderr_task.await.unwrap_or_default();

        // Wait for process exit and check status.
        let status = child.wait().await.context("Failed to wait for Codex CLI")?;

        if !status.success() {
            let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
            tracing::warn!(
                code,
                stderr = %stderr_output.chars().take(200).collect::<String>(),
                "Codex CLI exited with non-zero status"
            );
            return Ok((collected, false));
        }

        Ok((collected, true))
    }

    /// Persist current session map to disk atomically (write-then-rename).
    /// No-op if persistence is disabled.
    fn persist_sessions(&self) {
        let Some(path) = &self.sessions_path else {
            return;
        };
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(data) = serde_json::to_string(&*sessions) else {
            return;
        };
        drop(sessions);

        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Atomic write: tmp file then rename.
        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, data.as_bytes()) {
            tracing::warn!(error = %e, "Failed to write codex sessions file");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            tracing::warn!(error = %e, "Failed to rename codex sessions file");
        }
    }

    /// Store a session entry, evicting oldest if at capacity.
    fn store_session(&self, key: &str, entry: SessionEntry) {
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if sessions.len() >= MAX_SESSIONS && !sessions.contains_key(key) {
                // Evict an arbitrary entry to stay bounded.
                if let Some(oldest_key) = sessions.keys().next().cloned() {
                    sessions.remove(&oldest_key);
                }
            }
            sessions.insert(key.to_string(), entry);
        }
        self.persist_sessions();
    }

    /// Look up a session entry for the given key and model.
    /// Returns None if no session exists or if the model has changed.
    fn get_session(&self, key: &str, model: &str) -> Option<String> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.get(key).and_then(|entry| {
            if entry.model == model {
                Some(entry.thread_id.clone())
            } else {
                None
            }
        })
    }

    /// Clear a session entry (e.g., on resume failure).
    fn clear_session(&self, key: &str) {
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.remove(key);
        }
        self.persist_sessions();
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

    /// Run a fresh codex exec with streaming progress. Returns (response_text, thread_id).
    async fn exec_fresh_streaming(
        &self,
        prompt: &str,
        model: &str,
        on_progress: &tokio::sync::mpsc::Sender<String>,
    ) -> Result<(String, Option<String>)> {
        let args = build_codex_exec_args(model, &self.sandbox_mode);
        let (stdout, success) = self.run_codex_streaming(&args, prompt, on_progress).await?;

        if !success {
            bail!("Codex CLI exited with non-zero status");
        }

        let thread_id = extract_thread_id(&stdout);
        let text = extract_last_agent_message(&stdout)?;
        Ok((text, thread_id))
    }

    /// Resume an existing codex session with streaming progress.
    async fn exec_resume_streaming(
        &self,
        thread_id: &str,
        prompt: &str,
        model: &str,
        on_progress: &tokio::sync::mpsc::Sender<String>,
    ) -> Result<String> {
        let args = build_codex_resume_args(thread_id, model, &self.sandbox_mode);
        let (stdout, success) = self.run_codex_streaming(&args, prompt, on_progress).await?;

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

/// Derive a session key from a session_id and model.
///
/// Combines the stable conversation identity (e.g. "telegram/user123") with
/// the model name to ensure session isolation per model.
/// Uses `\0` as delimiter to prevent collisions (neither session_id nor model
/// can contain null bytes in practice).
fn session_key(session_id: &str, model: &str) -> String {
    format!("{session_id}\0{model}")
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

/// Sanitize text for safe display in Telegram draft messages.
///
/// Strips HTML entities to prevent formatting issues in draft updates.
fn sanitize_for_draft(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .chars()
        .take(200) // Limit per-event progress length.
        .collect()
}

/// Format a Codex CLI JSONL event as a human-readable progress string.
///
/// Returns `Some(formatted_message)` for events worth showing to the user,
/// `None` for internal/ignored events. Safe gating: only whitelisted event
/// shapes produce output.
pub(crate) fn format_codex_event(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let event: Value = serde_json::from_str(line).ok()?;
    let event_type = event.get("type")?.as_str()?;

    match event_type {
        "event_msg" => {
            let payload = event.get("payload")?;

            // Skip internal metrics and prompt echoes.
            if let Some(
                "token_count" | "user_message" | "task_started" | "task_complete",
            ) = payload.get("type").and_then(Value::as_str)
            {
                return None;
            }

            // Extract human-readable commentary from the msg/message field.
            let msg = payload
                .get("msg")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)?;

            // Skip messages that look like structured JSON (not commentary).
            if msg.starts_with('{') {
                return None;
            }

            Some(format!("💬 {}\n", sanitize_for_draft(msg)))
        }

        "response_item" => {
            let item = event.get("payload")?.get("item")?;
            let item_type = item.get("type")?.as_str()?;

            if item_type == "function_call" {
                let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
                Some(format!("🔧 {}\n", sanitize_for_draft(name)))
            } else {
                None
            }
        }

        _ => None,
    }
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

        let (text, _thread_id) = self.exec_fresh(&prompt, model).await?;
        Ok(text)
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        _temperature: f64,
    ) -> Result<String> {
        // Stateless fallback: serialize full history as a single prompt.
        // Session resume is handled in chat() which has access to session_id.
        let prompt = serialize_history(messages);
        let (text, _thread_id) = self.exec_fresh(&prompt, model).await?;
        Ok(text)
    }

    /// Structured chat with session resume support.
    ///
    /// When `session_id` is provided (from channel layer), uses it to track
    /// Codex CLI thread IDs for server-side session resume. This gives Codex
    /// richer context without re-serializing the full conversation each turn.
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> Result<ChatResponse> {
        let messages = request.messages;

        // If no session_id, fall back to stateless behavior.
        let Some(sid) = request.session_id else {
            let text = self.chat_with_history(messages, model, temperature).await?;
            return Ok(ChatResponse {
                text: Some(text),
                tool_calls: Vec::new(),
                usage: None,
            });
        };

        let key = session_key(sid, model);

        // Extract the last user message for resume mode.
        let last_user = messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        // Try session resume if we have a stored thread_id.
        if let Some(thread_id) = self.get_session(&key, model) {
            tracing::info!(
                thread_id = %thread_id,
                "Codex CLI: resuming session"
            );

            match self.exec_resume(&thread_id, last_user, model).await {
                Ok(text) => {
                    return Ok(ChatResponse {
                        text: Some(text),
                        tool_calls: Vec::new(),
                        usage: None,
                    });
                }
                Err(err) => {
                    tracing::warn!(
                        thread_id = %thread_id,
                        error = %err,
                        "Codex CLI: resume failed, falling back to fresh exec"
                    );
                    self.clear_session(&key);
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
                &key,
                SessionEntry {
                    thread_id: tid,
                    model: model.to_string(),
                },
            );
        }

        Ok(ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            usage: None,
        })
    }

    fn supports_progress_streaming(&self) -> bool {
        true
    }

    async fn chat_with_progress(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        _temperature: f64,
        on_progress: &tokio::sync::mpsc::Sender<String>,
    ) -> Result<ChatResponse> {
        let messages = request.messages;

        // If no session_id, fall back to stateless streaming.
        let Some(sid) = request.session_id else {
            let prompt = serialize_history(messages);
            let (text, _thread_id) =
                self.exec_fresh_streaming(&prompt, model, on_progress).await?;
            return Ok(ChatResponse {
                text: Some(text),
                tool_calls: Vec::new(),
                usage: None,
            });
        };

        let key = session_key(sid, model);

        let last_user = messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        // Try session resume with streaming.
        if let Some(thread_id) = self.get_session(&key, model) {
            tracing::info!(
                thread_id = %thread_id,
                "Codex CLI: resuming session (streaming)"
            );

            match self
                .exec_resume_streaming(&thread_id, last_user, model, on_progress)
                .await
            {
                Ok(text) => {
                    return Ok(ChatResponse {
                        text: Some(text),
                        tool_calls: Vec::new(),
                        usage: None,
                    });
                }
                Err(err) => {
                    tracing::warn!(
                        thread_id = %thread_id,
                        error = %err,
                        "Codex CLI: streaming resume failed, falling back to fresh exec"
                    );
                    self.clear_session(&key);
                }
            }
        }

        // Fresh exec with streaming.
        let prompt = serialize_history(messages);
        let (text, thread_id) =
            self.exec_fresh_streaming(&prompt, model, on_progress).await?;

        if let Some(tid) = thread_id {
            self.store_session(
                &key,
                SessionEntry {
                    thread_id: tid,
                    model: model.to_string(),
                },
            );
        }

        Ok(ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            usage: None,
        })
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
        let k1 = session_key("telegram/user1", "gpt-5.3-codex-spark");
        let k2 = session_key("telegram/user1", "gpt-5.3-codex-spark");
        assert_eq!(k1, k2);
    }

    #[test]
    fn session_key_differs_by_model() {
        let k1 = session_key("telegram/user1", "gpt-5.3-codex-spark");
        let k2 = session_key("telegram/user1", "gpt-5.3-codex");
        assert_ne!(k1, k2);
    }

    #[test]
    fn session_key_differs_by_sender() {
        let k1 = session_key("telegram/user_a", "model");
        let k2 = session_key("telegram/user_b", "model");
        assert_ne!(k1, k2);
    }

    #[test]
    fn session_key_differs_by_channel() {
        let k1 = session_key("telegram/user1", "model");
        let k2 = session_key("discord/user1", "model");
        assert_ne!(k1, k2);
    }

    #[test]
    fn session_store_and_retrieve() {
        let provider = CodexCliProvider::new_for_test("fake");
        let key = session_key("telegram/user1", "model-a");
        provider.store_session(
            &key,
            SessionEntry {
                thread_id: "tid-1".into(),
                model: "model-a".into(),
            },
        );
        assert_eq!(provider.get_session(&key, "model-a"), Some("tid-1".into()));
    }

    #[test]
    fn session_invalidated_on_model_change() {
        let provider = CodexCliProvider::new_for_test("fake");
        let key = session_key("telegram/user1", "model-a");
        provider.store_session(
            &key,
            SessionEntry {
                thread_id: "tid-1".into(),
                model: "model-a".into(),
            },
        );
        // Same key but different model — should return None.
        assert_eq!(provider.get_session(&key, "model-b"), None);
    }

    #[test]
    fn session_clear() {
        let provider = CodexCliProvider::new_for_test("fake");
        let key = session_key("telegram/user1", "model-a");
        provider.store_session(
            &key,
            SessionEntry {
                thread_id: "tid-1".into(),
                model: "model-a".into(),
            },
        );
        provider.clear_session(&key);
        assert_eq!(provider.get_session(&key, "model-a"), None);
    }

    #[test]
    fn session_bounded_eviction() {
        let provider = CodexCliProvider::new_for_test("fake");
        // Fill to MAX_SESSIONS.
        for i in 0..MAX_SESSIONS {
            provider.store_session(
                &format!("ch/user-{i}"),
                SessionEntry {
                    thread_id: format!("tid-{i}"),
                    model: "m".into(),
                },
            );
        }
        // Adding one more should evict one.
        provider.store_session(
            "ch/user-new",
            SessionEntry {
                thread_id: "tid-new".into(),
                model: "m".into(),
            },
        );
        let sessions = provider.sessions.lock().unwrap();
        assert_eq!(sessions.len(), MAX_SESSIONS);
        assert!(sessions.contains_key("ch/user-new"));
    }

    #[test]
    fn session_key_no_collision_with_colon_in_parts() {
        // "a:b" + "c" must differ from "a" + "b:c" — null delimiter prevents ambiguity.
        let k1 = session_key("telegram/a:b", "c");
        let k2 = session_key("telegram/a", "b:c");
        assert_ne!(k1, k2);
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

    // --- Session persistence tests ---

    /// Helper: create a provider with persistence to a temp file.
    fn provider_with_persistence(path: PathBuf) -> CodexCliProvider {
        CodexCliProvider {
            codex_bin: "fake".into(),
            sandbox_mode: "read-only".into(),
            sessions: Mutex::new(HashMap::new()),
            sessions_path: Some(path),
        }
    }

    #[test]
    fn persist_and_reload_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");

        // Store a session and persist.
        let p1 = provider_with_persistence(path.clone());
        p1.store_session(
            "telegram/user1\0model-a",
            SessionEntry {
                thread_id: "tid-abc".into(),
                model: "model-a".into(),
            },
        );

        // Verify file exists.
        assert!(path.exists());

        // Reload from disk.
        let loaded = CodexCliProvider::load_sessions_from_disk(&path);
        assert_eq!(loaded.len(), 1);
        let entry = loaded.get("telegram/user1\0model-a").unwrap();
        assert_eq!(entry.thread_id, "tid-abc");
        assert_eq!(entry.model, "model-a");
    }

    #[test]
    fn clear_session_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");

        let provider = provider_with_persistence(path.clone());
        provider.store_session(
            "key1",
            SessionEntry {
                thread_id: "tid-1".into(),
                model: "m".into(),
            },
        );
        provider.store_session(
            "key2",
            SessionEntry {
                thread_id: "tid-2".into(),
                model: "m".into(),
            },
        );
        provider.clear_session("key1");

        let loaded = CodexCliProvider::load_sessions_from_disk(&path);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key("key2"));
        assert!(!loaded.contains_key("key1"));
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let path = PathBuf::from("/nonexistent/path/sessions.json");
        let loaded = CodexCliProvider::load_sessions_from_disk(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_corrupt_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        std::fs::write(&path, "not valid json{{{").unwrap();

        let loaded = CodexCliProvider::load_sessions_from_disk(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn no_persistence_without_path() {
        let provider = CodexCliProvider::new_for_test("fake");
        assert!(provider.sessions_path.is_none());

        // store_session should work in-memory without panicking.
        provider.store_session(
            "key",
            SessionEntry {
                thread_id: "tid".into(),
                model: "m".into(),
            },
        );
        assert_eq!(provider.get_session("key", "m"), Some("tid".into()));
    }

    #[test]
    fn session_entry_serialization_roundtrip() {
        let entry = SessionEntry {
            thread_id: "019c88e9-462f-7620".into(),
            model: "gpt-5.3-codex-spark".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let restored: SessionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.thread_id, entry.thread_id);
        assert_eq!(restored.model, entry.model);
    }

    // --- format_codex_event tests ---

    #[test]
    fn format_event_commentary() {
        let line = r#"{"type":"event_msg","payload":{"type":"status","msg":"Thinking about the problem..."}}"#;
        let result = format_codex_event(line).unwrap();
        assert!(result.starts_with("💬 "));
        assert!(result.contains("Thinking about the problem"));
    }

    #[test]
    fn format_event_tool_call() {
        let line = r#"{"type":"response_item","payload":{"item":{"type":"function_call","name":"file_read"}}}"#;
        let result = format_codex_event(line).unwrap();
        assert!(result.starts_with("🔧 "));
        assert!(result.contains("file_read"));
    }

    #[test]
    fn format_event_skips_token_count() {
        let line = r#"{"type":"event_msg","payload":{"type":"token_count","msg":"tokens: 500"}}"#;
        assert!(format_codex_event(line).is_none());
    }

    #[test]
    fn format_event_skips_user_message() {
        let line = r#"{"type":"event_msg","payload":{"type":"user_message","msg":"hello"}}"#;
        assert!(format_codex_event(line).is_none());
    }

    #[test]
    fn format_event_skips_task_started() {
        let line = r#"{"type":"event_msg","payload":{"type":"task_started","msg":"starting"}}"#;
        assert!(format_codex_event(line).is_none());
    }

    #[test]
    fn format_event_skips_task_complete() {
        let line = r#"{"type":"event_msg","payload":{"type":"task_complete","msg":"done"}}"#;
        assert!(format_codex_event(line).is_none());
    }

    #[test]
    fn format_event_skips_json_msg() {
        let line = r#"{"type":"event_msg","payload":{"msg":"{\"key\":\"value\"}"}}"#;
        assert!(format_codex_event(line).is_none());
    }

    #[test]
    fn format_event_skips_unknown_type() {
        let line = r#"{"type":"some_future_event","payload":{}}"#;
        assert!(format_codex_event(line).is_none());
    }

    #[test]
    fn format_event_skips_empty_line() {
        assert!(format_codex_event("").is_none());
        assert!(format_codex_event("   ").is_none());
    }

    #[test]
    fn format_event_skips_non_json() {
        assert!(format_codex_event("not json at all").is_none());
    }

    #[test]
    fn format_event_skips_non_function_response_item() {
        let line = r#"{"type":"response_item","payload":{"item":{"type":"message","text":"hello"}}}"#;
        assert!(format_codex_event(line).is_none());
    }

    #[test]
    fn format_event_uses_message_field_fallback() {
        let line = r#"{"type":"event_msg","payload":{"message":"Using alternate field"}}"#;
        let result = format_codex_event(line).unwrap();
        assert!(result.contains("Using alternate field"));
    }

    #[test]
    fn format_event_tool_without_name() {
        let line = r#"{"type":"response_item","payload":{"item":{"type":"function_call"}}}"#;
        let result = format_codex_event(line).unwrap();
        assert!(result.contains("tool"));
    }

    // --- sanitize_for_draft tests ---

    #[test]
    fn sanitize_escapes_html() {
        assert_eq!(sanitize_for_draft("<b>bold</b>"), "&lt;b&gt;bold&lt;/b&gt;");
    }

    #[test]
    fn sanitize_escapes_ampersand() {
        assert_eq!(sanitize_for_draft("a & b"), "a &amp; b");
    }

    #[test]
    fn sanitize_truncates_long_text() {
        let long = "x".repeat(300);
        let result = sanitize_for_draft(&long);
        assert_eq!(result.len(), 200);
    }

    #[test]
    fn sanitize_preserves_short_text() {
        assert_eq!(sanitize_for_draft("hello"), "hello");
    }

    // --- supports_progress_streaming test ---

    #[test]
    fn codex_cli_supports_progress_streaming() {
        let provider = CodexCliProvider::new_for_test("fake");
        assert!(provider.supports_progress_streaming());
    }
}
