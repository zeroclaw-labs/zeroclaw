//! Session-to-session messaging tools for inter-agent communication.

use async_trait::async_trait;
use serde_json::json;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_infra::channel_conversation::ChannelConversationStore;
use zeroclaw_infra::session_backend::SessionBackend;

/// Validate that a session ID is non-empty and contains at least one
/// alphanumeric character (prevents blank keys after sanitization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionValidationError {
    Empty,
    NoAlphanumeric,
}

impl SessionValidationError {
    fn message(self) -> &'static str {
        match self {
            Self::Empty | Self::NoAlphanumeric => {
                "Invalid 'session_id': must be non-empty and contain at least one alphanumeric character."
            }
        }
    }

    fn into_tool_result(self) -> ToolResult {
        ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(self.message().into()),
        }
    }
}

fn validate_session_id(session_id: &str) -> Result<(), SessionValidationError> {
    let trimmed = session_id.trim();
    if trimmed.is_empty() {
        return Err(SessionValidationError::Empty);
    }
    if !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return Err(SessionValidationError::NoAlphanumeric);
    }
    Ok(())
}

fn resolve_existing_session_key(backend: &dyn SessionBackend, session_id: &str) -> Option<String> {
    let requested = session_id.trim();
    let sessions = backend.list_sessions();
    if sessions.iter().any(|key| key == requested) {
        return Some(requested.to_string());
    }
    if !requested.starts_with("gw_") {
        let gateway_key = format!("gw_{requested}");
        if sessions.iter().any(|key| key == &gateway_key) {
            return Some(gateway_key);
        }
    }
    None
}

/// Decide whether `session_key` owns a Channel conversation record (and so must
/// route reset/delete through the shared `ChannelConversationStore` lifecycle),
/// versus a gateway dashboard session that the gateway owns directly.
///
/// The rule is metadata-driven, not a `!gw_` prefix guess:
/// - A `gw_`-prefixed key is always a gateway dashboard session (never channel).
/// - Durable backends: a session row is channel-owned iff its persisted
///   `session_metadata.channel_id` is set. Conversation-id presence proves only
///   that a record exists; it is not Channel ownership metadata.
/// - Memory-only: channel-owned iff the shared store currently holds a live
///   record for this key. The caller passes the store's live key set so the
///   helper stays a pure predicate with no hidden state lookup.
///
/// This is the single ownership rule shared by the gateway API delete handler
/// and `SessionResetTool`/`SessionDeleteTool`; none of them infer ownership
/// from the absence of a `gw_` prefix.
pub fn is_channel_owned_session(
    backend: Option<&dyn SessionBackend>,
    memory_channel_keys: &[String],
    session_key: &str,
) -> bool {
    if session_key.starts_with("gw_") {
        return false;
    }
    // Durable backends classify ownership solely from reliable routing
    // metadata. A conversation id proves record existence, not Channel
    // ownership, so it must not be used as a fallback.
    if let Some(backend) = backend {
        return backend
            .get_session_metadata(session_key)
            .and_then(|metadata| metadata.channel_id)
            .is_some();
    }
    // Memory-only (no backend): a live Channel record in the shared store's key
    // set is the signal.
    memory_channel_keys.iter().any(|key| key == session_key)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOwnershipScope {
    agent_alias: String,
    channel_ids: BTreeSet<String>,
}

impl SessionOwnershipScope {
    pub fn for_agent(agent_alias: impl Into<String>) -> Self {
        Self {
            agent_alias: agent_alias.into(),
            channel_ids: BTreeSet::new(),
        }
    }

    pub fn with_channels<I, S>(agent_alias: impl Into<String>, channel_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            agent_alias: agent_alias.into(),
            channel_ids: channel_ids.into_iter().map(Into::into).collect(),
        }
    }

    fn authorize(&self, backend: &dyn SessionBackend, session_id: &str) -> Result<String, String> {
        let Some(session_key) = resolve_existing_session_key(backend, session_id) else {
            return Ok(session_id.trim().to_string());
        };

        let Some(metadata) = backend.get_session_metadata(&session_key) else {
            return Err(format!(
                "Session '{session_id}' exists but has no ownership metadata; refusing destructive session operation from agent '{}'.",
                self.agent_alias
            ));
        };

        if let Some(owner) = metadata.agent_alias.as_deref() {
            if owner == self.agent_alias {
                return Ok(session_key);
            }
            return Err(format!(
                "Session '{session_id}' is owned by agent '{owner}', not '{}'.",
                self.agent_alias
            ));
        }

        if let Some(channel_id) = metadata.channel_id.as_deref() {
            if self.channel_ids.contains(channel_id) {
                return Ok(session_key);
            }
            return Err(format!(
                "Session '{session_id}' belongs to channel '{channel_id}', which is not owned by agent '{}'.",
                self.agent_alias
            ));
        }

        Err(format!(
            "Session '{session_id}' has no agent or channel ownership metadata; refusing destructive session operation from agent '{}'.",
            self.agent_alias
        ))
    }
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
            output: output.into(),
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
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "session_id"})),
                    "sessions: missing session_id parameter"
                );
                anyhow::Error::msg("Missing 'session_id' parameter")
            })?;

        if let Err(error) = validate_session_id(session_id) {
            return Ok(error.into_tool_result());
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
                output: format!("No messages found for session '{session_id}'.").into(),
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
            output: output.into(),
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
                    "description": "The target session ID (e.g. telegram__user123). Gateway dashboard sessions may be addressed by their dashboard ID or by gw_<id>."
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
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "session_id"})),
                    "sessions: missing session_id parameter"
                );
                anyhow::Error::msg("Missing 'session_id' parameter")
            })?;

        if let Err(error) = validate_session_id(session_id) {
            return Ok(error.into_tool_result());
        }

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "message"})),
                    "sessions: missing message parameter"
                );
                anyhow::Error::msg("Missing 'message' parameter")
            })?;

        if message.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Message content must not be empty.".into()),
            });
        }

        let Some(target_session_key) =
            resolve_existing_session_key(self.backend.as_ref(), session_id)
        else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Session '{session_id}' not found. Use sessions_list or sessions_current to choose an existing session. Gateway dashboard sessions are stored as 'gw_<session_id>'."
                )),
            });
        };

        let chat_msg = zeroclaw_api::model_provider::ChatMessage::user(message);

        match self.backend.append(&target_session_key, &chat_msg) {
            Ok(()) => {
                let output = if target_session_key == session_id.trim() {
                    format!("Message sent to session '{target_session_key}'.")
                } else {
                    format!(
                        "Message sent to session '{target_session_key}' (requested '{session_id}')."
                    )
                };
                Ok(ToolResult {
                    success: true,
                    output: output.into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to send message: {e}")),
            }),
        }
    }
}

