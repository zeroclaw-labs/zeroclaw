//! Session-to-session messaging tools for inter-agent communication.
//!
//! Provides three tools:
//! - `sessions_list` — list active sessions with metadata
//! - `sessions_history` — read message history from a specific session
//! - `sessions_send` — send a message to a specific session

use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_infra::session_backend::SessionBackend;

/// Validate that a session ID is non-empty and contains at least one
/// alphanumeric character (prevents blank keys after sanitization).
fn validate_session_id(session_id: &str) -> Result<(), ToolResult> {
    let trimmed = session_id.trim();
    if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return Err(ToolResult {
            success: false,
            output: String::new(),
            error: Some(
                "Invalid 'session_id': must be non-empty and contain at least one alphanumeric character.".into(),
            ),
        });
    }
    Ok(())
}

// ── SessionsListTool ────────────────────────────────────────────────

/// Lists active sessions with their channel, last activity time, and message count.
pub struct SessionsListTool {
    backend: Arc<dyn SessionBackend>,
}

impl SessionsListTool {
    pub fn new(backend: Arc<dyn SessionBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for SessionsListTool {
    fn name(&self) -> &str {
        "sessions_list"
    }

    fn description(&self) -> &str {
        "List all active conversation sessions with their channel, last activity time, and message count."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Max sessions to return (default: 50)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(50, |v| v as usize);

        let metadata = self.backend.list_sessions_with_metadata();

        if metadata.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No active sessions found.".into(),
                error: None,
            });
        }

        let capped: Vec<_> = metadata.into_iter().take(limit).collect();
        let mut output = format!("Found {} session(s):\n", capped.len());
        for meta in &capped {
            // Extract channel from key (convention: channel__identifier)
            let channel = meta.key.split("__").next().unwrap_or(&meta.key);
            let _ = writeln!(
                output,
                "- {}: channel={}, messages={}, last_activity={}",
                meta.key, channel, meta.message_count, meta.last_activity
            );
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ── SessionsHistoryTool ─────────────────────────────────────────────

/// Reads the message history of a specific session by ID.
pub struct SessionsHistoryTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
}

impl SessionsHistoryTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self { backend, security }
    }
}

#[async_trait]
impl Tool for SessionsHistoryTool {
    fn name(&self) -> &str {
        "sessions_history"
    }

    fn description(&self) -> &str {
        "Read the message history of a specific session by its session ID. Returns the last N messages."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The session ID to read history from (e.g. telegram__user123)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max messages to return, from most recent (default: 20)"
                }
            },
            "required": ["session_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "sessions_history")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        if let Err(result) = validate_session_id(session_id) {
            return Ok(result);
        }

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(20, |v| v as usize);

        let messages = self.backend.load(session_id);

        if messages.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!("No messages found for session '{session_id}'."),
                error: None,
            });
        }

        // Take the last `limit` messages
        let start = messages.len().saturating_sub(limit);
        let tail = &messages[start..];

        let mut output = format!(
            "Session '{}': showing {}/{} messages\n",
            session_id,
            tail.len(),
            messages.len()
        );
        for msg in tail {
            let _ = writeln!(output, "[{}] {}", msg.role, msg.content);
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ── SessionsSendTool ────────────────────────────────────────────────

/// Sends a message to a specific session, enabling inter-agent communication.
pub struct SessionsSendTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
}

impl SessionsSendTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self { backend, security }
    }
}

#[async_trait]
impl Tool for SessionsSendTool {
    fn name(&self) -> &str {
        "sessions_send"
    }

