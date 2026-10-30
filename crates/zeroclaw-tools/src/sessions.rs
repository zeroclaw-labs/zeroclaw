//! Session inspection through the execution-scoped, authorized history port.

use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::{Arc, OnceLock};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

static SESSIONS_SEND_DESCRIPTION: OnceLock<String> = OnceLock::new();

fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_tool_string_with_args(key, args)
}

fn tool_msg(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
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

fn native_history_mutation_unsupported(tool: &str) -> ToolResult {
    ToolResult {
        success: false,
        output: ToolOutput::default(),
        error: Some(tool_msg_with_args(
            "tool-sessions-native-mutation-unsupported",
            &[("tool", tool)],
        )),
    }
}

fn render_native_summary(summary: &zeroclaw_api::session_events::views::SessionSummary) -> String {
    tool_msg_with_args(
        "tool-sessions-native-summary",
        &[
            (
                "session_id",
                summary
                    .family_key()
                    .unwrap_or(summary.head.stream_id.as_str()),
            ),
            ("channel", summary.channel_id().unwrap_or("")),
            ("messages", &summary.message_count.to_string()),
            ("last_activity", &summary.last_activity.to_string()),
        ],
    )
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

fn history_reader()
-> anyhow::Result<Arc<dyn zeroclaw_api::session_events::views::SessionHistoryReadPort>> {
    zeroclaw_api::session_events::capture::current_session_execution()
        .and_then(|execution| execution.history_reader())
        .ok_or_else(|| anyhow::Error::msg(tool_msg("tool-sessions-native-reader-unavailable")))
}

/// Session tool backed by the current authorized execution context.
#[derive(Default)]
pub struct SessionsListTool;
impl SessionsListTool {
    pub fn new() -> Self {
        Self
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
            .unwrap_or(50)
            .min(1000) as usize;

        let reader = history_reader()?;
        let summaries = reader.list(limit).await?;
        let output = summaries
            .iter()
            .map(render_native_summary)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult {
            success: true,
            output: if output.is_empty() {
                tool_msg("tool-sessions-native-empty").into()
            } else {
                output.into()
            },
            error: None,
        })
    }
}

/// Session tool backed by the current authorized execution context.
pub struct SessionsHistoryTool {
    security: Arc<SecurityPolicy>,
}
impl SessionsHistoryTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
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
            .unwrap_or(20)
            .clamp(1, 1000) as usize;

        let reader = history_reader()?;
        let Some(page) = reader.history(session_id, limit).await? else {
            return Ok(session_not_found(session_id));
        };
        let mut output = tool_msg_with_args(
            "tool-sessions-native-history",
            &[("session_id", session_id)],
        );
        for message in page.messages {
            let _ = write!(output, "\n[{}] {}", message.role, message.text);
            for file in message.files {
                let _ = write!(output, "\n{file}");
            }
        }
        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

/// Session tool backed by the current authorized execution context.
pub struct SessionsSendTool {
    security: Arc<SecurityPolicy>,
}
impl SessionsSendTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for SessionsSendTool {
    fn name(&self) -> &str {
        "sessions_send"
    }

    fn description(&self) -> &str {
        SESSIONS_SEND_DESCRIPTION
            .get_or_init(|| tool_msg("tool-sessions-send"))
            .as_str()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": tool_msg("tool-sessions-send-param-session-id")
                },
                "message": {
                    "type": "string",
                    "description": tool_msg("tool-sessions-send-param-message")
                }
            },
            "required": ["session_id", "message"]
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
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

        Ok(native_history_mutation_unsupported(self.name()))
    }
}

/// Session tool backed by the current authorized execution context.
#[derive(Default)]
pub struct SessionsCurrentTool;
impl SessionsCurrentTool {
    pub fn new() -> Self {
        Self
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
        let reader = history_reader()?;
        Ok(ToolResult {
            success: true,
            output: render_native_summary(&reader.current().await?).into(),
            error: None,
        })
    }
}

/// Session tool backed by the current authorized execution context.
pub struct SessionResetTool {
    security: Arc<SecurityPolicy>,
}
impl SessionResetTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
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

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
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

        Ok(native_history_mutation_unsupported(self.name()))
    }
}

