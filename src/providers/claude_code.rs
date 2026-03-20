//! Claude Code headless CLI provider with streaming and session persistence.
//!
//! Integrates with the Claude Code CLI via `--output-format stream-json`,
//! reading events as they arrive. Tool calls are tracked for progress display,
//! sessions are persisted via `--resume`, and each response includes a status
//! line with session ID and context usage.
//!
//! # Directives
//!
//! The provider recognizes special directives prepended to the message:
//! - `[ZEROCLAW_CWD:/path]` — run Claude Code in the given working directory
//! - `[ZEROCLAW_SESSION_KEY:key]` — persist/resume sessions keyed by this identifier
//!
//! # Environment variables
//!
//! - `CLAUDE_CODE_PATH` — override the path to the `claude` binary (default: `"claude"`)

use crate::providers::traits::{ChatMessage, ChatRequest, ChatResponse, Provider, TokenUsage};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::Duration;

/// Environment variable for overriding the path to the `claude` binary.
pub const CLAUDE_CODE_PATH_ENV: &str = "CLAUDE_CODE_PATH";

/// Default `claude` binary name (resolved via `PATH`).
const DEFAULT_CLAUDE_CODE_BINARY: &str = "claude";

/// Model name used to signal "use the provider's own default model".
const DEFAULT_MODEL_MARKER: &str = "default";
/// 30 minutes allows complex multi-tool workflows to complete.
const CLAUDE_CODE_REQUEST_TIMEOUT: Duration = Duration::from_secs(1800);
/// Stale sessions should fail fast rather than burning the full timeout.
const CLAUDE_CODE_RESUME_TIMEOUT: Duration = Duration::from_secs(30);
/// Avoid leaking oversized stderr payloads.
const MAX_CLAUDE_CODE_STDERR_CHARS: usize = 512;
const CLAUDE_CODE_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// Collected metadata from a stream-json session.
#[derive(Debug, Default)]
struct StreamResult {
    result_text: String,
    session_id: Option<String>,
    is_error: bool,
    tool_calls: Vec<String>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cost_usd: f64,
    duration_ms: u64,
    num_turns: u64,
}

/// Provider that invokes the Claude Code CLI as a subprocess.
///
/// Uses `--output-format stream-json` to read events as they arrive,
/// tracking tool calls and session IDs for persistent conversations.
pub struct ClaudeCodeProvider {
    /// Path to the `claude` binary.
    binary_path: PathBuf,
    /// Maps session keys (e.g. room IDs) to Claude Code session IDs.
    sessions: Arc<Mutex<HashMap<String, String>>>,
    /// Optional path for persisting the session map across restarts.
    sessions_file: Option<PathBuf>,
}

impl ClaudeCodeProvider {
    pub fn new() -> Self {
        Self::with_state_dir(None)
    }