    fn description(&self) -> &str {
        "Send a message to a specific session by its session ID. The message is appended to the session's conversation history as a 'user' message, enabling inter-agent communication."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The target session ID (e.g. telegram__user123)"
                },
                "message": {
                    "type": "string",
                    "description": "The message content to send"
                }
            },
            "required": ["session_id", "message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "sessions_send")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        if let Err(result) = validate_session_id(session_id) {
            return Ok(result);
        }

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?;

        if message.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Message content must not be empty.".into()),
            });
        }

        let chat_msg = zeroclaw_api::provider::ChatMessage::user(message);

        match self.backend.append(session_id, &chat_msg) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Message sent to session '{session_id}'."),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to send message: {e}")),
            }),
        }
    }
}

// ── SessionResetTool ────────────────────────────────────────────────

/// Resets a session by clearing its message history. The session key
/// remains valid for new messages. Useful for cleaning up stale
/// conversations without deleting the session entry itself.
pub struct SessionResetTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
}

impl SessionResetTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self { backend, security }
    }
}

#[async_trait]
impl Tool for SessionResetTool {
    fn name(&self) -> &str {
        "sessions_reset"
    }

    fn description(&self) -> &str {
        "Reset a session by clearing all its messages. The session can still receive new messages after reset."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The session ID to reset (e.g. telegram__user123)"
                }
            },
            "required": ["session_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "sessions_reset")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        if let Err(result) = validate_session_id(session_id) {
            return Ok(result);
        }

        // TOCTOU: a message appended between load() and the last remove_last()
        // will also be removed. Acceptable until clear_messages lands (#5701).
        let messages = self.backend.load(session_id);
        if messages.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!("Session '{session_id}' is already empty."),
                error: None,
            });
        }

        // Remove messages one by one via remove_last until empty
        let msg_count = messages.len();
        for _ in 0..msg_count {
            if let Err(e) = self.backend.remove_last(session_id) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to reset session: {e}")),
                });
            }
        }

        Ok(ToolResult {
            success: true,
            output: format!("Session '{session_id}' reset ({msg_count} messages cleared)."),
            error: None,
        })
    }
}

// ── SessionDeleteTool ──────────────────────────────────────────────

/// Permanently deletes a session and all its messages. The session key
/// becomes invalid and must be recreated for new conversations.
pub struct SessionDeleteTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
}

impl SessionDeleteTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self { backend, security }
    }
}

#[async_trait]
impl Tool for SessionDeleteTool {
    fn name(&self) -> &str {
        "sessions_delete"
    }

