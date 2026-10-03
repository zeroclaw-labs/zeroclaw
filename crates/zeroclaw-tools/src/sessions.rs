//! Session-to-session messaging tools for inter-agent communication.

use async_trait::async_trait;
use serde_json::json;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::sync::{Arc, OnceLock};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_infra::acp_session_store::{
    AcpSessionAccess, AcpSessionData, AcpSessionStore, AcpSessionSummary,
};
use zeroclaw_infra::session_backend::{
    ScopedSessionAccess, SessionBackend, SessionOwnershipDenial, check_session_ownership,
};

#[cfg(test)]
use zeroclaw_infra::session_backend::SessionMetadata;

static SESSIONS_LIST_DESCRIPTION: OnceLock<String> = OnceLock::new();
static SESSIONS_HISTORY_DESCRIPTION: OnceLock<String> = OnceLock::new();
static SESSIONS_SEND_DESCRIPTION: OnceLock<String> = OnceLock::new();

/// Agent-scoped access to the durable ACP session store.
///
/// The handle is attached by ACP server and RPC ACP Agent construction. Each tool
/// execution still verifies that the task-local session key names a live ACP
/// session owned by `agent_alias`; construction for an alias alone is not an
/// ACP invocation grant.
/// Listing exposes live sessions only. History may read a killed target's
/// retained transcript when it belongs to the same agent; this does not revive it.
#[derive(Clone)]
pub struct AcpSessionReadView {
    store: Arc<AcpSessionStore>,
    agent_alias: Arc<str>,
}

enum AcpTargetAccess {
    Inactive,
    Owned,
    Foreign,
    Missing,
}

impl AcpSessionReadView {
    pub fn new(store: Arc<AcpSessionStore>, agent_alias: impl Into<String>) -> Self {
        Self {
            store,
            agent_alias: Arc::from(agent_alias.into()),
        }
    }

    fn is_active(&self) -> anyhow::Result<bool> {
        let Some(current_key) = current_session_key() else {
            return Ok(false);
        };
        self.store
            .is_live_session_for_agent(&current_key, &self.agent_alias)
    }

    fn active_summaries(&self) -> anyhow::Result<Option<Vec<AcpSessionSummary>>> {
        let Some(current_key) = current_session_key() else {
            return Ok(None);
        };
        let summaries = self.store.list_live_sessions_by_agent(&self.agent_alias)?;
        if summaries
            .iter()
            .any(|summary| summary.session_uuid == current_key)
        {
            Ok(Some(summaries))
        } else {
            Ok(None)
        }
    }

    fn classify_target(&self, session_id: &str) -> anyhow::Result<AcpTargetAccess> {
        if !self.is_active()? {
            return Ok(AcpTargetAccess::Inactive);
        }
        Ok(
            match self
                .store
                .classify_session_for_agent(session_id, &self.agent_alias)?
            {
                AcpSessionAccess::Owned => AcpTargetAccess::Owned,
                AcpSessionAccess::Foreign => AcpTargetAccess::Foreign,
                AcpSessionAccess::Missing => AcpTargetAccess::Missing,
            },
        )
    }

    fn load_owned(&self, session_id: &str) -> anyhow::Result<Option<AcpSessionData>> {
        self.store
            .load_session_for_agent(session_id, &self.agent_alias)
    }

    fn all_session_ids(&self) -> anyhow::Result<std::collections::BTreeSet<String>> {
        Ok(self.store.list_session_ids()?.into_iter().collect())
    }
}

fn current_session_key() -> Option<String> {
    zeroclaw_api::TOOL_LOOP_SESSION_KEY
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_tool_string_with_args(key, args)
}

fn session_history_header(session_id: &str, shown: usize, total: usize) -> String {
    let shown = shown.to_string();
    let total = total.to_string();
    tool_msg_with_args(
        "tool-sessions-history-header",
        &[
            ("session_id", session_id),
            ("shown", &shown),
            ("total", &total),
        ],
    )
}

fn acp_send_unsupported() -> String {
    tool_msg_with_args(
        "tool-sessions-send-error-acp-unsupported",
        &[
            ("tool", "sessions_send"),
            ("channel", "ACP"),
            ("product", "Code"),
        ],
    )
}

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

/// How a scope learns which channels its agent currently owns.
///
/// The channel arm of the ownership predicate is a live authorization fact:
/// an operator can reassign `discord.ops` from A to B while A stays enabled
/// and keeps its tool names. A set copied at construction keeps admitting A
/// to the reassigned channel's sessions, so the revocation silently fails
/// for whoever still holds a retained agent. `Live` resolves per check.
#[derive(Clone)]
enum OwnedChannels {
    Fixed(BTreeSet<String>),
    Live(Arc<zeroclaw_infra::session_backend::ChannelAuthority>),
}

impl OwnedChannels {
    fn authority(&self) -> Arc<zeroclaw_infra::session_backend::ChannelAuthority> {
        match self {
            Self::Fixed(channels) => {
                let channels = channels.clone();
                Arc::new(move |effect| effect(&channels))
            }
            Self::Live(authority) => Arc::clone(authority),
        }
    }

    fn resolve(&self) -> BTreeSet<String> {
        match self {
            Self::Fixed(ids) => ids.clone(),
            Self::Live(resolve) => {
                let mut channels = BTreeSet::new();
                resolve(&mut |current| channels = current.clone());
                channels
            }
        }
    }
}

impl std::fmt::Debug for OwnedChannels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fixed(ids) => f.debug_tuple("Fixed").field(ids).finish(),
            Self::Live(_) => f.write_str("Live(<resolver>)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionOwnershipScope {
    agent_alias: String,
    channel_ids: OwnedChannels,
}

impl SessionOwnershipScope {
    pub fn for_agent(agent_alias: impl Into<String>) -> Self {
        Self {
            agent_alias: agent_alias.into(),
            channel_ids: OwnedChannels::Fixed(BTreeSet::new()),
        }
    }