    /// Create a provider with an optional state directory for session persistence.
    ///
    /// When `state_dir` is provided, the session_key→session_id mapping is
    /// persisted to `{state_dir}/claude_code_sessions.json` so that `--resume`
    /// survives daemon restarts.
    pub fn with_state_dir(state_dir: Option<&std::path::Path>) -> Self {
        let binary_path = std::env::var(CLAUDE_CODE_PATH_ENV)
            .ok()
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CLAUDE_CODE_BINARY));

        let sessions_file = state_dir.map(|dir| dir.join("claude_code_sessions.json"));

        let sessions = sessions_file
            .as_ref()
            .and_then(|path| {
                let data = std::fs::read_to_string(path).ok()?;
                let map: HashMap<String, String> = serde_json::from_str(&data).ok()?;
                tracing::info!(
                    count = map.len(),
                    path = %path.display(),
                    "Loaded persisted Claude Code session mappings"
                );
                Some(map)
            })
            .unwrap_or_default();

        Self {
            binary_path,
            sessions: Arc::new(Mutex::new(sessions)),
            sessions_file,
        }
    }

    /// Returns true when the response text indicates the accumulated session
    /// context was too large for the model to process.
    fn is_context_overflow(response: &str) -> bool {
        let lower = response.to_lowercase();
        lower.contains("prompt is too long")
            || lower.contains("context length exceeded")
            || lower.contains("context window exceeded")
            || lower.contains("too many tokens")
    }

    /// Returns true when a lighter-weight model reported it lacks the capability
    /// to fulfill the request (tool access, shell, etc.). Only meaningful when
    /// a non-default model was used — the default model should not self-escalate.
    fn is_capability_failure(model: &str, response: &str) -> bool {
        if !Self::should_forward_model(model) {
            return false; // already at default tier, don't escalate
        }
        let lower = response.to_lowercase();
        lower.contains("don't have direct shell access")
            || lower.contains("don't have access to")
            || lower.contains("do not have access to")
            || lower.contains("unable to access")
            || lower.contains("cannot access")
            || lower.contains("can't access")
            || lower.contains("i'm not able to run")
            || lower.contains("i cannot run")
            || lower.contains("i can't run")
            || lower.contains("no access to")
            || lower.contains("blocked by security policy")
            || lower.contains("permission denied")
            || lower.contains("not permitted")
    }

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

    fn extract_cwd(message: &str) -> (Option<PathBuf>, &str) {
        if let Some(rest) = message.strip_prefix("[ZEROCLAW_CWD:") {
            if let Some(end) = rest.find(']') {
                let path = rest[..end].trim();
                if !path.is_empty() {
                    let remainder = rest[end + 1..].trim_start_matches('\n');
                    return (Some(PathBuf::from(path)), remainder);
                }
            }
        }
        (None, message)
    }

    fn extract_session_key(message: &str) -> (Option<String>, &str) {
        if let Some(rest) = message.strip_prefix("[ZEROCLAW_SESSION_KEY:") {
            if let Some(end) = rest.find(']') {
                let key = rest[..end].trim();
                if !key.is_empty() {
                    let remainder = rest[end + 1..].trim_start_matches('\n');
                    return (Some(key.to_string()), remainder);
                }
            }
        }
        (None, message)
    }

    fn extract_tmux_target(message: &str) -> (Option<String>, &str) {
        if let Some(rest) = message.strip_prefix("[ZEROCLAW_TMUX_TARGET:") {
            if let Some(end) = rest.find(']') {
                let target = rest[..end].trim();
                if !target.is_empty() {
                    let remainder = rest[end + 1..].trim_start_matches('\n');
                    return (Some(target.to_string()), remainder);
                }
            }
        }
        (None, message)
    }

    /// Check if a line looks like Claude Code's interactive prompt (idle, ready for input).
    fn is_claude_prompt_line(line: &str) -> bool {
        let trimmed = line.trim();
        // Claude Code uses ❯ (U+276F) as its prompt character
        trimmed == "\u{276F}"
            || trimmed.starts_with("\u{276F} ")
            || trimmed == ">"
            || trimmed.ends_with("> ")
            || (trimmed.ends_with('>') && !trimmed.contains('<') && !trimmed.contains("```"))
    }

    /// Capture the contents of a tmux pane (last 500 lines of scrollback).
    async fn tmux_capture_pane(target: &str) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("tmux")
            .args(["capture-pane", "-t", target, "-p", "-S", "-500"])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to capture tmux pane '{}': {}", target, e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "tmux capture-pane failed for '{}': {}",
                target,
                stderr.trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Send a message to a tmux pane. Uses `set-buffer` + `paste-buffer` for
    /// reliable delivery regardless of message length, then sends Enter.
    async fn tmux_send_keys(target: &str, message: &str) -> anyhow::Result<()> {
        // Collapse to a single line — Claude Code interactive mode accepts single-line input.
        let oneline = message.replace('\n', " ");

        // set-buffer sets the content directly (no file needed), then paste into target pane.
        let set = tokio::process::Command::new("tmux")
            .args(["set-buffer", &oneline])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("tmux set-buffer failed: {}", e))?;
        if !set.status.success() {
            let stderr = String::from_utf8_lossy(&set.stderr);
            anyhow::bail!("tmux set-buffer failed: {}", stderr.trim());
        }

        let paste = tokio::process::Command::new("tmux")
            .args(["paste-buffer", "-t", target])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("tmux paste-buffer failed: {}", e))?;
        if !paste.status.success() {
            let stderr = String::from_utf8_lossy(&paste.stderr);
            anyhow::bail!("tmux paste-buffer failed for '{}': {}", target, stderr.trim());
        }

        // Press Enter to submit
        let enter = tokio::process::Command::new("tmux")
            .args(["send-keys", "-t", target, "Enter"])
            .output()
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to send Enter to tmux pane '{}': {}", target, e)
            })?;
        if !enter.status.success() {
            let stderr = String::from_utf8_lossy(&enter.stderr);
            anyhow::bail!(
                "tmux send-keys Enter failed for '{}': {}",
                target,
                stderr.trim()
            );
        }
        Ok(())
    }

    /// Check if a line is Claude Code UI chrome (separator, status bar, etc.).
    fn is_chrome_line(line: &str) -> bool {
        let trimmed = line.trim();
        // Full-width separator lines (────) — only match lines that are purely ─ chars
        // and long enough to be a UI separator (not a table border which mixes │┼├ etc.)
        if trimmed.len() > 40
            && trimmed.chars().all(|c| c == '\u{2500}')
        {
            return true;
        }
        // Status bar lines (context %, timer, model info)
        if trimmed.contains("context]") || trimmed.contains("bypass permissions") {
            return true;
        }
        // Progress bar patterns
        if trimmed.contains('\u{2591}') || trimmed.contains('\u{2588}') {
            return true;
        }
        false
    }

    /// Extract Claude's response from before/after pane snapshots.
    fn extract_tmux_response(before: &str, after: &str, sent_message: &str) -> String {
        let after_lines: Vec<&str> = after.lines().collect();

        if after_lines.len() <= before.lines().count() {
            return String::new();
        }

        let sent_trimmed = sent_message.trim();

        // Find where the new content starts by looking for the echoed prompt line.
        // This is more reliable than counting lines since tmux may reuse screen space.
        let before_lines: Vec<&str> = before.lines().collect();
        let prompt_needle = format!("\u{276F} {}", sent_trimmed);
        let new_start = after_lines
            .iter()
            .rposition(|line| line.trim() == prompt_needle.trim())
            .map(|i| i + 1)
            .unwrap_or(before_lines.len());

        if new_start >= after_lines.len() {
            return String::new();
        }

        let new_lines = &after_lines[new_start..];

        // Filter out Claude Code UI chrome and extract just the response text.
        // Lines starting with ⏺ are Claude's output markers — keep the content after ⏺.
        let mut result: Vec<String> = Vec::new();
        for line in new_lines {
            let trimmed = line.trim();

            // Skip prompt lines, chrome, tool calls, and tool output — but don't break,
            // since Claude interleaves tool calls with response text.
            if Self::is_claude_prompt_line(trimmed) || Self::is_chrome_line(trimmed) {
                continue;
            }

            // Strip the ⏺ marker prefix
            if let Some(rest) = trimmed.strip_prefix('\u{23FA}') {
                let content = rest.trim();
                // Skip tool call lines like "Bash(command)"
                if content.starts_with("Bash(") || content.starts_with("Read(")
                    || content.starts_with("Write(") || content.starts_with("Edit(")
                    || content.starts_with("Glob(") || content.starts_with("Grep(")
                    || content.starts_with("Web Search(")
                    || content.starts_with("Searched for")
                {
                    continue;
                }
                if !content.is_empty() {
                    result.push(content.to_string());
                }
                continue;
            }

            // Skip tool output lines (⎿ prefix) and expand markers
            if trimmed.starts_with('\u{23BF}') || trimmed.contains("ctrl+o to expand") {
                continue;
            }
            // Skip +N lines markers and "Read N file" markers
            if trimmed.starts_with("\u{2026}") || trimmed.starts_with("… +") {
                continue;
            }
            if trimmed.starts_with("Read ") && trimmed.contains("file") {
                continue;
            }
            // Skip "Did N search" markers
            if trimmed.starts_with("Did ") && trimmed.contains("search") {
                continue;
            }

            result.push(trimmed.to_string());
        }

        // Trim trailing empty lines and any remaining chrome at the end
        while result.last().is_some_and(|l| {
            let t = l.trim();
            t.is_empty() || Self::is_claude_prompt_line(t) || Self::is_chrome_line(t)
        }) {
            result.pop();
        }

        result.join("\n").trim().to_string()
    }

    /// Route a message through an interactive Claude session in a tmux pane.
    async fn invoke_tmux(&self, target: &str, message: &str) -> anyhow::Result<String> {
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
        const RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

        // Snapshot pane before sending
        let before = Self::tmux_capture_pane(target).await?;

        // Send the prompt
        Self::tmux_send_keys(target, message).await?;
        tracing::info!(target, "Sent prompt to tmux pane");

        // Poll until response completes (prompt reappears and output stabilizes)
        let deadline = tokio::time::Instant::now() + RESPONSE_TIMEOUT;
        let mut last_snapshot = before.clone();
        let mut stable_count: u32 = 0;

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            if tokio::time::Instant::now() > deadline {
                anyhow::bail!(
                    "Tmux Claude session at '{}' did not respond within {:?}",
                    target,
                    RESPONSE_TIMEOUT,
                );
            }

            let snapshot = Self::tmux_capture_pane(target).await?;

            // Check if output has new content beyond the before snapshot
            let has_new_content = snapshot.lines().count() > before.lines().count();

            // Check if the prompt has reappeared near the end.
            // Claude Code's status bar occupies the last few lines, so check
            // the last 5 lines for the ❯ prompt.
            let ends_with_prompt = snapshot
                .trim_end()
                .lines()
                .rev()
                .take(5)
                .any(Self::is_claude_prompt_line);

            if has_new_content && ends_with_prompt && snapshot == last_snapshot {
                stable_count += 1;
                if stable_count >= 2 {
                    let response = Self::extract_tmux_response(&before, &snapshot, message);
                    tracing::info!(target, len = response.len(), "Captured tmux response");
                    return Ok(response);
                }
            } else {
                stable_count = 0;
            }

            last_snapshot = snapshot;
        }
    }

    fn get_session(&self, key: &str) -> Option<String> {
        self.sessions.lock().ok()?.get(key).cloned()
    }

    fn set_session(&self, key: String, session_id: String) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(key, session_id);
            self.persist_sessions(&sessions);
        }
    }

    fn clear_session(&self, key: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(key);
            self.persist_sessions(&sessions);
        }
    }

    /// Write the current session map to disk (best-effort, errors are logged).
    fn persist_sessions(&self, sessions: &HashMap<String, String>) {
        let Some(ref path) = self.sessions_file else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(path = %parent.display(), error = %e, "Failed to create sessions dir");
                return;
            }
        }
        match serde_json::to_string(sessions) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    tracing::warn!(path = %path.display(), error = %e, "Failed to persist session map");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialize session map");
            }
        }
    }

    /// Parse a single stream-json event line and update the result accumulator.
    fn process_stream_event(line: &str, result: &mut StreamResult) {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            return;
        };

        match event.get("type").and_then(|t| t.as_str()) {
            Some("result") => {
                result.result_text = event
                    .get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                result.session_id = event
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .map(String::from);
                result.is_error = event
                    .get("is_error")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false);
                result.cost_usd = event
                    .get("total_cost_usd")
                    .and_then(|c| c.as_f64())
                    .unwrap_or(0.0);
                result.duration_ms = event
                    .get("duration_ms")
                    .and_then(|d| d.as_u64())
                    .unwrap_or(0);
                result.num_turns = event.get("num_turns").and_then(|n| n.as_u64()).unwrap_or(0);

                if let Some(usage) = event.get("usage") {
                    result.input_tokens = usage
                        .get("input_tokens")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    result.output_tokens = usage
                        .get("output_tokens")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                    result.cache_read_tokens = usage
                        .get("cache_read_input_tokens")
                        .and_then(|t| t.as_u64())
                        .unwrap_or(0);
                }
            }
            Some("assistant") => {
                // Extract tool_use calls for progress tracking.
                if let Some(content) = event.pointer("/message/content").and_then(|c| c.as_array())
                {
                    for item in content {
                        if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                                result.tool_calls.push(name.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Format a human-readable token count (e.g. "45.2k").
    fn format_tokens(tokens: u64) -> String {
        if tokens >= 1_000_000 {
            format!("{:.1}M", tokens as f64 / 1_000_000.0)
        } else if tokens >= 1_000 {
            format!("{:.1}k", tokens as f64 / 1_000.0)
        } else {
            tokens.to_string()
        }
    }

    /// Build a status line from the stream result metadata.
    fn build_status_line(result: &StreamResult) -> String {
        let mut parts = Vec::new();

        if let Some(ref sid) = result.session_id {
            let short = if sid.len() > 8 { &sid[..8] } else { sid };
            parts.push(format!("session {short}"));
        }

        let total_tokens = result.input_tokens + result.output_tokens;
        if total_tokens > 0 {
            parts.push(format!(
                "{}in + {}out",
                Self::format_tokens(result.input_tokens),
                Self::format_tokens(result.output_tokens),
            ));
            if result.cache_read_tokens > 0 {
                parts.push(format!(
                    "{}cached",
                    Self::format_tokens(result.cache_read_tokens)
                ));
            }
        }

        if result.cost_usd > 0.0 {
            parts.push(format!("${:.4}", result.cost_usd));
        }

        if result.duration_ms > 0 {
            let secs = result.duration_ms as f64 / 1000.0;
            parts.push(format!("{secs:.1}s"));
        }

        if !result.tool_calls.is_empty() {
            parts.push(format!("{} tool calls", result.tool_calls.len()));
        }

        if parts.is_empty() {
            return String::new();
        }

        format!("\n\n---\n`{}`", parts.join(" | "))
    }

    /// Build a progress summary of tool calls made during the session.
    fn build_progress_summary(tool_calls: &[String]) -> String {
        if tool_calls.is_empty() {
            return String::new();
        }

        // Deduplicate and count.
        let mut counts: Vec<(String, usize)> = Vec::new();
        for name in tool_calls {
            if let Some(entry) = counts.iter_mut().find(|(n, _)| n == name) {
                entry.1 += 1;
            } else {
                counts.push((name.clone(), 1));
            }
        }

        let mut summary = String::from("**Tools used:** ");
        for (i, (name, count)) in counts.iter().enumerate() {
            if i > 0 {
                summary.push_str(", ");
            }
            if *count > 1 {
                write!(summary, "{name} x{count}").ok();
            } else {
                summary.push_str(name);
            }
        }
        summary.push_str("\n\n");
        summary
    }

    /// Format prior conversation turns as a text preamble for the prompt.
    ///
    /// Used when `--resume` is unavailable so the model still sees earlier
    /// messages as context rather than discarding them.
    fn format_history_as_context(messages: &[ChatMessage]) -> Option<String> {
        let turns: Vec<&ChatMessage> = messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .collect();

        // Nothing useful to prepend if there is at most one user message.
        if turns.len() <= 1 {
            return None;
        }

        // Everything except the final user message becomes context.
        let prior = &turns[..turns.len() - 1];
        if prior.is_empty() {
            return None;
        }

        let mut ctx = String::from("[Previous conversation]\n");
        for msg in prior {
            let label = if msg.role == "user" {
                "User"
            } else {
                "Assistant"
            };
            writeln!(ctx, "{label}: {}", msg.content).ok();
        }
        ctx.push_str("[End of previous conversation]\n\n");
        Some(ctx)
    }

    async fn invoke_cli(&self, message: &str, model: &str) -> anyhow::Result<String> {
        let (cwd, message) = Self::extract_cwd(message);
        let (session_key, message) = Self::extract_session_key(message);
        let (tmux_target, message) = Self::extract_tmux_target(message);

        // Route through tmux if configured for this channel.
        // chat_with_history already stripped system prompt/history for tmux messages.
        if let Some(ref target) = tmux_target {
            tracing::info!(
                target = target,
                msg_len = message.len(),
                msg_preview = &message[..message.len().min(100)],
                "Tmux routing: sending user message"
            );
            return self.invoke_tmux(target, message).await;
        }

        let resume_id = session_key.as_ref().and_then(|k| self.get_session(k));

        let resume_timeout = if resume_id.is_some() {
            CLAUDE_CODE_RESUME_TIMEOUT
        } else {
            CLAUDE_CODE_REQUEST_TIMEOUT
        };

        let result = self
            .invoke_cli_inner(
                message,
                model,
                cwd.as_ref(),
                resume_id.as_deref(),
                resume_timeout,
            )
            .await;

        // If --resume failed (hard error), clear stale session and retry without it.
        if result.is_err() && resume_id.is_some() {
            if let Some(ref key) = session_key {
                tracing::warn!(
                    session_key = key.as_str(),
                    "Claude Code --resume failed, clearing stale session and retrying"
                );
                self.clear_session(key);
            }
            let fresh = self
                .invoke_cli_inner(
                    message,
                    model,
                    cwd.as_ref(),
                    None,
                    CLAUDE_CODE_REQUEST_TIMEOUT,
                )
                .await?;
            return Ok(self.finalize_response(fresh, session_key.as_deref()));
        }

        let encoded = result?;

        // If the assigned model reported context overflow or a capability failure,
        // escalate to the default model and retry. Context overflow retries with
        // the same session; capability failure retries fresh (no session needed).
        if Self::is_capability_failure(model, &encoded) {
            tracing::warn!(
                model = model,
                "Capability failure detected, escalating to default model"
            );
            let escalated = self
                .invoke_cli_inner(
                    message,
                    DEFAULT_MODEL_MARKER,
                    cwd.as_ref(),
                    resume_id.as_deref(),
                    CLAUDE_CODE_RESUME_TIMEOUT,
                )
                .await?;
            return Ok(self.finalize_response(escalated, session_key.as_deref()));
        }

        if Self::is_context_overflow(&encoded) && resume_id.is_some() {
            tracing::warn!(
                model = model,
                "Context overflow with session resume, escalating to default model"
            );
            let escalated = self
                .invoke_cli_inner(
                    message,
                    DEFAULT_MODEL_MARKER,
                    cwd.as_ref(),
                    resume_id.as_deref(),
                    CLAUDE_CODE_RESUME_TIMEOUT,
                )
                .await
                .unwrap_or_default();

            if !Self::is_context_overflow(&escalated) {
                return Ok(self.finalize_response(escalated, session_key.as_deref()));
            }

            // Even the full model overflows — session is unrecoverable; clear and retry fresh.
            if let Some(ref key) = session_key {
                tracing::warn!(
                    session_key = key.as_str(),
                    "Session unrecoverable after escalation, clearing and retrying fresh"
                );
                self.clear_session(key);
            }
            let fresh = self
                .invoke_cli_inner(
                    message,
                    DEFAULT_MODEL_MARKER,
                    cwd.as_ref(),
                    None,
                    CLAUDE_CODE_REQUEST_TIMEOUT,
                )
                .await?;
            return Ok(self.finalize_response(fresh, session_key.as_deref()));
        }

        Ok(self.finalize_response(encoded, session_key.as_deref()))
    }

    /// Store session ID from the result and return the formatted response.
    fn finalize_response(&self, encoded: String, session_key: Option<&str>) -> String {
        // The encoded response may have [ZEROCLAW_NEW_SESSION:id] prefix.
        let mut response = encoded;
        if let Some(rest) = response.strip_prefix("[ZEROCLAW_NEW_SESSION:") {
            if let Some(end) = rest.find(']') {
                let new_id = rest[..end].trim().to_string();
                let remainder = rest[end + 1..].trim_start_matches('\n').to_string();
                if let Some(key) = session_key {
                    tracing::info!(
                        session_key = key,
                        session_id = new_id.as_str(),
                        "Claude Code session persisted for --resume"
                    );
                    self.set_session(key.to_string(), new_id);
                }
                response = remainder;
            }
        }
        response
    }

    /// Inner CLI invocation using stream-json for real-time event processing.
    async fn invoke_cli_inner(
        &self,
        message: &str,
        model: &str,
        cwd: Option<&PathBuf>,
        resume_session_id: Option<&str>,
        request_timeout: Duration,
    ) -> anyhow::Result<String> {
        let mut cmd = Command::new(&self.binary_path);
        cmd.arg("--print");
        cmd.arg("--verbose");
        cmd.arg("--dangerously-skip-permissions");
        cmd.arg("--output-format").arg("stream-json");

        if let Some(session_id) = resume_session_id {
            cmd.arg("--resume").arg(session_id);
        }

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        if Self::should_forward_model(model) {
            cmd.arg("--model").arg(model);
        }

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

        // Write prompt to stdin.
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(message.as_bytes()).await.map_err(|err| {
                anyhow::anyhow!("Failed to write prompt to Claude Code stdin: {err}")
            })?;
            stdin.shutdown().await.map_err(|err| {
                anyhow::anyhow!("Failed to finalize Claude Code stdin stream: {err}")
            })?;
        }

        // Read stdout line-by-line as stream-json events arrive.
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Claude Code process has no stdout handle"))?;
        let mut reader = BufReader::new(stdout).lines();
        let mut stream = StreamResult::default();
        let deadline = tokio::time::Instant::now() + request_timeout;

        loop {
            let line = tokio::select! {
                line = reader.next_line() => {
                    line.map_err(|err| anyhow::anyhow!("Error reading Claude Code stdout: {err}"))?
                }
                () = tokio::time::sleep_until(deadline) => {
                    child.kill().await.ok();
                    anyhow::bail!(
                        "Claude Code request timed out after {:?} (binary: {})",
                        request_timeout,
                        self.binary_path.display()
                    );
                }
            };

            let Some(line) = line else {
                break; // EOF — process exited.
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            Self::process_stream_event(trimmed, &mut stream);
        }

        // Wait for process to fully exit and check status.
        let status = child
            .wait()
            .await
            .map_err(|err| anyhow::anyhow!("Failed to wait for Claude Code process: {err}"))?;

        if !status.success() && stream.result_text.is_empty() {
            // Read stderr for diagnostics.
            let stderr = if let Some(mut stderr_handle) = child.stderr.take() {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut stderr_handle, &mut buf)
                    .await
                    .ok();
                buf
            } else {
                Vec::new()
            };
            let stderr_excerpt = Self::redact_stderr(&stderr);
            let stderr_note = if stderr_excerpt.is_empty() {
                String::new()
            } else {
                format!(" Stderr: {stderr_excerpt}")
            };
            anyhow::bail!(
                "Claude Code exited with non-zero status {}. \
                 Check that Claude Code is authenticated and the CLI is supported.{stderr_note}",
                status.code().unwrap_or(-1)
            );
        }

        // Build the response with progress summary and status line.
        let mut response = String::new();

        if !stream.tool_calls.is_empty() {
            response.push_str(&Self::build_progress_summary(&stream.tool_calls));
        }

        response.push_str(&stream.result_text);
        response.push_str(&Self::build_status_line(&stream));

        // Encode session ID for the caller to persist.
        if let Some(ref session_id) = stream.session_id {
            return Ok(format!("[ZEROCLAW_NEW_SESSION:{session_id}]\n{response}"));
        }

        Ok(response)
    }

    // Keep parse_output for backward compat / tests.
    fn parse_output(stdout: &str) -> (String, Option<String>) {
        #[derive(Deserialize)]
        struct JsonOutput {
            result: Option<String>,
            session_id: Option<String>,
        }
        let trimmed = stdout.trim();
        if let Ok(parsed) = serde_json::from_str::<JsonOutput>(trimmed) {
            (parsed.result.unwrap_or_default(), parsed.session_id)
        } else {
            (trimmed.to_string(), None)
        }
    }
}

impl Default for ClaudeCodeProvider {
    fn default() -> Self {
        Self::with_state_dir(None)
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

        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());
        let last_user = messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        // Peek at the last user message for a session key to decide if
        // --resume will be available.
        let (_, after_cwd) = Self::extract_cwd(last_user);
        let (session_key, _) = Self::extract_session_key(after_cwd);
        let has_resume = session_key
            .as_ref()
            .and_then(|k| self.get_session(k))
            .is_some();

        // Check if this message has a tmux target directive.
        // If so, send only the directives + raw user message (no system prompt/history).
        let has_tmux = system
            .map_or(false, |s| s.contains("[ZEROCLAW_TMUX_TARGET:"));
        if has_tmux {
            // Extract just the directives from the system prompt and pair with user message.
            let sys = system.unwrap_or("");
            let mut directives = String::new();
            for line in sys.lines() {
                if line.starts_with("[ZEROCLAW_") {
                    directives.push_str(line);
                    directives.push('\n');
                } else {
                    break;
                }
            }
            directives.push_str(last_user);
            return self.invoke_cli(&directives, model).await;
        }

        // When --resume is available, the CLI already has the full
        // conversation context — just send the latest message.
        // When it is not, prepend prior turns so the model sees history.
        let mut full_message = String::new();

        if let Some(sys) = system {
            if !sys.is_empty() {
                full_message.push_str(sys);
                full_message.push_str("\n\n");
            }
        }

        if !has_resume {
            if let Some(ctx) = Self::format_history_as_context(messages) {
                full_message.push_str(&ctx);
            }
        }

        full_message.push_str(last_user);
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
    use std::sync::OnceLock;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    // ── Constructor tests ──

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

    // ── Model forwarding ──

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

    // ── Temperature ──

    #[test]
    fn validate_temperature_allows_any_finite() {
        assert!(ClaudeCodeProvider::validate_temperature(0.7).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(1.0).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(0.1).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(0.2).is_ok());
    }

    // ── CWD directive ──

    #[test]
    fn extract_cwd_parses_directive() {
        let (cwd, rest) = ClaudeCodeProvider::extract_cwd(
            "[ZEROCLAW_CWD:/Users/dustin/projects/alpha]\nHello world",
        );
        assert_eq!(cwd.unwrap(), PathBuf::from("/Users/dustin/projects/alpha"));
        assert_eq!(rest, "Hello world");
    }

    #[test]
    fn extract_cwd_returns_none_without_directive() {
        let (cwd, rest) = ClaudeCodeProvider::extract_cwd("Hello world");
        assert!(cwd.is_none());
        assert_eq!(rest, "Hello world");
    }

    // ── Session key directive ──

    #[test]
    fn extract_session_key_parses_directive() {
        let (key, rest) = ClaudeCodeProvider::extract_session_key(
            "[ZEROCLAW_SESSION_KEY:!room123:server]\nHello world",
        );
        assert_eq!(key.unwrap(), "!room123:server");
        assert_eq!(rest, "Hello world");
    }

    #[test]
    fn extract_session_key_returns_none_without_directive() {
        let (key, rest) = ClaudeCodeProvider::extract_session_key("Hello world");
        assert!(key.is_none());
        assert_eq!(rest, "Hello world");
    }

    #[test]
    fn extract_session_key_ignores_empty_key() {
        let (key, rest) = ClaudeCodeProvider::extract_session_key("[ZEROCLAW_SESSION_KEY:]\nHello");
        assert!(key.is_none());
        assert_eq!(rest, "[ZEROCLAW_SESSION_KEY:]\nHello");
    }

    // ── Combined directive extraction ──

    #[test]
    fn extract_chained_directives() {
        let input = "[ZEROCLAW_CWD:/projects/a]\n[ZEROCLAW_SESSION_KEY:room1]\nWhat time is it?";
        let (cwd, rest) = ClaudeCodeProvider::extract_cwd(input);
        assert_eq!(cwd.unwrap(), PathBuf::from("/projects/a"));
        let (key, rest) = ClaudeCodeProvider::extract_session_key(rest);
        assert_eq!(key.unwrap(), "room1");
        assert_eq!(rest, "What time is it?");
    }

    // ── Stream event processing ──

    #[test]
    fn process_stream_event_result() {
        let event = r#"{"type":"result","subtype":"success","is_error":false,"result":"Hello world","session_id":"abc-123","total_cost_usd":0.0142,"duration_ms":8500,"num_turns":3,"usage":{"input_tokens":1500,"output_tokens":200,"cache_read_input_tokens":500}}"#;
        let mut result = StreamResult::default();
        ClaudeCodeProvider::process_stream_event(event, &mut result);
        assert_eq!(result.result_text, "Hello world");
        assert_eq!(result.session_id.as_deref(), Some("abc-123"));
        assert!(!result.is_error);
        assert_eq!(result.input_tokens, 1500);
        assert_eq!(result.output_tokens, 200);
        assert_eq!(result.cache_read_tokens, 500);
        assert!((result.cost_usd - 0.0142).abs() < 0.0001);
        assert_eq!(result.duration_ms, 8500);
        assert_eq!(result.num_turns, 3);
    }

    #[test]
    fn process_stream_event_tool_use() {
        let event = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","id":"t1"},{"type":"tool_use","name":"Read","id":"t2"}]}}"#;
        let mut result = StreamResult::default();
        ClaudeCodeProvider::process_stream_event(event, &mut result);
        assert_eq!(result.tool_calls, vec!["Bash", "Read"]);
    }

    #[test]
    fn process_stream_event_ignores_unknown() {
        let event = r#"{"type":"system","subtype":"init","session_id":"xyz"}"#;
        let mut result = StreamResult::default();
        ClaudeCodeProvider::process_stream_event(event, &mut result);
        assert!(result.result_text.is_empty());
        assert!(result.session_id.is_none()); // Only captured from "result" events.
    }

    #[test]
    fn process_stream_event_ignores_garbage() {
        let mut result = StreamResult::default();
        ClaudeCodeProvider::process_stream_event("not json at all", &mut result);
        assert!(result.result_text.is_empty());
    }

    // ── Status line formatting ──

    #[test]
    fn build_status_line_full() {
        let result = StreamResult {
            session_id: Some("abcdef12-3456-7890".into()),
            input_tokens: 45200,
            output_tokens: 1200,
            cache_read_tokens: 30000,
            cost_usd: 0.0523,
            duration_ms: 12400,
            tool_calls: vec!["Bash".into(), "Read".into(), "Bash".into()],
            ..Default::default()
        };
        let line = ClaudeCodeProvider::build_status_line(&result);
        assert!(line.contains("session abcdef12"));
        assert!(line.contains("45.2kin"));
        assert!(line.contains("1.2kout"));
        assert!(line.contains("30.0kcached"));
        assert!(line.contains("$0.0523"));
        assert!(line.contains("12.4s"));
        assert!(line.contains("3 tool calls"));
    }

    #[test]
    fn build_status_line_empty() {
        let result = StreamResult::default();
        let line = ClaudeCodeProvider::build_status_line(&result);
        assert!(line.is_empty());
    }

    // ── Progress summary ──

    #[test]
    fn build_progress_summary_deduplicates() {
        let calls = vec![
            "Bash".into(),
            "Read".into(),
            "Bash".into(),
            "Write".into(),
            "Bash".into(),
        ];
        let summary = ClaudeCodeProvider::build_progress_summary(&calls);
        assert!(summary.contains("Bash x3"));
        assert!(summary.contains("Read"));
        assert!(summary.contains("Write"));
    }

    #[test]
    fn build_progress_summary_empty() {
        let summary = ClaudeCodeProvider::build_progress_summary(&[]);
        assert!(summary.is_empty());
    }

    // ── Token formatting ──

    #[test]
    fn format_tokens_units() {
        assert_eq!(ClaudeCodeProvider::format_tokens(500), "500");
        assert_eq!(ClaudeCodeProvider::format_tokens(1500), "1.5k");
        assert_eq!(ClaudeCodeProvider::format_tokens(45200), "45.2k");
        assert_eq!(ClaudeCodeProvider::format_tokens(1_500_000), "1.5M");
    }

    // ── Session storage ──

    #[test]
    fn session_store_and_retrieve() {
        let provider = ClaudeCodeProvider::new();
        assert!(provider.get_session("room1").is_none());
        provider.set_session("room1".into(), "session-abc".into());
        assert_eq!(provider.get_session("room1").unwrap(), "session-abc");
        provider.set_session("room1".into(), "session-def".into());
        assert_eq!(provider.get_session("room1").unwrap(), "session-def");
    }

    #[test]
    fn session_clear_removes_entry() {
        let provider = ClaudeCodeProvider::new();
        provider.set_session("room1".into(), "session-abc".into());
        provider.clear_session("room1");
        assert!(provider.get_session("room1").is_none());
    }

    #[test]
    fn session_clear_nonexistent_is_noop() {
        let provider = ClaudeCodeProvider::new();
        provider.clear_session("nonexistent");
    }

    // ── Response finalization ──

    #[test]
    fn finalize_response_extracts_and_stores_session() {
        let provider = ClaudeCodeProvider::new();
        let response = provider.finalize_response(
            "[ZEROCLAW_NEW_SESSION:sess-xyz]\nHello world".into(),
            Some("room1"),
        );
        assert_eq!(response, "Hello world");
        assert_eq!(provider.get_session("room1").unwrap(), "sess-xyz");
    }

    #[test]
    fn finalize_response_strips_without_key() {
        let provider = ClaudeCodeProvider::new();
        let response =
            provider.finalize_response("[ZEROCLAW_NEW_SESSION:sess-xyz]\nHello world".into(), None);
        assert_eq!(response, "Hello world");
        assert!(provider.get_session("anything").is_none());
    }

    #[test]
    fn finalize_response_no_directive() {
        let provider = ClaudeCodeProvider::new();
        let response = provider.finalize_response("Hello world".into(), Some("room1"));
        assert_eq!(response, "Hello world");
        assert!(provider.get_session("room1").is_none());
    }

    // ── JSON output parsing (backward compat) ──

    #[test]
    fn parse_output_extracts_result_and_session() {
        let json = r#"{"type":"result","result":"Hello world","session_id":"abc-123"}"#;
        let (text, session_id) = ClaudeCodeProvider::parse_output(json);
        assert_eq!(text, "Hello world");
        assert_eq!(session_id.unwrap(), "abc-123");
    }

    #[test]
    fn parse_output_falls_back_to_raw_text() {
        let raw = "This is plain text, not JSON";
        let (text, session_id) = ClaudeCodeProvider::parse_output(raw);
        assert_eq!(text, raw);
        assert!(session_id.is_none());
    }

    // ── History context formatting ──

    #[test]
    fn format_history_single_user_message_returns_none() {
        let messages = vec![ChatMessage::user("Hello")];
        assert!(ClaudeCodeProvider::format_history_as_context(&messages).is_none());
    }

    #[test]
    fn format_history_no_messages_returns_none() {
        let messages: Vec<ChatMessage> = vec![];
        assert!(ClaudeCodeProvider::format_history_as_context(&messages).is_none());
    }

    #[test]
    fn format_history_includes_prior_turns() {
        let messages = vec![
            ChatMessage::user("What is Rust?"),
            ChatMessage::assistant("Rust is a systems programming language."),
            ChatMessage::user("Tell me more"),
        ];
        let ctx = ClaudeCodeProvider::format_history_as_context(&messages).unwrap();
        assert!(ctx.contains("[Previous conversation]"));
        assert!(ctx.contains("User: What is Rust?"));
        assert!(ctx.contains("Assistant: Rust is a systems programming language."));
        assert!(!ctx.contains("Tell me more"));
        assert!(ctx.contains("[End of previous conversation]"));
    }

    #[test]
    fn format_history_skips_system_messages() {
        let messages = vec![
            ChatMessage::system("Be helpful"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi"),
            ChatMessage::user("Bye"),
        ];
        let ctx = ClaudeCodeProvider::format_history_as_context(&messages).unwrap();
        assert!(!ctx.contains("Be helpful"));
        assert!(ctx.contains("User: Hello"));
        assert!(ctx.contains("Assistant: Hi"));
    }

    // ── Disk persistence ──

    #[test]
    fn session_persists_to_disk_and_reloads() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let provider = ClaudeCodeProvider::with_state_dir(Some(dir.path()));
        provider.set_session("room1".into(), "sess-aaa".into());
        provider.set_session("room2".into(), "sess-bbb".into());

        // Create a new provider from the same directory — should load persisted data.
        let provider2 = ClaudeCodeProvider::with_state_dir(Some(dir.path()));
        assert_eq!(provider2.get_session("room1").unwrap(), "sess-aaa");
        assert_eq!(provider2.get_session("room2").unwrap(), "sess-bbb");
    }

    #[test]
    fn session_clear_persists_removal() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let provider = ClaudeCodeProvider::with_state_dir(Some(dir.path()));
        provider.set_session("room1".into(), "sess-aaa".into());
        provider.clear_session("room1");

        let provider2 = ClaudeCodeProvider::with_state_dir(Some(dir.path()));
        assert!(provider2.get_session("room1").is_none());
    }

    #[test]
    fn with_state_dir_none_does_not_persist() {
        let provider = ClaudeCodeProvider::with_state_dir(None);
        provider.set_session("room1".into(), "sess-aaa".into());
        assert_eq!(provider.get_session("room1").unwrap(), "sess-aaa");
        // No file written — no crash, just in-memory.
        assert!(provider.sessions_file.is_none());
    }

    // ── CLI invocation ──

    #[tokio::test]
    async fn invoke_missing_binary_returns_error() {
        let provider = ClaudeCodeProvider {
            binary_path: PathBuf::from("/nonexistent/path/to/claude"),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            sessions_file: None,
        };
        let result = provider.invoke_cli("hello", "default").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn Claude Code binary"),
            "unexpected error message: {msg}"
        );
    }
}