    fn description(&self) -> &str {
        "Permanently delete a session and all its messages. This cannot be undone."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The session ID to delete (e.g. telegram__user123)"
                }
            },
            "required": ["session_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "sessions_delete")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        if let Err(result) = validate_session_id(session_id) {
            return Ok(result);
        }

        let existed = !self.backend.load(session_id).is_empty();

        match self.backend.delete_session(session_id) {
            Ok(true) => Ok(ToolResult {
                success: true,
                output: format!("Session '{session_id}' deleted."),
                error: None,
            }),
            Ok(false) if !existed => Ok(ToolResult {
                success: true,
                output: format!(
                    "Session '{session_id}' not found (may have already been deleted)."
                ),
                error: None,
            }),
            Ok(false) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Session '{session_id}' exists but could not be deleted \
                     — the storage backend may not support this operation."
                )),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to delete session: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_api::provider::ChatMessage;
    use zeroclaw_infra::session_store::SessionStore;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn test_backend() -> (TempDir, Arc<dyn SessionBackend>) {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        (tmp, Arc::new(store))
    }

    fn seeded_backend() -> (TempDir, Arc<dyn SessionBackend>) {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        store
            .append("telegram__alice", &ChatMessage::user("Hello from Alice"))
            .unwrap();
        store
            .append(
                "telegram__alice",
                &ChatMessage::assistant("Hi Alice, how can I help?"),
            )
            .unwrap();
        store
            .append("discord__bob", &ChatMessage::user("Hey from Bob"))
            .unwrap();
        (tmp, Arc::new(store))
    }

    // ── SessionsListTool tests ──────────────────────────────────────

    #[tokio::test]
    async fn list_empty_sessions() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsListTool::new(backend);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No active sessions"));
    }

    #[tokio::test]
    async fn list_sessions_shows_all() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsListTool::new(backend);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("2 session(s)"));
        assert!(result.output.contains("telegram__alice"));
        assert!(result.output.contains("discord__bob"));
    }

    #[tokio::test]
    async fn list_sessions_respects_limit() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsListTool::new(backend);
        let result = tool.execute(json!({"limit": 1})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("1 session(s)"));
    }

    #[tokio::test]
    async fn list_sessions_extracts_channel() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsListTool::new(backend);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.output.contains("channel=telegram"));
        assert!(result.output.contains("channel=discord"));
    }

    #[test]
    fn list_tool_name_and_schema() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsListTool::new(backend);
        assert_eq!(tool.name(), "sessions_list");
        assert!(tool.parameters_schema()["properties"]["limit"].is_object());
    }

    // ── SessionsHistoryTool tests ───────────────────────────────────

    #[tokio::test]
    async fn history_empty_session() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsHistoryTool::new(backend, test_security());
        let result = tool
            .execute(json!({"session_id": "nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No messages found"));
    }

    #[tokio::test]
    async fn history_returns_messages() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsHistoryTool::new(backend, test_security());
        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("showing 2/2 messages"));
        assert!(result.output.contains("[user] Hello from Alice"));
        assert!(result.output.contains("[assistant] Hi Alice"));
    }

    #[tokio::test]
    async fn history_respects_limit() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsHistoryTool::new(backend, test_security());
        let result = tool
            .execute(json!({"session_id": "telegram__alice", "limit": 1}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("showing 1/2 messages"));
        // Should show only the last message
        assert!(result.output.contains("[assistant]"));
        assert!(!result.output.contains("[user] Hello from Alice"));
    }

    #[tokio::test]
    async fn history_missing_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsHistoryTool::new(backend, test_security());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session_id"));
    }

    #[tokio::test]
    async fn history_rejects_empty_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsHistoryTool::new(backend, test_security());
        let result = tool.execute(json!({"session_id": "   "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid"));
    }

    #[test]
    fn history_tool_name_and_schema() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsHistoryTool::new(backend, test_security());
        assert_eq!(tool.name(), "sessions_history");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["session_id"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("session_id"))
        );
    }

    // ── SessionsSendTool tests ──────────────────────────────────────

    #[tokio::test]
    async fn send_appends_message() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::new(backend.clone(), test_security());
        let result = tool
            .execute(json!({
                "session_id": "telegram__alice",
                "message": "Hello from another agent"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Message sent"));

        // Verify message was appended
        let messages = backend.load("telegram__alice");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Hello from another agent");
    }

    #[tokio::test]
    async fn send_to_existing_session() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsSendTool::new(backend.clone(), test_security());
        let result = tool
            .execute(json!({
                "session_id": "telegram__alice",
                "message": "Inter-agent message"
            }))
            .await
            .unwrap();
        assert!(result.success);

        let messages = backend.load("telegram__alice");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].content, "Inter-agent message");
    }

    #[tokio::test]
    async fn send_rejects_empty_message() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::new(backend, test_security());
        let result = tool
            .execute(json!({
                "session_id": "telegram__alice",
                "message": "   "
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn send_rejects_empty_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::new(backend, test_security());
        let result = tool
            .execute(json!({
                "session_id": "",
                "message": "hello"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid"));
    }

    #[tokio::test]
    async fn send_rejects_non_alphanumeric_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::new(backend, test_security());
        let result = tool
            .execute(json!({
                "session_id": "///",
                "message": "hello"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid"));
    }

    #[tokio::test]
    async fn send_missing_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::new(backend, test_security());
        let result = tool.execute(json!({"message": "hi"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session_id"));
    }

    #[tokio::test]
    async fn send_missing_message() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::new(backend, test_security());
        let result = tool.execute(json!({"session_id": "telegram__alice"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("message"));
    }

    #[test]
    fn send_tool_name_and_schema() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::new(backend, test_security());
        assert_eq!(tool.name(), "sessions_send");
        let schema = tool.parameters_schema();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("session_id"))
        );
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("message"))
        );
    }

    // ── SessionResetTool tests ─────────────────────────────────────

    #[tokio::test]
    async fn reset_clears_messages() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionResetTool::new(backend.clone(), test_security());
        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("2 messages cleared"));

        // Verify messages are gone
        let messages = backend.load("telegram__alice");
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn reset_empty_session_is_noop() {
        let (_tmp, backend) = test_backend();
        let tool = SessionResetTool::new(backend, test_security());
        let result = tool
            .execute(json!({"session_id": "nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("already empty"));
    }

    #[tokio::test]
    async fn reset_does_not_affect_other_sessions() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionResetTool::new(backend.clone(), test_security());
        tool.execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        // Bob's session should be untouched
        let bob_msgs = backend.load("discord__bob");
        assert_eq!(bob_msgs.len(), 1);
    }

    #[tokio::test]
    async fn reset_rejects_empty_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionResetTool::new(backend, test_security());
        let result = tool.execute(json!({"session_id": ""})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid"));
    }

    #[test]
    fn reset_tool_name_and_schema() {
        let (_tmp, backend) = test_backend();
        let tool = SessionResetTool::new(backend, test_security());
        assert_eq!(tool.name(), "sessions_reset");
        let schema = tool.parameters_schema();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("session_id"))
        );
    }

    // ── SessionDeleteTool tests ────────────────────────────────────

    #[tokio::test]
    async fn delete_removes_session() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionDeleteTool::new(backend.clone(), test_security());
        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("deleted"));

        // Verify session is gone
        let messages = backend.load("telegram__alice");
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn delete_nonexistent_session_succeeds() {
        let (_tmp, backend) = test_backend();
        let tool = SessionDeleteTool::new(backend, test_security());
        let result = tool
            .execute(json!({"session_id": "nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn delete_does_not_affect_other_sessions() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionDeleteTool::new(backend.clone(), test_security());
        tool.execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        // Bob's session should be untouched
        let bob_msgs = backend.load("discord__bob");
        assert_eq!(bob_msgs.len(), 1);
    }

    #[tokio::test]
    async fn delete_rejects_empty_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionDeleteTool::new(backend, test_security());
        let result = tool.execute(json!({"session_id": "   "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid"));
    }

    #[test]
    fn delete_tool_name_and_schema() {
        let (_tmp, backend) = test_backend();
        let tool = SessionDeleteTool::new(backend, test_security());
        assert_eq!(tool.name(), "sessions_delete");
        let schema = tool.parameters_schema();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("session_id"))
        );
    }

    // ── NoOpDeleteBackend (test helper) ────────────────────────────

    /// Delegates everything except delete_session, which uses the trait
    /// default (returns Ok(false) without deleting anything).
    struct NoOpDeleteBackend(Arc<dyn SessionBackend>);

    impl SessionBackend for NoOpDeleteBackend {
        fn load(&self, key: &str) -> Vec<ChatMessage> {
            self.0.load(key)
        }
        fn append(&self, key: &str, msg: &ChatMessage) -> std::io::Result<()> {
            self.0.append(key, msg)
        }
        fn remove_last(&self, key: &str) -> std::io::Result<bool> {
            self.0.remove_last(key)
        }
        fn list_sessions(&self) -> Vec<String> {
            self.0.list_sessions()
        }
    }

    #[tokio::test]
    async fn delete_detects_noop_backend() {
        let (_tmp, inner) = seeded_backend();
        let backend: Arc<dyn SessionBackend> = Arc::new(NoOpDeleteBackend(inner));
        let tool = SessionDeleteTool::new(backend, test_security());
        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("could not be deleted"));
    }
}