    /// Channel ownership fixed at construction.
    ///
    /// Correct only where the scope cannot outlive the policy it was built
    /// from. Retained agents must use [`Self::with_live_channels`].
    pub fn with_channels<I, S>(agent_alias: impl Into<String>, channel_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            agent_alias: agent_alias.into(),
            channel_ids: OwnedChannels::Fixed(channel_ids.into_iter().map(Into::into).collect()),
        }
    }

    /// Channel ownership resolved from canonical config on every check.
    pub fn with_live_channels(
        agent_alias: impl Into<String>,
        resolve: Arc<zeroclaw_infra::session_backend::ChannelAuthority>,
    ) -> Self {
        Self {
            agent_alias: agent_alias.into(),
            channel_ids: OwnedChannels::Live(resolve),
        }
    }

    fn authorize(&self, backend: &dyn SessionBackend, session_id: &str) -> Result<String, String> {
        let Some(session_key) = resolve_existing_session_key(backend, session_id) else {
            return Ok(session_id.trim().to_string());
        };

        let Some(metadata) = backend.get_session_metadata(&session_key) else {
            return Err(format!(
                "Session '{session_id}' exists but has no ownership metadata; refusing session operation from agent '{}'.",
                self.agent_alias
            ));
        };

        check_session_ownership(
            metadata.agent_alias.as_deref(),
            metadata.channel_id.as_deref(),
            &self.agent_alias,
            &self.channel_ids.resolve(),
        )
        .map(|()| session_key)
        .map_err(|denial| self.denial_message(session_id, &denial))
    }

    fn denial_message(&self, session_id: &str, denial: &SessionOwnershipDenial) -> String {
        match denial {
            SessionOwnershipDenial::ForeignAgent(owner) => format!(
                "Session '{session_id}' is owned by agent '{owner}', not '{}'.",
                self.agent_alias
            ),
            SessionOwnershipDenial::ForeignChannel(channel_id) => format!(
                "Session '{session_id}' belongs to channel '{channel_id}', which is not owned by agent '{}'.",
                self.agent_alias
            ),
            SessionOwnershipDenial::Unattributed => format!(
                "Session '{session_id}' has no agent or channel ownership metadata; refusing session operation from agent '{}'.",
                self.agent_alias
            ),
            SessionOwnershipDenial::AtomicCheckUnavailable => format!(
                "Session backend cannot atomically verify ownership for '{session_id}'; refusing session operation from agent '{}'.",
                self.agent_alias
            ),
        }
    }

    /// Non-erroring form of the same three-tier ownership predicate, for
    /// filtering listings: agent match, else owned-channel match, else not
    /// owned (unattributed sessions are not owned by anyone).
    #[cfg(test)]
    fn owns_metadata(&self, metadata: &SessionMetadata) -> bool {
        check_session_ownership(
            metadata.agent_alias.as_deref(),
            metadata.channel_id.as_deref(),
            &self.agent_alias,
            &self.channel_ids.resolve(),
        )
        .is_ok()
    }
}

// ── SessionsListTool ────────────────────────────────────────────────

/// Lists active sessions with their channel, last activity time, and message count.
pub struct SessionsListTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
    ownership_scope: Option<SessionOwnershipScope>,
    acp_sessions: Option<AcpSessionReadView>,
}

impl SessionsListTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            backend,
            security,
            ownership_scope: None,
            acp_sessions: None,
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
            acp_sessions: None,
        }
    }

    /// Attach an ACP session read view without widening the ownership scope.
    #[must_use]
    pub fn with_optional_acp_sessions(mut self, acp_sessions: Option<AcpSessionReadView>) -> Self {
        self.acp_sessions = acp_sessions;
        self
    }
}

#[async_trait]
impl Tool for SessionsListTool {
    fn name(&self) -> &str {
        "sessions_list"
    }

    fn description(&self) -> &str {
        SESSIONS_LIST_DESCRIPTION
            .get_or_init(|| crate::i18n::get_required_tool_string("tool-sessions-list"))
            .as_str()
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
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "sessions_list")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(50, |v| v as usize);

        // Ownership-filter before the limit so foreign sessions cannot
        // crowd owned sessions out of a capped listing.
        let metadata = match &self.ownership_scope {
            Some(scope) => self
                .backend
                .list_with_authority(&scope.agent_alias, scope.channel_ids.authority().as_ref())?,
            None => self.backend.list_sessions_with_metadata(),
        };
        let mut metadata: Vec<SessionListEntry> = metadata
            .into_iter()
            .map(|meta| SessionListEntry {
                channel: meta.key.split("__").next().unwrap_or(&meta.key).to_string(),
                key: meta.key,
                last_activity: meta.last_activity,
                message_count: meta.message_count,
            })
            .collect();

        if let Some(view) = &self.acp_sessions
            && let Some(acp_sessions) = view.active_summaries()?
        {
            let acp_ids = view.all_session_ids()?;
            metadata.retain(|entry| !acp_ids.contains(&entry.key));
            metadata.extend(acp_sessions.into_iter().map(|summary| SessionListEntry {
                key: summary.session_uuid,
                channel: "acp".to_string(),
                last_activity: summary.last_activity,
                message_count: summary.message_count,
            }));
            metadata.sort_by_key(|entry| std::cmp::Reverse(entry.last_activity));
        }

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
            let _ = writeln!(
                output,
                "- {}: channel={}, messages={}, last_activity={}",
                meta.key, meta.channel, meta.message_count, meta.last_activity
            );
        }

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

struct SessionListEntry {
    key: String,
    channel: String,
    last_activity: chrono::DateTime<chrono::Utc>,
    message_count: usize,
}

// ── SessionsHistoryTool ─────────────────────────────────────────────

/// Reads the message history of a specific session by ID.
pub struct SessionsHistoryTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
    ownership_scope: Option<SessionOwnershipScope>,
    acp_sessions: Option<AcpSessionReadView>,
}

impl SessionsHistoryTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            backend,
            security,
            ownership_scope: None,
            acp_sessions: None,
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
            acp_sessions: None,
        }
    }

    /// Attach an ACP session read view without widening the ownership scope.
    #[must_use]
    pub fn with_optional_acp_sessions(mut self, acp_sessions: Option<AcpSessionReadView>) -> Self {
        self.acp_sessions = acp_sessions;
        self
    }
}

#[async_trait]
impl Tool for SessionsHistoryTool {
    fn name(&self) -> &str {
        "sessions_history"
    }

    fn description(&self) -> &str {
        SESSIONS_HISTORY_DESCRIPTION
            .get_or_init(|| crate::i18n::get_required_tool_string("tool-sessions-history"))
            .as_str()
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

        // ACP protocol sessions are classified first and answered by the
        // ACP view, which enforces its own ownership. Only an ACP `Missing`
        // (or the absence of an ACP view) falls through to the chat backend,
        // where the trusted per-agent ownership scope governs access.
        let mut require_existing_chat_session = false;
        if let Some(view) = &self.acp_sessions {
            match view.classify_target(session_id)? {
                AcpTargetAccess::Owned => {
                    let Some(session) = view.load_owned(session_id)? else {
                        return Ok(session_not_found(session_id));
                    };
                    return Ok(render_acp_history(&session, limit));
                }
                AcpTargetAccess::Foreign => return Ok(session_not_found(session_id)),
                AcpTargetAccess::Inactive => {}
                AcpTargetAccess::Missing => require_existing_chat_session = true,
            }
        }

        let messages = match &self.ownership_scope {
            Some(scope) => {
                let Some(target_session_key) =
                    resolve_existing_session_key(self.backend.as_ref(), session_id)
                else {
                    // A chat session that does not resolve is reported as not
                    // found rather than falling open to an unscoped load.
                    if require_existing_chat_session {
                        return Ok(session_not_found(session_id));
                    }
                    return Ok(ToolResult {
                        success: true,
                        output: format!("No messages found for session '{session_id}'.").into(),
                        error: None,
                    });
                };
                match self.backend.load_with_authority(
                    &target_session_key,
                    &scope.agent_alias,
                    scope.channel_ids.authority().as_ref(),
                ) {
                    Ok(ScopedSessionAccess::Granted(messages)) => messages,
                    Ok(ScopedSessionAccess::Missing) => Vec::new(),
                    Ok(ScopedSessionAccess::Denied(denial)) => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(scope.denial_message(session_id, &denial)),
                        });
                    }
                    Err(error) => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(format!(
                                "Failed to read session '{session_id}' after ownership verification: {error}"
                            )),
                        });
                    }
                }
            }
            None => {
                let resolved_session_id = if require_existing_chat_session {
                    let Some(session_key) =
                        resolve_existing_session_key(self.backend.as_ref(), session_id)
                    else {
                        return Ok(session_not_found(session_id));
                    };
                    session_key
                } else {
                    session_id.to_string()
                };
                self.backend.load(&resolved_session_id)
            }
        };

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
            "{}\n",
            session_history_header(session_id, tail.len(), messages.len())
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

