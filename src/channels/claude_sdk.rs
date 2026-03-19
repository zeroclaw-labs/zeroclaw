//! Claude Code SDK integration.
//!
//! Spawns Claude Code sessions as child processes with proper hook support.
//! Parses stream-json output and relays progress to the orchestrator.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// A running Claude Code session.
pub struct ClaudeCodeSession {
    child: Child,
    pub run_id: String,
    pub worktree_path: Option<PathBuf>,
}

/// Progress event from a Claude Code session.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Text output from the assistant.
    AssistantText(String),
    /// Tool use started.
    ToolUse { name: String, input: String },
    /// Tool result received.
    ToolResult { name: String, output: String },
    /// Session completed successfully.
    Completed { result: String },
    /// Session encountered an error.
    Error(String),
}

/// Options for spawning a Claude Code session.
pub struct SpawnOptions {
    /// The prompt/task to execute.
    pub prompt: String,
    /// Optional system prompt context to append.
    pub system_context: Option<String>,
    /// Working directory for the session.
    pub work_dir: PathBuf,
    /// Optional model override.
    pub model: Option<String>,
    /// Maximum turns before the session stops.
    pub max_turns: Option<u32>,
}

impl ClaudeCodeSession {
    /// Spawn a new Claude Code session.
    ///
    /// Uses `--output-format stream-json` for structured output parsing.
    /// Does NOT use `--dangerously-skip-permissions` — hooks fire normally.
    #[allow(clippy::unused_async)]
    pub async fn spawn(run_id: String, opts: SpawnOptions) -> Result<Self> {
        let mut cmd = Command::new("claude");

        cmd.arg("-p").arg(&opts.prompt);
        cmd.arg("--output-format").arg("stream-json");

        if let Some(ref ctx) = opts.system_context {
            cmd.arg("--append-system-prompt").arg(ctx);
        }

        if let Some(ref model) = opts.model {
            cmd.arg("--model").arg(model);
        }

        if let Some(max_turns) = opts.max_turns {
            cmd.arg("--max-turns").arg(max_turns.to_string());
        }

        cmd.current_dir(&opts.work_dir);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Don't inherit stdin — non-interactive mode
        cmd.stdin(Stdio::null());

        let child = cmd.spawn().context("Failed to spawn Claude Code session")?;

        tracing::info!(run_id = %run_id, work_dir = %opts.work_dir.display(), "Claude Code session spawned");

        Ok(Self {
            child,
            run_id,
            worktree_path: Some(opts.work_dir),
        })
    }

    /// Stream progress events from the session.
    ///
    /// Reads stdout line-by-line, parsing stream-json format.
    /// Returns the final result when the session completes.
    pub async fn stream_progress(
        &mut self,
        mut on_event: impl FnMut(SessionEvent),
    ) -> Result<String> {
        let stdout = self
            .child
            .stdout
            .take()
            .context("No stdout handle available")?;

        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut last_text = String::new();

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(json) => {
                    let event = parse_stream_event(&json);
                    match &event {
                        SessionEvent::AssistantText(text) => {
                            last_text = text.clone();
                        }
                        SessionEvent::Completed { result } => {
                            last_text = result.clone();
                        }
                        _ => {}
                    }
                    on_event(event);
                }
                Err(_) => {
                    // Non-JSON output — treat as raw text
                    last_text = line.clone();
                    on_event(SessionEvent::AssistantText(line));
                }
            }
        }

        // Wait for the process to exit
        let status = self.child.wait().await?;

        if !status.success() {
            // Capture stderr
            let stderr = if let Some(mut stderr_handle) = self.child.stderr.take() {
                let mut buf = String::new();
                use tokio::io::AsyncReadExt;
                let _ = stderr_handle.read_to_string(&mut buf).await;
                buf
            } else {
                String::new()
            };

            if !stderr.is_empty() {
                on_event(SessionEvent::Error(stderr.clone()));
                anyhow::bail!("Claude Code exited with error: {stderr}");
            }
        }

        Ok(last_text)
    }

    /// Kill the session forcefully.
    pub async fn kill(&mut self) -> Result<()> {
        self.child.kill().await?;
        Ok(())
    }

    /// Cleanup worktree after session completes.
    #[allow(clippy::unused_async)]
    pub async fn cleanup(&self) -> Result<()> {
        // Worktree cleanup is handled by the caller (the orchestrator)
        // since it needs to inspect the result before deciding whether
        // to keep or remove the worktree.
        Ok(())
    }
}

/// Parse a stream-json event from Claude Code.
fn parse_stream_event(json: &serde_json::Value) -> SessionEvent {
    let event_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "assistant" => {
            let text = json
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| {
                    if let Some(arr) = c.as_array() {
                        arr.iter().find_map(|block| {
                            if block.get("type")?.as_str()? == "text" {
                                block.get("text")?.as_str().map(String::from)
                            } else {
                                None
                            }
                        })
                    } else {
                        c.as_str().map(String::from)
                    }
                })
                .unwrap_or_default();
            SessionEvent::AssistantText(text)
        }
        "tool_use" => {
            let name = json
                .get("name")
                .or_else(|| json.get("tool_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let input = json.get("input").map(|v| v.to_string()).unwrap_or_default();
            SessionEvent::ToolUse { name, input }
        }
        "tool_result" => {
            let name = json
                .get("name")
                .or_else(|| json.get("tool_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let output = json
                .get("output")
                .or_else(|| json.get("content"))
                .map(|v| {
                    if let Some(s) = v.as_str() {
                        s.to_string()
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_default();
            SessionEvent::ToolResult { name, output }
        }
        "result" => {
            let result = json
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            SessionEvent::Completed { result }
        }
        "error" => {
            let error = json
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
                .to_string();
            SessionEvent::Error(error)
        }
        _ => {
            // Unknown event type — extract any text content
            let text = json.to_string();
            SessionEvent::AssistantText(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_assistant_text_event() {
        let json = json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": "Hello world"}]
            }
        });
        match parse_stream_event(&json) {
            SessionEvent::AssistantText(text) => assert_eq!(text, "Hello world"),
            other => panic!("Expected AssistantText, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_use_event() {
        let json = json!({
            "type": "tool_use",
            "name": "Read",
            "input": {"file_path": "/tmp/test.rs"}
        });
        match parse_stream_event(&json) {
            SessionEvent::ToolUse { name, .. } => assert_eq!(name, "Read"),
            other => panic!("Expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn parse_result_event() {
        let json = json!({
            "type": "result",
            "result": "Task completed successfully"
        });
        match parse_stream_event(&json) {
            SessionEvent::Completed { result } => {
                assert_eq!(result, "Task completed successfully");
            }
            other => panic!("Expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn parse_error_event() {
        let json = json!({
            "type": "error",
            "error": "something went wrong"
        });
        match parse_stream_event(&json) {
            SessionEvent::Error(msg) => assert_eq!(msg, "something went wrong"),
            other => panic!("Expected Error, got {other:?}"),
        }
    }
}