// ── SessionsCurrentTool ────────────────────────────────────────────

/// Returns the session key and metadata for the currently active session.
/// Reads the session key from the `TOOL_LOOP_SESSION_KEY` task-local,
/// which is scoped around gateway and channel agent turns.
pub struct SessionsCurrentTool {
    backend: Arc<dyn SessionBackend>,
}

impl SessionsCurrentTool {
    pub fn new(backend: Arc<dyn SessionBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for SessionsCurrentTool {
    fn name(&self) -> &str {
        "sessions_current"
    }

    fn description(&self) -> &str {
        "Return the session key and metadata for the session this agent is currently running in."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let session_key = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .try_with(Clone::clone)
            .ok()
            .flatten();

        let Some(key) = session_key else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(
                    "No active session context. This tool is only available during a gateway session.".into(),
                ),
            });
        };

        let mut output = format!("Current session: {key}\n");
        if let Some(meta) = self.backend.get_session_metadata(&key) {
            if let Some(name) = meta.name.filter(|name| !name.is_empty()) {
                let _ = writeln!(output, "Name: {name}");
            }
            if meta.message_count > 0 {
                let _ = writeln!(output, "Messages: {}", meta.message_count);
            }
        }

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

// ── SessionResetTool ────────────────────────────────────────────────

/// Resets a session by clearing its message history. The session key
/// remains valid for new messages. Useful for cleaning up stale
/// conversations without deleting the session entry itself.
pub struct SessionResetTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
    ownership_scope: Option<SessionOwnershipScope>,
    /// Shared Channel session lifecycle. When the target key is Channel-owned,
    /// reset cancels competing workers and deletes the current record. The next
    /// ordinary message creates the replacement record.
    channel_sessions: Option<Arc<ChannelConversationStore>>,
}

impl SessionResetTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            backend,
            security,
            ownership_scope: None,
            channel_sessions: None,
        }
    }

    pub fn for_agent(
        backend: Arc<dyn SessionBackend>,
        security: Arc<SecurityPolicy>,
        ownership_scope: SessionOwnershipScope,
    ) -> Self {
        Self {
            backend,
            security,
            ownership_scope: Some(ownership_scope),
            channel_sessions: None,
        }
    }

    /// Wire the shared Channel lifecycle for Channel-owned reset targets.
    /// Non-Channel targets keep the generic backend rotation path.
    pub fn with_channel_sessions(
        mut self,
        channel_sessions: Arc<ChannelConversationStore>,
    ) -> Self {
        self.channel_sessions = Some(channel_sessions);
        self
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
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "session_id"})),
                    "sessions: missing session_id parameter"
                );
                anyhow::Error::msg("Missing 'session_id' parameter")
            })?;

        if let Err(error) = validate_session_id(session_id) {
            return Ok(error.into_tool_result());
        }

        let target_session_key = match &self.ownership_scope {
            Some(scope) => match scope.authorize(self.backend.as_ref(), session_id) {
                Ok(key) => key,
                Err(error) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(error),
                    });
                }
            },
            None => resolve_existing_session_key(self.backend.as_ref(), session_id)
                .unwrap_or_else(|| session_id.trim().to_string()),
        };

        // Channel reset deletes the current record; only the next ordinary
        // message creates a replacement identity. Generic WS/RPC sessions keep
        // the backend rotation behavior required by their durable lifecycle.
        let memory_channel_keys = self
            .channel_sessions
            .as_ref()
            .filter(|store| store.contains_memory_record(&target_session_key))
            .map(|_| vec![target_session_key.clone()])
            .unwrap_or_default();
        let channel_backend = self
            .channel_sessions
            .as_ref()
            .and_then(|store| store.backend().map(Arc::as_ref));
        let is_channel_owned =
            is_channel_owned_session(channel_backend, &memory_channel_keys, &target_session_key);
        if is_channel_owned && let Some(ref channel_sessions) = self.channel_sessions {
            match channel_sessions.delete(&target_session_key).await {
                Ok(_) => Ok(ToolResult {
                    success: true,
                    output: format!("Session '{target_session_key}' reset.").into(),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to reset session: {e}")),
                }),
            }
        } else {
            match self
                .backend
                .clear_and_rotate_conversation(&target_session_key)
            {
                Ok(_) => Ok(ToolResult {
                    success: true,
                    output: format!("Session '{target_session_key}' reset.").into(),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to reset session: {e}")),
                }),
            }
        }
    }
}