fn render_acp_history(session: &AcpSessionData, limit: usize) -> ToolResult {
    let start = session.messages.len().saturating_sub(limit);
    let tail = &session.messages[start..];
    let mut output = format!(
        "{}\n",
        session_history_header(&session.session_uuid, tail.len(), session.messages.len())
    );
    for message in tail {
        match message {
            zeroclaw_api::model_provider::ConversationMessage::Chat(chat) => {
                let _ = writeln!(output, "[{}] {}", chat.role, chat.content);
            }
            zeroclaw_api::model_provider::ConversationMessage::AssistantToolCalls {
                text,
                tool_calls,
                ..
            } => {
                if let Some(text) = text.as_deref().filter(|text| !text.is_empty()) {
                    let _ = writeln!(output, "[assistant] {text}");
                }
                for call in tool_calls {
                    let _ = writeln!(output, "[assistant tool_call] {}", call.name);
                }
            }
            zeroclaw_api::model_provider::ConversationMessage::ToolResults(results) => {
                for result in results {
                    let _ = writeln!(output, "[tool {}] {}", result.tool_name, result.content);
                }
            }
        }
    }
    ToolResult {
        success: true,
        output: output.into(),
        error: None,
    }
}

fn session_not_found(session_id: &str) -> ToolResult {
    ToolResult {
        success: false,
        output: ToolOutput::default(),
        error: Some(format!(
            "Session '{session_id}' not found. Use sessions_list or sessions_current to choose an existing session. Gateway dashboard sessions are stored as 'gw_<session_id>'."
        )),
    }
}

// ── SessionsSendTool ────────────────────────────────────────────────

/// Sends a message to a specific session, enabling inter-agent communication.
pub struct SessionsSendTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
    ownership_scope: Option<SessionOwnershipScope>,
    acp_sessions: Option<AcpSessionReadView>,
}

impl SessionsSendTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            backend,
            security,
            ownership_scope: None,
            acp_sessions: None,
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
            acp_sessions: None,
        }
    }

    /// Attach an ACP session read view without widening the ownership scope.
    #[must_use]
    pub fn with_optional_acp_sessions(mut self, acp_sessions: Option<AcpSessionReadView>) -> Self {
        self.acp_sessions = acp_sessions;
        self
    }
}

#[async_trait]
impl Tool for SessionsSendTool {
    fn name(&self) -> &str {
        "sessions_send"
    }

    fn description(&self) -> &str {
        SESSIONS_SEND_DESCRIPTION
            .get_or_init(|| crate::i18n::get_required_tool_string("tool-sessions-send"))
            .as_str()
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

        if let Some(view) = &self.acp_sessions {
            match view.classify_target(session_id)? {
                AcpTargetAccess::Owned => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(acp_send_unsupported()),
                    });
                }
                AcpTargetAccess::Foreign => return Ok(session_not_found(session_id)),
                AcpTargetAccess::Inactive | AcpTargetAccess::Missing => {}
            }
        }

        let Some(target_session_key) =
            resolve_existing_session_key(self.backend.as_ref(), session_id)
        else {
            return Ok(session_not_found(session_id));
        };

        let chat_msg = zeroclaw_api::model_provider::ChatMessage::user(message);
        let access = match &self.ownership_scope {
            Some(scope) => self.backend.append_with_authority(
                &target_session_key,
                &chat_msg,
                &scope.agent_alias,
                scope.channel_ids.authority().as_ref(),
            ),
            None => self
                .backend
                .append(&target_session_key, &chat_msg)
                .map(|()| ScopedSessionAccess::Granted(())),
        };

        match access {
            Ok(ScopedSessionAccess::Granted(())) => {
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
            Ok(ScopedSessionAccess::Missing) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Session '{session_id}' not found. Use sessions_list or sessions_current to choose an existing session. Gateway dashboard sessions are stored as 'gw_<session_id>'."
                )),
            }),
            Ok(ScopedSessionAccess::Denied(denial)) => {
                let error = self.ownership_scope.as_ref().map_or_else(
                    || "Session ownership check failed unexpectedly.".to_string(),
                    |scope| scope.denial_message(session_id, &denial),
                );
                Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error),
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
    acp_sessions: Option<AcpSessionReadView>,
}

impl SessionsCurrentTool {
    pub fn new(backend: Arc<dyn SessionBackend>) -> Self {
        Self {
            backend,
            acp_sessions: None,
        }
    }

    pub fn with_acp_sessions(
        backend: Arc<dyn SessionBackend>,
        acp_sessions: AcpSessionReadView,
    ) -> Self {
        Self {
            backend,
            acp_sessions: Some(acp_sessions),
        }
    }