/// Session tool backed by the current authorized execution context.
pub struct SessionDeleteTool {
    security: Arc<SecurityPolicy>,
}
impl SessionDeleteTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
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

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
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

        Ok(native_history_mutation_unsupported(self.name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    struct NativeReader(std::sync::atomic::AtomicBool);
    #[async_trait]
    impl zeroclaw_api::session_events::views::SessionHistoryReadPort for NativeReader {
        async fn current(
            &self,
        ) -> anyhow::Result<zeroclaw_api::session_events::views::SessionSummary> {
            use zeroclaw_api::session_events::{
                EventCursor, SessionStreamId, views::SessionSummary,
            };
            anyhow::ensure!(
                self.0.load(std::sync::atomic::Ordering::SeqCst),
                "native_read_revoked"
            );
            Ok(SessionSummary {
                head: EventCursor::beginning(SessionStreamId::new()),
                owning_agent: Some("fixture".into()),
                fields: std::collections::BTreeMap::from([(
                    "family_key".into(),
                    serde_json::json!("native"),
                )]),
                created_at: 1,
                last_activity: 2,
                message_count: 1,
                running: false,
                closed: false,
                deleted: false,
            })
        }
        async fn list(
            &self,
            _limit: usize,
        ) -> anyhow::Result<Vec<zeroclaw_api::session_events::views::SessionSummary>> {
            Ok(vec![self.current().await?])
        }
        async fn history(
            &self,
            _family: &str,
            _limit: usize,
        ) -> anyhow::Result<Option<zeroclaw_api::session_events::views::HistoricalMessagePage>>
        {
            self.current().await?;
            Ok(Some(
                zeroclaw_api::session_events::views::HistoricalMessagePage {
                    messages: vec![zeroclaw_api::session_events::views::HistoricalMessage {
                        created_at: None,
                        message_id: zeroclaw_api::session_events::MessageId::new(),
                        role: "user".into(),
                        text: "native event history".into(),
                        files: vec![],
                        attachments: vec![],
                        unresolved_parts: vec![],
                        unavailable_parts: vec![],
                        content_source: None,
                    }],
                    next_cursor: None,
                },
            ))
        }
    }
    struct NativePort(Arc<NativeReader>);
    #[async_trait]
    impl zeroclaw_api::session_events::capture::SessionExecutionPort for NativePort {
        fn history_reader(
            &self,
        ) -> Option<Arc<dyn zeroclaw_api::session_events::views::SessionHistoryReadPort>> {
            Some(self.0.clone())
        }
        async fn execute(
            &self,
            _input: zeroclaw_api::session_events::capture::EngineTurnInput,
            _engine: zeroclaw_api::session_events::capture::EngineTurnFuture<'_>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("reader fixture cannot execute")
        }
    }

    #[tokio::test]
    async fn native_session_tools_use_the_bound_reader_and_propagate_revocation() {
        let reader = Arc::new(NativeReader(std::sync::atomic::AtomicBool::new(true)));
        let port = Arc::new(NativePort(reader.clone()));
        zeroclaw_api::session_events::capture::scope_session_execution(port, async {
            let list = SessionsListTool::new();
            let current = SessionsCurrentTool::new();
            let history = SessionsHistoryTool::new(test_security());
            let listed = list.execute(json!({})).await.expect("native list");
            assert!(listed.output.as_str().contains("native"));
            assert!(!listed.output.as_str().contains("telegram__alice"));
            assert!(
                current
                    .execute(json!({}))
                    .await
                    .expect("current without legacy context key")
                    .output
                    .as_str()
                    .contains("native")
            );
            let result = history
                .execute(json!({"session_id":"native"}))
                .await
                .expect("native history");
            assert!(result.output.as_str().contains("native event history"));
            assert!(!result.output.as_str().contains("stale legacy history"));
            for tool in [
                Box::new(SessionsSendTool::new(test_security())) as Box<dyn Tool>,
                Box::new(SessionResetTool::new(test_security())),
                Box::new(SessionDeleteTool::new(test_security())),
            ] {
                let result = tool
                    .execute(json!({"session_id":"native","message":"late legacy write"}))
                    .await
                    .expect("explicit native rejection");
                assert!(!result.success, "{}", tool.name());
            }
            reader.0.store(false, std::sync::atomic::Ordering::SeqCst);
            assert!(list.execute(json!({})).await.is_err());
            assert!(current.execute(json!({})).await.is_err());
            assert!(
                history
                    .execute(json!({"session_id":"native"}))
                    .await
                    .is_err()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn session_tools_without_bound_reader_fail_closed() {
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(SessionsListTool::new()),
            Box::new(SessionsCurrentTool::new()),
            Box::new(SessionsHistoryTool::new(test_security())),
        ];
        for tool in tools {
            assert!(
                tool.execute(json!({"session_id":"telegram__alice"}))
                    .await
                    .is_err(),
                "{}",
                tool.name()
            );
        }
    }
}