// ── SessionDeleteTool ──────────────────────────────────────────────

/// Permanently deletes a session and all its messages. The session key
/// becomes invalid and must be recreated for new conversations.
pub struct SessionDeleteTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
    ownership_scope: Option<SessionOwnershipScope>,
    /// Shared Channel session lifecycle. When the target key is a
    /// Channel-owned session and this handle is present, delete goes through
    /// the shared lifecycle (`delete_session`, which cancels + waits competing
    /// workers). Otherwise delete uses the backend `delete_session`.
    channel_sessions: Option<Arc<ChannelConversationStore>>,
}

impl SessionDeleteTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            backend,
            security,
            ownership_scope: None,
            channel_sessions: None,
        }
    }

    pub fn for_agent(
        backend: Arc<dyn SessionBackend>,
        security: Arc<SecurityPolicy>,
        ownership_scope: SessionOwnershipScope,
    ) -> Self {
        Self {
            backend,
            security,
            ownership_scope: Some(ownership_scope),
            channel_sessions: None,
        }
    }

    /// Wire the shared Channel session lifecycle so a Channel-owned target key
    /// deletes through `delete_session` (cancel + wait competing workers).
    /// Non-Channel targets keep the backend-only delete.
    pub fn with_channel_sessions(
        mut self,
        channel_sessions: Arc<ChannelConversationStore>,
    ) -> Self {
        self.channel_sessions = Some(channel_sessions);
        self
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
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "session_id"})),
                    "sessions: missing session_id parameter"
                );
                anyhow::Error::msg("Missing 'session_id' parameter")
            })?;

        if let Err(error) = validate_session_id(session_id) {
            return Ok(error.into_tool_result());
        }

        let target_session_key = match &self.ownership_scope {
            Some(scope) => match scope.authorize(self.backend.as_ref(), session_id) {
                Ok(key) => key,
                Err(error) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(error),
                    });
                }
            },
            None => resolve_existing_session_key(self.backend.as_ref(), session_id)
                .unwrap_or_else(|| session_id.trim().to_string()),
        };

        // A Channel-owned key (a record with `session_metadata.channel_id`, or
        // a live memory-only record in the shared store) with the shared handle
        // wired in deletes through the lifecycle: cancel + wait competing
        // workers, then remove the record. Gateway dashboard (`gw_`) and
        // non-channel sessions use the backend delete. Ownership is metadata-
        // driven, not a `!gw_` prefix guess.
        let memory_channel_keys = self
            .channel_sessions
            .as_ref()
            .filter(|store| store.contains_memory_record(&target_session_key))
            .map(|_| vec![target_session_key.clone()])
            .unwrap_or_default();
        let channel_backend = self
            .channel_sessions
            .as_ref()
            .and_then(|store| store.backend().map(Arc::as_ref));
        let is_channel_owned =
            is_channel_owned_session(channel_backend, &memory_channel_keys, &target_session_key);
        if is_channel_owned && let Some(ref channel_sessions) = self.channel_sessions {
            match channel_sessions.delete_session(&target_session_key).await {
                Ok(true) => Ok(ToolResult {
                    success: true,
                    output: format!("Session '{target_session_key}' deleted.").into(),
                    error: None,
                }),
                Ok(false) => Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Session '{target_session_key}' not found (may have already been deleted)."
                    )
                    .into(),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to delete session: {e}")),
                }),
            }
        } else {
            let existed = !self.backend.load(&target_session_key).is_empty();

            match self.backend.delete_session(&target_session_key) {
                Ok(true) => Ok(ToolResult {
                    success: true,
                    output: format!("Session '{target_session_key}' deleted.").into(),
                    error: None,
                }),
                Ok(false) if !existed => Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Session '{target_session_key}' not found (may have already been deleted)."
                    )
                    .into(),
                    error: None,
                }),
                Ok(false) => Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Session '{target_session_key}' exists but could not be deleted \
                     — the storage backend may not support this operation."
                    )),
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to delete session: {e}")),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use zeroclaw_api::model_provider::ChatMessage;
    use zeroclaw_infra::session_backend::SessionMetadata;
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

    struct MetadataBackend {
        inner: Arc<dyn SessionBackend>,
        metadata: Mutex<HashMap<String, SessionMetadata>>,
    }

    impl MetadataBackend {
        fn new(inner: Arc<dyn SessionBackend>, metadata: Vec<SessionMetadata>) -> Self {
            Self {
                inner,
                metadata: Mutex::new(
                    metadata
                        .into_iter()
                        .map(|entry| (entry.key.clone(), entry))
                        .collect(),
                ),
            }
        }
    }

    impl SessionBackend for MetadataBackend {
        fn load(&self, key: &str) -> Vec<ChatMessage> {
            self.inner.load(key)
        }

        fn append(&self, key: &str, msg: &ChatMessage) -> std::io::Result<()> {
            self.inner.append(key, msg)
        }

        fn remove_last(&self, key: &str) -> std::io::Result<bool> {
            self.inner.remove_last(key)
        }

        fn list_sessions(&self) -> Vec<String> {
            self.inner.list_sessions()
        }

        fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
            self.inner.clear_messages(session_key)
        }

        fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
            let deleted = self.inner.delete_session(session_key)?;
            if deleted {
                self.metadata.lock().unwrap().remove(session_key);
            }
            Ok(deleted)
        }

        fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
            self.metadata
                .lock()
                .unwrap()
                .get(session_key)
                .cloned()
                .or_else(|| self.inner.get_session_metadata(session_key))
        }

        fn session_exists(&self, session_key: &str) -> bool {
            self.metadata.lock().unwrap().contains_key(session_key)
                || self.inner.session_exists(session_key)
        }

        fn resolve_or_create_conversation_id(&self, session_key: &str) -> std::io::Result<String> {
            self.inner.resolve_or_create_conversation_id(session_key)
        }

        fn clear_and_rotate_conversation(&self, session_key: &str) -> std::io::Result<String> {
            self.inner.clear_and_rotate_conversation(session_key)
        }

        fn append_if_conversation_matches(
            &self,
            session_key: &str,
            expected_conversation_id: &str,
            message: &ChatMessage,
        ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite> {
            self.inner.append_if_conversation_matches(
                session_key,
                expected_conversation_id,
                message,
            )
        }

        fn remove_last_if_conversation_matches(
            &self,
            session_key: &str,
            expected_conversation_id: &str,
        ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite> {
            self.inner
                .remove_last_if_conversation_matches(session_key, expected_conversation_id)
        }

        fn update_last_if_conversation_matches(
            &self,
            session_key: &str,
            expected_conversation_id: &str,
            message: &ChatMessage,
        ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite> {
            self.inner.update_last_if_conversation_matches(
                session_key,
                expected_conversation_id,
                message,
            )
        }
    }

    fn session_metadata(
        key: &str,
        agent_alias: Option<&str>,
        channel_id: Option<&str>,
        message_count: usize,
    ) -> SessionMetadata {
        SessionMetadata {
            key: key.to_string(),
            name: None,
            created_at: Utc::now(),
            last_activity: Utc::now(),
            message_count,
            agent_alias: agent_alias.map(str::to_string),
            channel_id: channel_id.map(str::to_string),
            room_id: None,
            sender_id: None,
            conversation_id: None,
        }
    }

    fn seeded_metadata_backend(
        metadata: Vec<SessionMetadata>,
    ) -> (TempDir, Arc<dyn SessionBackend>) {
        let (tmp, inner) = seeded_backend();
        (tmp, Arc::new(MetadataBackend::new(inner, metadata)))
    }

    // ── Session ID validation tests ─────────────────────────────────

    #[test]
    fn validate_session_id_rejects_empty() {
        assert_eq!(validate_session_id(""), Err(SessionValidationError::Empty));
    }

    #[test]
    fn validate_session_id_rejects_whitespace_only() {
        assert_eq!(
            validate_session_id("   "),
            Err(SessionValidationError::Empty)
        );
    }

    #[test]
    fn validate_session_id_rejects_non_alphanumeric() {
        assert_eq!(
            validate_session_id("///"),
            Err(SessionValidationError::NoAlphanumeric)
        );
    }

    #[test]
    fn validate_session_id_accepts_valid_id() {
        assert_eq!(validate_session_id("test_session_id"), Ok(()));
    }

    #[test]
    fn validation_error_message_starts_with_invalid() {
        assert!(
            SessionValidationError::Empty
                .message()
                .starts_with("Invalid")
        );
        assert!(
            SessionValidationError::NoAlphanumeric
                .message()
                .starts_with("Invalid")
        );
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
        assert!(result.error.is_some());
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
    async fn send_appends_message_to_existing_session() {
        let (_tmp, backend) = test_backend();
        backend
            .append("telegram__alice", &ChatMessage::user("Hello from Alice"))
            .unwrap();
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
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content, "Hello from another agent");
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
    async fn send_to_gateway_session_accepts_dashboard_session_id() {
        let (_tmp, backend) = test_backend();
        backend
            .append(
                "gw_operator-1",
                &ChatMessage::assistant("Existing dashboard message"),
            )
            .unwrap();
        let tool = SessionsSendTool::new(backend.clone(), test_security());

        let result = tool
            .execute(json!({
                "session_id": "operator-1",
                "message": "Wake up"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("gw_operator-1"));

        let gateway_messages = backend.load("gw_operator-1");
        assert_eq!(gateway_messages.len(), 2);
        assert_eq!(gateway_messages[1].role, "user");
        assert_eq!(gateway_messages[1].content, "Wake up");
        assert!(backend.load("operator-1").is_empty());
    }

    #[tokio::test]
    async fn send_rejects_unknown_session() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::new(backend.clone(), test_security());

        let result = tool
            .execute(json!({
                "session_id": "operator-1",
                "message": "Wake up"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("not found")
        );
        assert!(backend.load("operator-1").is_empty());
        assert!(backend.load("gw_operator-1").is_empty());
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
        assert!(result.error.is_some());
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
        assert!(result.error.is_some());
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

    // ── SessionsCurrentTool tests ──────────────────────────────────

    #[tokio::test]
    async fn sessions_current_returns_key_when_scoped() {
        let (tmp, backend) = test_backend();
        let _ = tmp;
        backend
            .append("gw_test-123", &ChatMessage::user("hello"))
            .unwrap();

        let tool = SessionsCurrentTool::new(backend);
        let result = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("gw_test-123".into()), tool.execute(json!({})))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("gw_test-123"));
        assert!(result.output.contains("Messages: 1"));
    }

    #[tokio::test]
    async fn sessions_current_fails_without_scope() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsCurrentTool::new(backend);

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("No active session context"));
    }

    #[tokio::test]
    async fn sessions_current_includes_name() {
        let tmp = TempDir::new().unwrap();
        let sqlite = zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(tmp.path()).unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(sqlite);
        backend
            .append("gw_named", &ChatMessage::user("hi"))
            .unwrap();
        backend.set_session_name("gw_named", "My Chat").unwrap();

        let tool = SessionsCurrentTool::new(backend);
        let result = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("gw_named".into()), tool.execute(json!({})))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("My Chat"));
    }

    #[tokio::test]
    async fn sessions_current_unknown_key_still_succeeds() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsCurrentTool::new(backend);

        let result = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("gw_unknown".into()), tool.execute(json!({})))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("gw_unknown"));
        assert!(!result.output.contains("Messages:"));
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
        assert!(result.output.contains("reset"));
        assert!(backend.load("telegram__alice").is_empty());

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
        assert!(result.output.contains("reset"));
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
    async fn reset_scoped_allows_own_agent_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            Some("rowan"),
            None,
            2,
        )]);
        let tool = SessionResetTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("reset"));
        assert!(backend.load("telegram__alice").is_empty());
    }

    #[tokio::test]
    async fn reset_scoped_denies_other_agent_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            Some("sable"),
            None,
            2,
        )]);
        let tool = SessionResetTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("owned by agent 'sable'"));
        assert_eq!(backend.load("telegram__alice").len(), 2);
    }

    #[tokio::test]
    async fn reset_scoped_allows_owned_channel_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            None,
            Some("telegram.default"),
            2,
        )]);
        let tool = SessionResetTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::with_channels("rowan", ["telegram.default"]),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(backend.load("telegram__alice").is_empty());
    }

    #[tokio::test]
    async fn reset_scoped_denies_unowned_channel_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            None,
            Some("telegram.default"),
            2,
        )]);
        let tool = SessionResetTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::with_channels("rowan", ["discord.default"]),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not owned by agent 'rowan'"));
        assert_eq!(backend.load("telegram__alice").len(), 2);
    }

    #[tokio::test]
    async fn reset_scoped_denies_legacy_unattributed_session() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionResetTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap()
                .contains("no agent or channel ownership metadata")
        );
        assert_eq!(backend.load("telegram__alice").len(), 2);
    }

    #[tokio::test]
    async fn reset_rejects_empty_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionResetTool::new(backend, test_security());
        let result = tool.execute(json!({"session_id": ""})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
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
    async fn delete_scoped_allows_own_agent_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            Some("rowan"),
            None,
            2,
        )]);
        let tool = SessionDeleteTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("deleted"));
        assert!(backend.load("telegram__alice").is_empty());
    }

    #[tokio::test]
    async fn delete_scoped_denies_other_agent_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            Some("sable"),
            None,
            2,
        )]);
        let tool = SessionDeleteTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("owned by agent 'sable'"));
        assert_eq!(backend.load("telegram__alice").len(), 2);
    }

    #[tokio::test]
    async fn delete_scoped_allows_owned_channel_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            None,
            Some("telegram.default"),
            2,
        )]);
        let tool = SessionDeleteTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::with_channels("rowan", ["telegram.default"]),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(backend.load("telegram__alice").is_empty());
    }

    #[tokio::test]
    async fn delete_scoped_denies_unowned_channel_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            None,
            Some("telegram.default"),
            2,
        )]);
        let tool = SessionDeleteTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::with_channels("rowan", ["discord.default"]),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not owned by agent 'rowan'"));
        assert_eq!(backend.load("telegram__alice").len(), 2);
    }

    #[tokio::test]
    async fn delete_scoped_denies_legacy_unattributed_session() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionDeleteTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap()
                .contains("no agent or channel ownership metadata")
        );
        assert_eq!(backend.load("telegram__alice").len(), 2);
    }

    #[tokio::test]
    async fn delete_rejects_empty_session_id() {
        let (_tmp, backend) = test_backend();
        let tool = SessionDeleteTool::new(backend, test_security());
        let result = tool.execute(json!({"session_id": "   "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
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
    /// Coupled to SessionBackend's default — if that default changes,
    /// this wrapper's behavior changes too.
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
        fn resolve_or_create_conversation_id(&self, key: &str) -> std::io::Result<String> {
            self.0.resolve_or_create_conversation_id(key)
        }
        fn clear_and_rotate_conversation(&self, key: &str) -> std::io::Result<String> {
            self.0.clear_and_rotate_conversation(key)
        }
        fn append_if_conversation_matches(
            &self,
            key: &str,
            expected_conversation_id: &str,
            message: &ChatMessage,
        ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite> {
            self.0
                .append_if_conversation_matches(key, expected_conversation_id, message)
        }
        fn remove_last_if_conversation_matches(
            &self,
            key: &str,
            expected_conversation_id: &str,
        ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite> {
            self.0
                .remove_last_if_conversation_matches(key, expected_conversation_id)
        }
        fn update_last_if_conversation_matches(
            &self,
            key: &str,
            expected_conversation_id: &str,
            message: &ChatMessage,
        ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite> {
            self.0
                .update_last_if_conversation_matches(key, expected_conversation_id, message)
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

    // ── shared Channel lifecycle wiring (.with_channel_sessions) ──────

    fn durable_channel_backend(key: &str) -> (TempDir, Arc<dyn SessionBackend>) {
        use zeroclaw_infra::session_backend::SessionContext;
        let tmp = TempDir::new().unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(tmp.path()).unwrap(),
        );
        let _ = backend.open_conversation(key).unwrap();
        backend
            .set_session_context(
                key,
                SessionContext {
                    channel_id: Some("telegram.main"),
                    room_id: None,
                    sender_id: Some("alice"),
                },
            )
            .unwrap();
        (tmp, backend)
    }

    /// `sessions_reset` deletes a Channel-owned record. A later ordinary open
    /// creates the replacement identity.
    #[tokio::test]
    async fn sessions_reset_channel_key_deletes_then_next_open_creates_id() {
        use zeroclaw_infra::channel_conversation::ChannelConversationStore;
        let key = "telegram__alice";
        let (_tmp, backend) = durable_channel_backend(key);
        let channel_sessions = Arc::new(ChannelConversationStore::new(Some(Arc::clone(&backend))));
        let id_before = channel_sessions.resolve_conversation_id(key).unwrap();
        channel_sessions
            .append_history_if_current(key, &id_before, ChatMessage::user("hello"), 50)
            .unwrap();
        assert_eq!(backend.load(key).len(), 1);

        let tool = SessionResetTool::new(backend.clone(), test_security())
            .with_channel_sessions(Arc::clone(&channel_sessions));
        let result = tool.execute(json!({"session_id": key})).await.unwrap();
        assert!(result.success, "reset must succeed");

        assert!(!backend.session_exists(key), "reset must delete the record");
        assert!(
            channel_sessions.existing_record(key).await.is_none(),
            "reset must not retain an empty record"
        );
        let id_after = channel_sessions.open(key).await.unwrap().conversation_id;
        assert_ne!(id_before, id_after, "the next open must create a new id");
    }

    /// `sessions_delete` on a Channel-owned key with the shared handle wired
    /// removes the record; a subsequent stale append gets Deleted.
    #[tokio::test]
    async fn sessions_delete_channel_key_removes_record() {
        use zeroclaw_infra::channel_conversation::ChannelConversationStore;
        use zeroclaw_infra::session_backend::ConditionalSessionWrite;
        let key = "telegram__alice";
        let (_tmp, backend) = durable_channel_backend(key);
        let channel_sessions = Arc::new(ChannelConversationStore::new(Some(Arc::clone(&backend))));
        let id = channel_sessions.resolve_conversation_id(key).unwrap();
        channel_sessions
            .append_history_if_current(key, &id, ChatMessage::user("hello"), 50)
            .unwrap();

        let tool = SessionDeleteTool::new(backend.clone(), test_security())
            .with_channel_sessions(Arc::clone(&channel_sessions));
        let result = tool.execute(json!({"session_id": key})).await.unwrap();
        assert!(result.success, "delete must succeed");

        // The record is gone; a stale append is Deleted (no recreation).
        assert_eq!(
            backend
                .append_if_conversation_matches(key, &id, &ChatMessage::assistant("gone"))
                .unwrap(),
            ConditionalSessionWrite::Deleted
        );
    }

    /// With an active Channel turn present, reset cancels it and deletes the
    /// record, so a stale conditional append gets `Deleted`.
    #[tokio::test]
    async fn sessions_reset_with_active_turn_cancels_and_makes_append_deleted() {
        use zeroclaw_infra::channel_conversation::ChannelConversationStore;
        use zeroclaw_infra::session_backend::ConditionalSessionWrite;
        let key = "telegram__alice";
        let (_tmp, backend) = durable_channel_backend(key);
        let channel_sessions = Arc::new(ChannelConversationStore::new(Some(Arc::clone(&backend))));
        let id = channel_sessions.resolve_conversation_id(key).unwrap();
        channel_sessions
            .append_history_if_current(key, &id, ChatMessage::user("hello"), 50)
            .unwrap();

        let token = tokio_util::sync::CancellationToken::new();
        channel_sessions
            .register_in_flight(key, token.clone())
            .await;

        let tool = SessionResetTool::new(backend.clone(), test_security())
            .with_channel_sessions(Arc::clone(&channel_sessions));
        let result = tool.execute(json!({"session_id": key})).await.unwrap();
        assert!(
            result.success,
            "reset must succeed even with an active lease"
        );
        assert!(
            token.is_cancelled(),
            "reset must cancel the active Channel lease"
        );

        assert_eq!(
            channel_sessions
                .append_history_if_current(key, &id, ChatMessage::assistant("stale"), 50)
                .unwrap(),
            ConditionalSessionWrite::Deleted
        );
    }

    #[tokio::test]
    async fn sessions_delete_with_active_lease_cancels_waits_and_makes_append_deleted() {
        use zeroclaw_infra::channel_conversation::ChannelConversationStore;
        use zeroclaw_infra::session_backend::ConditionalSessionWrite;
        let key = "telegram__alice";
        let (_tmp, backend) = durable_channel_backend(key);
        let channel_sessions = Arc::new(ChannelConversationStore::new(Some(Arc::clone(&backend))));
        let id = channel_sessions.resolve_conversation_id(key).unwrap();
        channel_sessions
            .append_history_if_current(key, &id, ChatMessage::user("hello"), 50)
            .unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        channel_sessions
            .register_in_flight(key, token.clone())
            .await;

        let tool = SessionDeleteTool::new(backend.clone(), test_security())
            .with_channel_sessions(Arc::clone(&channel_sessions));
        let delete =
            zeroclaw_spawn::spawn!(async move { tool.execute(json!({"session_id": key})).await });
        token.cancelled().await;
        assert!(token.is_cancelled());
        let result = delete.await.unwrap().unwrap();
        assert!(
            result.success,
            "delete must succeed after active turn drains"
        );
        assert!(!backend.session_exists(key));
        assert_eq!(
            channel_sessions
                .append_history_if_current(key, &id, ChatMessage::assistant("gone"), 50)
                .unwrap(),
            ConditionalSessionWrite::Deleted
        );
    }

    // ── ownership rule: metadata-driven, not a `!gw_` prefix guess ────

    /// `is_channel_owned_session` is metadata-driven, not a `!gw_` prefix guess:
    /// a `gw_` key is never channel-owned; a durable row is channel-owned iff
    /// its `session_metadata.channel_id` is set; a non-channel key with no
    /// channel metadata is NOT channel-owned even though it lacks the `gw_`
    /// prefix (so it must NOT route through the shared Channel lifecycle).
    #[test]
    fn channel_delete_requires_channel_metadata_not_non_gw_guess() {
        use zeroclaw_infra::channel_conversation::ChannelConversationStore;
        let (_tmp, inner) = seeded_backend();
        // A non-`gw_` session with NO channel metadata row: a naive `!gw_`
        // guess would route this through the Channel lifecycle, but the
        // metadata-driven rule must NOT (it has no channel owner).
        let metadata = vec![session_metadata("legacy_internal_key", None, None, 2)];
        let backend: Arc<dyn SessionBackend> =
            Arc::new(MetadataBackend::new(Arc::clone(&inner), metadata));
        let store = ChannelConversationStore::new(Some(Arc::clone(&backend)));

        // `gw_` key: never channel-owned.
        assert!(!is_channel_owned_session(
            Some(backend.as_ref()),
            &[],
            "gw_dashboard",
        ));

        // Channel-owned row: channel_id is set.
        let channel_key = "telegram__alice";
        let channel_meta = vec![session_metadata(
            channel_key,
            None,
            Some("telegram.main"),
            3,
        )];
        let backend_ch: Arc<dyn SessionBackend> =
            Arc::new(MetadataBackend::new(Arc::clone(&inner), channel_meta));
        assert!(is_channel_owned_session(
            Some(backend_ch.as_ref()),
            &[],
            channel_key,
        ));

        // Non-`gw_` key with NO channel metadata: NOT channel-owned even
        // though it lacks the `gw_` prefix. This is the guard against the
        // `!gw_` guess.
        assert!(!is_channel_owned_session(
            Some(backend.as_ref()),
            &[],
            "legacy_internal_key",
        ));

        // Memory-only (no backend): a live Channel record in the store's key
        // set is the signal. `legacy_internal_key` is not in the set.
        let _ = store;
        let memory_keys = vec!["memory_key".to_string()];
        assert!(!is_channel_owned_session(
            None,
            &memory_keys,
            "anything_here"
        ));
    }

    /// When Channel reset cannot delete durable storage, the current record is
    /// kept intact so a retry can re-attempt the lifecycle operation.
    #[tokio::test]
    async fn channel_reset_storage_failure_keeps_memory_record() {
        use zeroclaw_infra::channel_conversation::ChannelConversationStore;

        // A backend whose record deletion always fails.
        struct FailingDeleteBackend {
            inner: Arc<dyn SessionBackend>,
        }
        impl SessionBackend for FailingDeleteBackend {
            fn load(&self, k: &str) -> Vec<ChatMessage> {
                self.inner.load(k)
            }
            fn append(&self, k: &str, m: &ChatMessage) -> std::io::Result<()> {
                self.inner.append(k, m)
            }
            fn remove_last(&self, k: &str) -> std::io::Result<bool> {
                self.inner.remove_last(k)
            }
            fn list_sessions(&self) -> Vec<String> {
                self.inner.list_sessions()
            }
            fn delete_session(&self, _k: &str) -> std::io::Result<bool> {
                Err(std::io::Error::other("injected delete failure"))
            }
            fn current_conversation_id(&self, k: &str) -> std::io::Result<Option<String>> {
                self.inner.current_conversation_id(k)
            }
            fn open_conversation(
                &self,
                k: &str,
            ) -> std::io::Result<zeroclaw_infra::session_backend::ChannelConversationRecord>
            {
                self.inner.open_conversation(k)
            }
            fn mutate_conversation_if_current(
                &self,
                k: &str,
                id: &str,
                m: zeroclaw_infra::SessionMutation<'_>,
            ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite>
            {
                self.inner.mutate_conversation_if_current(k, id, m)
            }
            fn resolve_or_create_conversation_id(&self, k: &str) -> std::io::Result<String> {
                self.inner.resolve_or_create_conversation_id(k)
            }
            fn append_if_conversation_matches(
                &self,
                k: &str,
                id: &str,
                m: &ChatMessage,
            ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite>
            {
                self.inner.append_if_conversation_matches(k, id, m)
            }
            fn remove_last_if_conversation_matches(
                &self,
                k: &str,
                id: &str,
            ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite>
            {
                self.inner.remove_last_if_conversation_matches(k, id)
            }
            fn update_last_if_conversation_matches(
                &self,
                k: &str,
                id: &str,
                m: &ChatMessage,
            ) -> std::io::Result<zeroclaw_infra::session_backend::ConditionalSessionWrite>
            {
                self.inner.update_last_if_conversation_matches(k, id, m)
            }
            fn session_exists(&self, k: &str) -> bool {
                self.inner.session_exists(k)
            }
            fn clear_messages(&self, k: &str) -> std::io::Result<usize> {
                self.inner.clear_messages(k)
            }
        }

        let (_tmp, inner) = test_backend();
        let backend: Arc<dyn SessionBackend> = Arc::new(FailingDeleteBackend {
            inner: Arc::clone(&inner),
        });
        let channel_sessions = Arc::new(ChannelConversationStore::new(Some(Arc::clone(&backend))));
        let key = "telegram__alice";
        let id = channel_sessions.resolve_conversation_id(key).unwrap();
        channel_sessions
            .append_history_if_current(key, &id, ChatMessage::user("hello"), 50)
            .unwrap();
        // The record exists with one message before the failed reset.
        assert_eq!(
            channel_sessions
                .existing_record(key)
                .await
                .map(|r| r.history.len())
                .unwrap_or(0),
            1,
            "record must be seeded with one message before reset"
        );

        let tool = SessionResetTool::new(backend.clone(), test_security())
            .with_channel_sessions(Arc::clone(&channel_sessions));
        let result = tool.execute(json!({"session_id": key})).await.unwrap();
        assert!(
            !result.success,
            "reset must report failure when the storage op fails"
        );

        // The durable record survived the failed reset unchanged.
        assert_eq!(
            backend.current_conversation_id(key).unwrap().as_deref(),
            Some(id.as_str()),
            "failed reset must keep the current conversation id"
        );
        assert_eq!(
            channel_sessions
                .existing_record(key)
                .await
                .map(|r| r.history.len())
                .unwrap_or(0),
            1,
            "failed reset must keep the in-memory record (history intact)"
        );
    }
}