    /// Attach an optional ACP session read view.
    #[must_use]
    pub fn with_optional_acp_sessions(mut self, acp_sessions: Option<AcpSessionReadView>) -> Self {
        self.acp_sessions = acp_sessions;
        self
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
        let session_key = current_session_key();

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
        if let Some(view) = &self.acp_sessions
            && view.is_active()?
        {
            let channel =
                tool_msg_with_args("tool-sessions-current-channel", &[("channel", "acp")]);
            let _ = writeln!(output, "{channel}");
            return Ok(ToolResult {
                success: true,
                output: output.into(),
                error: None,
            });
        }
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
}

impl SessionResetTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            backend,
            security,
            ownership_scope: None,
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
        }
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

        match self.backend.clear_messages(&target_session_key) {
            Ok(0) => Ok(ToolResult {
                success: true,
                output: format!("Session '{target_session_key}' is already empty.").into(),
                error: None,
            }),
            Ok(count) => Ok(ToolResult {
                success: true,
                output: format!("Session '{target_session_key}' reset ({count} messages cleared).")
                    .into(),
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

// ── SessionDeleteTool ──────────────────────────────────────────────

/// Permanently deletes a session and all its messages. The session key
/// becomes invalid and must be recreated for new conversations.
pub struct SessionDeleteTool {
    backend: Arc<dyn SessionBackend>,
    security: Arc<SecurityPolicy>,
    ownership_scope: Option<SessionOwnershipScope>,
}

impl SessionDeleteTool {
    pub fn new(backend: Arc<dyn SessionBackend>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            backend,
            security,
            ownership_scope: None,
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
        }
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

        fn load_with_authority(
            &self,
            key: &str,
            agent_alias: &str,
            authority: &zeroclaw_infra::session_backend::ChannelAuthority,
        ) -> std::io::Result<ScopedSessionAccess<Vec<ChatMessage>>> {
            let metadata = self.metadata.lock().unwrap();
            let mut result = Err(std::io::Error::other("authority unavailable"));
            authority(&mut |channel_ids| {
                result = (|| {
                    let Some(session) = metadata
                        .get(key)
                        .cloned()
                        .or_else(|| self.inner.get_session_metadata(key))
                    else {
                        return Ok(ScopedSessionAccess::Missing);
                    };
                    if let Err(denial) = check_session_ownership(
                        session.agent_alias.as_deref(),
                        session.channel_id.as_deref(),
                        agent_alias,
                        channel_ids,
                    ) {
                        return Ok(ScopedSessionAccess::Denied(denial));
                    }
                    Ok(ScopedSessionAccess::Granted(self.inner.load(key)))
                })();
            });
            result
        }

        fn append_with_authority(
            &self,
            key: &str,
            msg: &ChatMessage,
            agent_alias: &str,
            authority: &zeroclaw_infra::session_backend::ChannelAuthority,
        ) -> std::io::Result<ScopedSessionAccess<()>> {
            let metadata = self.metadata.lock().unwrap();
            let mut result = Err(std::io::Error::other("authority unavailable"));
            authority(&mut |channel_ids| {
                result = (|| {
                    let Some(session) = metadata
                        .get(key)
                        .cloned()
                        .or_else(|| self.inner.get_session_metadata(key))
                    else {
                        return Ok(ScopedSessionAccess::Missing);
                    };
                    if let Err(denial) = check_session_ownership(
                        session.agent_alias.as_deref(),
                        session.channel_id.as_deref(),
                        agent_alias,
                        channel_ids,
                    ) {
                        return Ok(ScopedSessionAccess::Denied(denial));
                    }
                    self.inner.append(key, msg)?;
                    Ok(ScopedSessionAccess::Granted(()))
                })();
            });
            result
        }

        fn list_with_authority(
            &self,
            agent_alias: &str,
            authority: &zeroclaw_infra::session_backend::ChannelAuthority,
        ) -> std::io::Result<Vec<SessionMetadata>> {
            let mut result = Vec::new();
            authority(&mut |channels| {
                result = self
                    .list_sessions_with_metadata()
                    .into_iter()
                    .filter(|meta| {
                        check_session_ownership(
                            meta.agent_alias.as_deref(),
                            meta.channel_id.as_deref(),
                            agent_alias,
                            channels,
                        )
                        .is_ok()
                    })
                    .collect();
            });
            Ok(result)
        }

        fn remove_last(&self, key: &str) -> std::io::Result<bool> {
            self.inner.remove_last(key)
        }

        fn list_sessions(&self) -> Vec<String> {
            self.inner.list_sessions()
        }

        fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
            // Serve injected metadata like the SQLite backend serves its
            // attribution columns; fall through for unattributed keys.
            self.inner
                .list_sessions_with_metadata()
                .into_iter()
                .map(|meta| {
                    self.metadata
                        .lock()
                        .unwrap()
                        .get(&meta.key)
                        .cloned()
                        .unwrap_or(meta)
                })
                .collect()
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
    }

    /// Test seam that pauses immediately before session access. The historical
    /// implementation reached `load` or `append` only after a separate metadata
    /// authorization, so this barrier deterministically opened its check/use
    /// window. The ownership-conditional methods instead resume into one stable
    /// backend operation.
    struct PauseBeforeAccessBackend {
        inner: Arc<dyn SessionBackend>,
        access_ready: std::sync::mpsc::SyncSender<()>,
        resume_access: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl PauseBeforeAccessBackend {
        fn pause(&self) {
            self.access_ready.send(()).unwrap();
            self.resume_access.lock().unwrap().recv().unwrap();
        }
    }

    impl SessionBackend for PauseBeforeAccessBackend {
        fn load(&self, key: &str) -> Vec<ChatMessage> {
            self.pause();
            self.inner.load(key)
        }

        fn append(&self, key: &str, msg: &ChatMessage) -> std::io::Result<()> {
            self.pause();
            self.inner.append(key, msg)
        }

        fn load_with_authority(
            &self,
            key: &str,
            agent_alias: &str,
            authority: &zeroclaw_infra::session_backend::ChannelAuthority,
        ) -> std::io::Result<ScopedSessionAccess<Vec<ChatMessage>>> {
            self.pause();
            self.inner.load_with_authority(key, agent_alias, authority)
        }

        fn append_with_authority(
            &self,
            key: &str,
            msg: &ChatMessage,
            agent_alias: &str,
            authority: &zeroclaw_infra::session_backend::ChannelAuthority,
        ) -> std::io::Result<ScopedSessionAccess<()>> {
            self.pause();
            self.inner
                .append_with_authority(key, msg, agent_alias, authority)
        }

        fn remove_last(&self, key: &str) -> std::io::Result<bool> {
            self.inner.remove_last(key)
        }

        fn list_sessions(&self) -> Vec<String> {
            self.inner.list_sessions()
        }

        fn get_session_metadata(&self, key: &str) -> Option<SessionMetadata> {
            self.inner.get_session_metadata(key)
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
            principal_id: None,
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
        let tool = SessionsListTool::new(backend, test_security());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No active sessions"));
    }

    #[tokio::test]
    async fn list_sessions_shows_all() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsListTool::new(backend, test_security());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("2 session(s)"));
        assert!(result.output.contains("telegram__alice"));
        assert!(result.output.contains("discord__bob"));
    }

    #[tokio::test]
    async fn list_sessions_respects_limit() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsListTool::new(backend, test_security());
        let result = tool.execute(json!({"limit": 1})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("1 session(s)"));
    }

    #[tokio::test]
    async fn list_sessions_extracts_channel() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsListTool::new(backend, test_security());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.output.contains("channel=telegram"));
        assert!(result.output.contains("channel=discord"));
    }

    #[test]
    fn list_tool_name_and_schema() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsListTool::new(backend, test_security());
        assert_eq!(tool.name(), "sessions_list");
        assert!(tool.description().contains("this agent's active"));
        assert!(tool.parameters_schema()["properties"]["limit"].is_object());
    }

    #[tokio::test]
    async fn list_scoped_shows_only_owned_sessions() {
        let (_tmp, backend) = seeded_metadata_backend(vec![
            session_metadata("telegram__alice", Some("rowan"), None, 2),
            session_metadata("discord__bob", Some("sable"), None, 1),
        ]);
        let tool = SessionsListTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("1 session(s)"));
        assert!(result.output.contains("telegram__alice"));
        assert!(!result.output.contains("discord__bob"));
    }

    #[tokio::test]
    async fn list_scoped_includes_owned_channel_sessions() {
        let (_tmp, backend) = seeded_metadata_backend(vec![
            session_metadata("telegram__alice", None, Some("telegram.default"), 2),
            session_metadata("discord__bob", None, Some("discord.default"), 1),
        ]);
        let tool = SessionsListTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::with_channels("rowan", ["telegram.default"]),
        );

        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("telegram__alice"));
        assert!(!result.output.contains("discord__bob"));
    }

    #[tokio::test]
    async fn list_scoped_hides_unattributed_sessions() {
        // seeded_backend has no metadata rows: every session is legacy.
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsListTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("No active sessions"));
    }

    #[tokio::test]
    async fn list_scoped_filters_before_limit() {
        // Foreign sessions must not consume limit slots: with limit 1 and
        // the owned session sorted after the foreign ones, the owned
        // session must still be the one returned.
        let (_tmp, backend) = seeded_metadata_backend(vec![
            session_metadata("telegram__alice", Some("sable"), None, 2),
            session_metadata("discord__bob", Some("rowan"), None, 1),
        ]);
        let tool = SessionsListTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool.execute(json!({"limit": 1})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("1 session(s)"));
        assert!(result.output.contains("discord__bob"));
        assert!(!result.output.contains("telegram__alice"));
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

    #[tokio::test]
    async fn history_scoped_allows_own_agent_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            Some("rowan"),
            None,
            2,
        )]);
        let tool = SessionsHistoryTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("[user] Hello from Alice"));
    }

    #[tokio::test]
    async fn history_scoped_denies_other_agent_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            Some("sable"),
            None,
            2,
        )]);
        let tool = SessionsHistoryTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("owned by agent 'sable'"));
        assert!(!result.output.contains("Hello from Alice"));
    }

    #[tokio::test]
    async fn history_scoped_allows_owned_channel_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            None,
            Some("telegram.default"),
            2,
        )]);
        let tool = SessionsHistoryTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::with_channels("rowan", ["telegram.default"]),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("[user] Hello from Alice"));
    }

    #[tokio::test]
    async fn history_scoped_denies_unowned_channel_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            None,
            Some("telegram.default"),
            2,
        )]);
        let tool = SessionsHistoryTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::with_channels("rowan", ["discord.default"]),
        );

        let result = tool
            .execute(json!({"session_id": "telegram__alice"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not owned by agent 'rowan'"));
    }

    #[tokio::test]
    async fn history_scoped_denies_legacy_unattributed_session() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsHistoryTool::for_agent(
            backend,
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
        assert!(!result.output.contains("Hello from Alice"));
    }

    #[tokio::test]
    async fn history_scoped_resolves_gateway_prefix_for_owned_session() {
        let (_tmp, inner) = test_backend();
        inner
            .append("gw_operator-1", &ChatMessage::user("dashboard turn"))
            .unwrap();
        let backend: Arc<dyn SessionBackend> = Arc::new(MetadataBackend::new(
            inner,
            vec![session_metadata("gw_operator-1", Some("rowan"), None, 1)],
        ));
        let tool = SessionsHistoryTool::for_agent(
            backend,
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({"session_id": "operator-1"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("[user] dashboard turn"));
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

    #[tokio::test]
    async fn send_scoped_allows_own_agent_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            Some("rowan"),
            None,
            2,
        )]);
        let tool = SessionsSendTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({
                "session_id": "telegram__alice",
                "message": "own-session message"
            }))
            .await
            .unwrap();

        assert!(result.success);
        let messages = backend.load("telegram__alice");
        assert_eq!(messages.last().unwrap().content, "own-session message");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn history_rechecks_ownership_atomically_after_concurrent_reassignment() {
        for backend_name in ["sqlite", "jsonl"] {
            let tmp = TempDir::new().unwrap();
            let inner = zeroclaw_infra::make_session_backend(tmp.path(), backend_name).unwrap();
            inner
                .append("gw_shared", &ChatMessage::user("agent_a private history"))
                .unwrap();
            inner
                .set_session_agent_alias("gw_shared", "agent_a")
                .unwrap();

            let (access_ready, access_rx) = std::sync::mpsc::sync_channel(0);
            let (resume_tx, resume_access) = std::sync::mpsc::sync_channel(0);
            let backend: Arc<dyn SessionBackend> = Arc::new(PauseBeforeAccessBackend {
                inner: Arc::clone(&inner),
                access_ready,
                resume_access: Mutex::new(resume_access),
            });
            let tool = SessionsHistoryTool::for_agent(
                backend,
                test_security(),
                SessionOwnershipScope::for_agent("agent_a"),
            );

            let task = zeroclaw_spawn::spawn!(async move {
                tool.execute(json!({"session_id": "shared"})).await.unwrap()
            });
            access_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("history must pause immediately before backend access");
            inner
                .set_session_agent_alias("gw_shared", "agent_b")
                .unwrap();
            inner
                .append("gw_shared", &ChatMessage::user("agent_b private history"))
                .unwrap();
            resume_tx.send(()).unwrap();

            let result = task.await.unwrap();
            assert!(!result.success, "backend={backend_name}: {result:?}");
            assert!(
                result
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("owned by agent 'agent_b'")),
                "backend={backend_name}: {result:?}"
            );
            assert!(!result.output.contains("agent_b private history"));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_rechecks_ownership_atomically_after_concurrent_reassignment() {
        for backend_name in ["sqlite", "jsonl"] {
            let tmp = TempDir::new().unwrap();
            let inner = zeroclaw_infra::make_session_backend(tmp.path(), backend_name).unwrap();
            inner
                .append("gw_shared", &ChatMessage::user("existing message"))
                .unwrap();
            inner
                .set_session_agent_alias("gw_shared", "agent_a")
                .unwrap();

            let (access_ready, access_rx) = std::sync::mpsc::sync_channel(0);
            let (resume_tx, resume_access) = std::sync::mpsc::sync_channel(0);
            let backend: Arc<dyn SessionBackend> = Arc::new(PauseBeforeAccessBackend {
                inner: Arc::clone(&inner),
                access_ready,
                resume_access: Mutex::new(resume_access),
            });
            let tool = SessionsSendTool::for_agent(
                backend,
                test_security(),
                SessionOwnershipScope::for_agent("agent_a"),
            );

            let task = zeroclaw_spawn::spawn!(async move {
                tool.execute(json!({
                    "session_id": "shared",
                    "message": "must not cross ownership"
                }))
                .await
                .unwrap()
            });
            access_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("send must pause immediately before backend access");
            inner
                .set_session_agent_alias("gw_shared", "agent_b")
                .unwrap();
            resume_tx.send(()).unwrap();

            let result = task.await.unwrap();
            assert!(!result.success, "backend={backend_name}: {result:?}");
            assert!(
                result
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("owned by agent 'agent_b'")),
                "backend={backend_name}: {result:?}"
            );
            let messages = inner.load("gw_shared");
            assert_eq!(messages.len(), 1, "backend={backend_name}");
            assert!(
                messages
                    .iter()
                    .all(|message| message.content != "must not cross ownership"),
                "backend={backend_name}: {messages:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retained_tools_resolve_channel_grant_after_backend_admission() {
        for backend_name in ["sqlite", "jsonl"] {
            for send in [false, true] {
                for revoked in [false, true] {
                    let tmp = TempDir::new().unwrap();
                    let inner =
                        zeroclaw_infra::make_session_backend(tmp.path(), backend_name).unwrap();
                    inner
                        .append("gw_shared", &ChatMessage::user("channel private history"))
                        .unwrap();
                    inner
                        .set_session_context(
                            "gw_shared",
                            zeroclaw_infra::session_backend::SessionContext {
                                channel_id: Some("discord.owner"),
                                ..Default::default()
                            },
                        )
                        .unwrap();
                    let policy = Arc::new(std::sync::RwLock::new(BTreeSet::from([
                        "discord.owner".to_string(),
                    ])));
                    let reader = Arc::clone(&policy);
                    let scope = SessionOwnershipScope::with_live_channels(
                        "owner",
                        Arc::new(move |effect| effect(&reader.read().unwrap())),
                    );
                    let (access_ready, access_rx) = std::sync::mpsc::sync_channel(0);
                    let (resume_tx, resume_access) = std::sync::mpsc::sync_channel(0);
                    let backend: Arc<dyn SessionBackend> = Arc::new(PauseBeforeAccessBackend {
                        inner: Arc::clone(&inner),
                        access_ready,
                        resume_access: Mutex::new(resume_access),
                    });
                    let tool: Arc<dyn Tool> = if send {
                        Arc::new(SessionsSendTool::for_agent(backend, test_security(), scope))
                    } else {
                        Arc::new(SessionsHistoryTool::for_agent(
                            backend,
                            test_security(),
                            scope,
                        ))
                    };
                    let task = zeroclaw_spawn::spawn!(async move {
                        tool.execute(json!({"session_id":"shared", "message":"new message"}))
                            .await
                            .unwrap()
                    });
                    access_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .unwrap();
                    if revoked {
                        policy.write().unwrap().clear();
                    }
                    resume_tx.send(()).unwrap();
                    let result = task.await.unwrap();
                    assert_eq!(
                        result.success, !revoked,
                        "{backend_name} send={send} revoked={revoked}: {result:?}"
                    );
                    if revoked {
                        assert!(!result.output.contains("channel private history"));
                    }
                    assert_eq!(
                        inner.load("gw_shared").len(),
                        if send && !revoked { 2 } else { 1 }
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn configured_jsonl_backend_supports_all_scoped_session_tools_after_restart() {
        let tmp = TempDir::new().unwrap();
        {
            let backend = zeroclaw_infra::make_session_backend(tmp.path(), "jsonl").unwrap();
            backend
                .append("discord__operator", &ChatMessage::user("private turn"))
                .unwrap();
            backend
                .set_session_agent_alias("discord__operator", "rowan")
                .unwrap();
        }

        let backend = zeroclaw_infra::make_session_backend(tmp.path(), "jsonl").unwrap();
        let scope = SessionOwnershipScope::for_agent("rowan");

        let listed =
            SessionsListTool::for_agent(Arc::clone(&backend), test_security(), scope.clone())
                .execute(json!({}))
                .await
                .unwrap();
        assert!(listed.success);
        assert!(listed.output.contains("discord__operator"));

        let history =
            SessionsHistoryTool::for_agent(Arc::clone(&backend), test_security(), scope.clone())
                .execute(json!({"session_id": "discord__operator"}))
                .await
                .unwrap();
        assert!(history.success);
        assert!(history.output.contains("private turn"));

        let sent = SessionsSendTool::for_agent(Arc::clone(&backend), test_security(), scope)
            .execute(json!({
                "session_id": "discord__operator",
                "message": "owned follow-up"
            }))
            .await
            .unwrap();
        assert!(sent.success);
        assert_eq!(
            backend.load("discord__operator").last().unwrap().content,
            "owned follow-up"
        );
    }

    #[tokio::test]
    async fn archived_jsonl_session_cannot_be_listed_or_resurrected_by_scoped_send() {
        let tmp = TempDir::new().unwrap();
        let session_key = format!(
            "{}-owned",
            (chrono::Local::now().date_naive() - chrono::Duration::days(10)).format("%Y-%m-%d")
        );
        let backend = zeroclaw_infra::make_session_backend(tmp.path(), "jsonl").unwrap();
        backend
            .append(&session_key, &ChatMessage::user("retained private turn"))
            .unwrap();
        backend
            .set_session_agent_alias(&session_key, "rowan")
            .unwrap();

        let hygiene = zeroclaw_config::schema::MemoryConfig {
            hygiene_enabled: true,
            archive_after_days: 7,
            purge_after_days: 0,
            ..Default::default()
        };
        zeroclaw_memory::hygiene::run_if_due(&hygiene, tmp.path()).unwrap();

        assert!(backend.list_sessions().is_empty());
        let scope = SessionOwnershipScope::for_agent("rowan");
        let listed =
            SessionsListTool::for_agent(Arc::clone(&backend), test_security(), scope.clone())
                .execute(json!({}))
                .await
                .unwrap();
        assert!(listed.success);
        assert!(listed.output.contains("No active sessions"));

        let sent = SessionsSendTool::for_agent(Arc::clone(&backend), test_security(), scope)
            .execute(json!({
                "session_id": session_key,
                "message": "must not resurrect"
            }))
            .await
            .unwrap();
        assert!(!sent.success);
        assert!(
            sent.error
                .as_deref()
                .is_some_and(|error| error.contains("not found"))
        );
        assert!(backend.list_sessions().is_empty());
        assert!(
            !tmp.path()
                .join("sessions")
                .join(format!("{session_key}.jsonl"))
                .exists()
        );
        assert!(
            !tmp.path()
                .join("sessions")
                .join(format!("{session_key}.metadata.json"))
                .exists()
        );
    }

    #[tokio::test]
    async fn send_scoped_denies_other_agent_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            Some("sable"),
            None,
            2,
        )]);
        let tool = SessionsSendTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({
                "session_id": "telegram__alice",
                "message": "injected"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("owned by agent 'sable'"));
        // The foreign session must not receive the message.
        assert_eq!(backend.load("telegram__alice").len(), 2);
    }

    #[tokio::test]
    async fn send_scoped_allows_owned_channel_session() {
        let (_tmp, backend) = seeded_metadata_backend(vec![session_metadata(
            "telegram__alice",
            None,
            Some("telegram.default"),
            2,
        )]);
        let tool = SessionsSendTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::with_channels("rowan", ["telegram.default"]),
        );

        let result = tool
            .execute(json!({
                "session_id": "telegram__alice",
                "message": "channel-owned message"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(backend.load("telegram__alice").len(), 3);
    }

    #[tokio::test]
    async fn send_scoped_denies_legacy_unattributed_session() {
        let (_tmp, backend) = seeded_backend();
        let tool = SessionsSendTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({
                "session_id": "telegram__alice",
                "message": "injected"
            }))
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
    async fn send_scoped_unknown_session_reports_not_found() {
        let (_tmp, backend) = test_backend();
        let tool = SessionsSendTool::for_agent(
            backend.clone(),
            test_security(),
            SessionOwnershipScope::for_agent("rowan"),
        );

        let result = tool
            .execute(json!({
                "session_id": "operator-1",
                "message": "hello"
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
        assert!(result.output.contains("2 messages cleared"));
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

    fn acp_fixture() -> (
        TempDir,
        Arc<AcpSessionStore>,
        AcpSessionReadView,
        String,
        String,
        String,
    ) {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(AcpSessionStore::new(tmp.path()).unwrap());
        let current = "11111111-1111-4111-8111-111111111111".to_string();
        let other = "22222222-2222-4222-8222-222222222222".to_string();
        let foreign = "foreign-acp-session".to_string();
        store
            .create_session(&current, "rowan", "/current", None)
            .unwrap();
        store
            .append_turn(
                &current,
                &[zeroclaw_api::model_provider::ConversationMessage::Chat(
                    ChatMessage::user("current prompt"),
                )],
            )
            .unwrap();
        store
            .create_session(&other, "rowan", "/other", None)
            .unwrap();
        store
            .append_turn(
                &other,
                &[zeroclaw_api::model_provider::ConversationMessage::Chat(
                    ChatMessage::assistant("other answer"),
                )],
            )
            .unwrap();
        store
            .create_session(&foreign, "sable", "/foreign", None)
            .unwrap();
        let view = AcpSessionReadView::new(store.clone(), "rowan");
        (tmp, store, view, current, other, foreign)
    }

    #[test]
    fn acp_session_output_fluent_keys_interpolate_protocol_values() {
        let header = session_history_header("session-alpha", 37, 89);
        assert!(header.contains("session-alpha"));
        assert!(header.contains("37"));
        assert!(header.contains("89"));
        assert!(!header.contains("{tool-sessions-history-header}"));

        let channel = tool_msg_with_args("tool-sessions-current-channel", &[("channel", "acp")]);
        assert!(channel.contains("acp"));
        assert!(!channel.contains("{tool-sessions-current-channel}"));

        let unsupported = acp_send_unsupported();
        assert!(unsupported.contains("sessions_send"));
        assert!(unsupported.contains("ACP"));
        assert!(unsupported.contains("Code"));
        assert!(!unsupported.contains("{tool-sessions-send-error-acp-unsupported}"));
    }

    #[tokio::test]
    async fn acp_current_list_and_history_share_owned_session_view() {
        let (_acp_tmp, _store, view, current, other, _foreign) = acp_fixture();
        let (_chat_tmp, backend) = test_backend();
        let current_tool = SessionsCurrentTool::with_acp_sessions(backend.clone(), view.clone());
        let list_tool = SessionsListTool::new(backend.clone(), test_security())
            .with_optional_acp_sessions(Some(view.clone()));
        let history_tool = SessionsHistoryTool::new(backend, test_security())
            .with_optional_acp_sessions(Some(view));

        zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(current.clone()), async {
                let current_result = current_tool.execute(json!({})).await.unwrap();
                assert!(current_result.success);
                assert!(current_result.output.contains(&current));
                let channel =
                    tool_msg_with_args("tool-sessions-current-channel", &[("channel", "acp")]);
                assert!(current_result.output.contains(&channel));

                let list_result = list_tool.execute(json!({})).await.unwrap();
                assert!(list_result.success);
                assert!(list_result.output.contains(&current));
                assert!(list_result.output.contains(&other));

                let current_history = history_tool
                    .execute(json!({"session_id": current}))
                    .await
                    .unwrap();
                assert!(current_history.success);
                assert!(
                    current_history
                        .output
                        .starts_with(&session_history_header(&current, 1, 1))
                );
                assert!(current_history.output.contains("current prompt"));

                let other_history = history_tool
                    .execute(json!({"session_id": other}))
                    .await
                    .unwrap();
                assert!(other_history.success);
                assert!(
                    other_history
                        .output
                        .starts_with(&session_history_header(&other, 1, 1))
                );
                assert!(other_history.output.contains("other answer"));
            })
            .await;
    }

    #[tokio::test]
    async fn killed_acp_targets_are_hidden_from_list_but_owned_history_remains_readable() {
        let (_acp_tmp, store, view, current, other, foreign) = acp_fixture();
        store
            .append_turn(
                &foreign,
                &[zeroclaw_api::model_provider::ConversationMessage::Chat(
                    ChatMessage::assistant("foreign retained answer"),
                )],
            )
            .unwrap();
        assert!(store.mark_session_killed(&other).unwrap());
        assert!(store.mark_session_killed(&foreign).unwrap());
        let (_chat_tmp, backend) = test_backend();
        let list = SessionsListTool::new(backend.clone(), test_security())
            .with_optional_acp_sessions(Some(view.clone()));
        let history = SessionsHistoryTool::new(backend, test_security())
            .with_optional_acp_sessions(Some(view));

        zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(current.clone()), async {
                let listed = list.execute(json!({})).await.unwrap();
                assert!(listed.success);
                assert!(listed.output.contains(&current));
                assert!(!listed.output.contains(&other));
                assert!(!listed.output.contains(&foreign));

                let owned = history.execute(json!({"session_id": other})).await.unwrap();
                assert!(owned.success);
                assert!(owned.output.contains("other answer"));

                let denied = history
                    .execute(json!({"session_id": foreign}))
                    .await
                    .unwrap();
                let missing = history
                    .execute(json!({"session_id": "missing-acp-session"}))
                    .await
                    .unwrap();
                assert!(!denied.success);
                assert!(!missing.success);
                assert!(!denied.output.contains("foreign retained answer"));
                assert_eq!(
                    denied.error.unwrap().replace(&foreign, "<id>"),
                    missing
                        .error
                        .unwrap()
                        .replace("missing-acp-session", "<id>")
                );
            })
            .await;

        assert!(store.is_session_killed(&other).unwrap());
        assert!(store.is_session_killed(&foreign).unwrap());
    }

    #[tokio::test]
    async fn acp_list_unions_before_sorting_and_limit() {
        let (_acp_tmp, _store, view, current, _other, _foreign) = acp_fixture();
        let (_chat_tmp, inner) = test_backend();
        let old_chat = SessionMetadata {
            key: "telegram__old".into(),
            name: None,
            created_at: Utc::now() - chrono::Duration::days(2),
            last_activity: Utc::now() - chrono::Duration::days(1),
            message_count: 1,
            agent_alias: Some("rowan".into()),
            channel_id: Some("telegram.default".into()),
            room_id: None,
            sender_id: None,
            principal_id: None,
        };
        let backend: Arc<dyn SessionBackend> =
            Arc::new(MetadataBackend::new(inner, vec![old_chat]));
        let tool =
            SessionsListTool::new(backend, test_security()).with_optional_acp_sessions(Some(view));

        let result = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(current.clone()), tool.execute(json!({"limit": 1})))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("channel=acp"));
        assert!(!result.output.contains("telegram__old"));
    }

    #[tokio::test]
    async fn non_acp_invocation_for_same_alias_remains_chat_only() {
        let (_acp_tmp, _store, view, current, _other, _foreign) = acp_fixture();
        let (_chat_tmp, backend) = seeded_backend();
        let tool =
            SessionsListTool::new(backend, test_security()).with_optional_acp_sessions(Some(view));

        let result = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("telegram__alice".into()), tool.execute(json!({})))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("telegram__alice"));
        assert!(!result.output.contains(&current));
    }

    #[tokio::test]
    async fn foreign_acp_uuid_is_indistinguishable_from_unknown_and_send_is_unsupported() {
        let (_acp_tmp, store, view, current, _other, foreign) = acp_fixture();
        let (_chat_tmp, backend) = test_backend();
        let history = SessionsHistoryTool::new(backend.clone(), test_security())
            .with_optional_acp_sessions(Some(view.clone()));
        let send =
            SessionsSendTool::new(backend, test_security()).with_optional_acp_sessions(Some(view));
        let unknown = "missing-acp-session";

        zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(current.clone()), async {
                let foreign_result = history
                    .execute(json!({"session_id": foreign}))
                    .await
                    .unwrap();
                let unknown_result = history
                    .execute(json!({"session_id": unknown}))
                    .await
                    .unwrap();
                assert!(!foreign_result.success);
                assert!(!unknown_result.success);
                assert_eq!(
                    foreign_result.error.unwrap().replace(&foreign, "<id>"),
                    unknown_result.error.unwrap().replace(unknown, "<id>")
                );

                let foreign_send = send
                    .execute(json!({"session_id": foreign, "message": "hello"}))
                    .await
                    .unwrap();
                let unknown_send = send
                    .execute(json!({"session_id": unknown, "message": "hello"}))
                    .await
                    .unwrap();
                assert!(!foreign_send.success);
                assert!(!unknown_send.success);
                assert_eq!(
                    foreign_send.error.unwrap().replace(&foreign, "<id>"),
                    unknown_send.error.unwrap().replace(unknown, "<id>")
                );

                let send_result = send
                    .execute(json!({"session_id": current, "message": "hello"}))
                    .await
                    .unwrap();
                assert!(!send_result.success);
                assert_eq!(send_result.error.unwrap(), acp_send_unsupported());
            })
            .await;

        assert_eq!(
            store
                .load_session_for_agent(&current, "rowan")
                .unwrap()
                .unwrap()
                .messages
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn uuid_shaped_chat_key_falls_back_only_when_absent_from_acp_store() {
        let (_acp_tmp, _store, view, current, _other, _foreign) = acp_fixture();
        let (_chat_tmp, backend) = test_backend();
        let chat_uuid = "44444444-4444-4444-8444-444444444444";
        backend
            .append(chat_uuid, &ChatMessage::user("uuid chat message"))
            .unwrap();
        let history = SessionsHistoryTool::new(backend, test_security())
            .with_optional_acp_sessions(Some(view));

        let result = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(
                Some(current),
                history.execute(json!({"session_id": chat_uuid})),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("uuid chat message"));
    }

    #[tokio::test]
    async fn same_key_collision_prefers_owned_acp_and_is_listed_once() {
        let (_acp_tmp, _store, view, current, _other, _foreign) = acp_fixture();
        let (_chat_tmp, backend) = test_backend();
        backend
            .append(&current, &ChatMessage::user("colliding chat message"))
            .unwrap();
        let list = SessionsListTool::new(backend.clone(), test_security())
            .with_optional_acp_sessions(Some(view.clone()));
        let history = SessionsHistoryTool::new(backend, test_security())
            .with_optional_acp_sessions(Some(view));

        zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(current.clone()), async {
                let listed = list.execute(json!({})).await.unwrap();
                assert_eq!(listed.output.matches(&current).count(), 1);

                let result = history
                    .execute(json!({"session_id": current}))
                    .await
                    .unwrap();
                assert!(result.success);
                assert!(result.output.contains("current prompt"));
                assert!(!result.output.contains("colliding chat message"));
            })
            .await;
    }

    #[tokio::test]
    async fn foreign_acp_collision_hides_chat_session_during_owned_acp_turn() {
        let (_acp_tmp, _store, view, current, _other, foreign) = acp_fixture();
        let (_chat_tmp, backend) = test_backend();
        backend
            .append(
                &foreign,
                &ChatMessage::user("colliding foreign chat message"),
            )
            .unwrap();
        let list =
            SessionsListTool::new(backend, test_security()).with_optional_acp_sessions(Some(view));

        let result = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(current), list.execute(json!({})))
            .await
            .unwrap();

        assert!(result.success);
        assert!(!result.output.contains(&foreign));
        assert!(!result.output.contains("colliding foreign chat message"));
    }

    /// A grant copied at construction keeps answering after the operator has
    /// taken the channel away. The scope must re-read ownership per check so
    /// a revocation actually revokes.
    #[test]
    fn a_retained_scope_stops_admitting_a_revoked_channel() {
        use std::sync::RwLock as StdRwLock;

        let owned: Arc<StdRwLock<BTreeSet<String>>> = Arc::new(StdRwLock::new(
            ["discord.ops".to_string()].into_iter().collect(),
        ));
        let reader = Arc::clone(&owned);
        // The scope is built ONCE, before the policy change, exactly as a
        // retained agent's tool would be.
        let scope = SessionOwnershipScope::with_live_channels(
            "agent-a",
            Arc::new(move |effect| effect(&reader.read().unwrap())),
        );

        let metadata = session_metadata("s1", None, Some("discord.ops"), 1);

        assert!(
            scope.owns_metadata(&metadata),
            "while the binding is held, the channel session is owned"
        );

        // The operator reassigns discord.ops away from agent-a.
        owned.write().unwrap().clear();

        assert!(
            !scope.owns_metadata(&metadata),
            "after revocation the retained scope must stop admitting the channel"
        );

        // Non-vacuous the other way: a fixed scope built from the same
        // starting policy still admits, which is the defect this replaces.
        let stale = SessionOwnershipScope::with_channels("agent-a", ["discord.ops"]);
        assert!(
            stale.owns_metadata(&metadata),
            "a construction-time grant is exactly what keeps answering"
        );
    }
}
