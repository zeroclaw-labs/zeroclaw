//! ACP (Agent Client Protocol) back-channel.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::elicitation::{
    ElicitationCapabilities, ElicitationMode, ElicitationRequest, ElicitationResponse,
    multi_select_schema, single_select_schema,
};

use crate::orchestrator::acp_server::RpcOutbound;

/// Per-session ACP back-channel. One instance is registered into each tool's
/// channel map at session/new time and torn down on session/stop.
pub struct AcpChannel {
    name: String,
    session_id: String,
    rpc: Arc<RpcOutbound>,
    /// How long to wait for a `session/request_permission` response before
    /// giving up and returning an error. Callers that never respond (crash,
    /// network drop, user closes IDE) would otherwise park `execute_tool_call`
    /// forever and hold the session slot against `max_sessions`.
    approval_timeout: Duration,
    client_caps: ElicitationCapabilities,
}

impl AcpChannel {
    pub fn new(
        name: impl Into<String>,
        session_id: impl Into<String>,
        rpc: Arc<RpcOutbound>,
        approval_timeout: Duration,
        client_caps: ElicitationCapabilities,
    ) -> Self {
        Self {
            name: name.into(),
            session_id: session_id.into(),
            rpc,
            approval_timeout,
            client_caps,
        }
    }

    /// Legacy multiple-choice path — overloads `session/request_permission`
    /// with synthetic `optionId`s. Kept for two minor releases as the
    /// fallback for clients that do not yet advertise `elicitation.form`.
    /// Removal is tracked in the spec under "Backward Compatibility".
    async fn request_choice_via_permission(
        &self,
        question: &str,
        choices: &[String],
        timeout: Duration,
    ) -> anyhow::Result<Option<String>> {
        // Build permission options. Each choice becomes its own option with a
        // synthetic id; we map the response id back to the choice text.
        // `kind` mirrors how Toad/Zed render: `allow_once` looks like a
        // primary action; `reject_once` is the cancel-style fallback.
        let mut options = Vec::with_capacity(choices.len());
        for (i, choice) in choices.iter().enumerate() {
            let kind = if i == choices.len() - 1 && choices.len() > 1 {
                "reject_once"
            } else {
                "allow_once"
            };
            options.push(json!({
                "optionId": format!("choice-{i}"),
                "name": choice,
                "kind": kind,
            }));
        }

        let params = json!({
            "sessionId": self.session_id,
            "options": options,
            // `toolCall` is required by the ACP schema. We use a synthetic
            // ask_user tool call so the client surfaces the prompt with a
            // sensible title.
            "toolCall": {
                "toolCallId": format!("ask-user-{}", uuid::Uuid::new_v4()),
                "title": question,
                "kind": "other",
                "status": "pending",
            }
        });

        let call = self.rpc.request("session/request_permission", params);
        let response = match tokio::time::timeout(timeout, call).await {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => {
                anyhow::bail!("ACP request_permission failed: {} ({})", e.message, e.code)
            }
            Err(_) => anyhow::bail!("ACP request_permission timed out after {timeout:?}"),
        };

        // Response shape: { outcome: { outcome: "selected", optionId: "..." } | { outcome: "cancelled" } }
        let outcome = response.get("outcome");
        let kind = outcome
            .and_then(|o| o.get("outcome"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        match kind {
            "selected" => {
                let option_id = outcome
                    .and_then(|o| o.get("optionId"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let idx = option_id
                    .strip_prefix("choice-")
                    .and_then(|s| s.parse::<usize>().ok());
                match idx.and_then(|i| choices.get(i)) {
                    Some(text) => Ok(Some(text.clone())),
                    None => anyhow::bail!("ACP returned unknown optionId: {option_id}"),
                }
            }
            "cancelled" => Ok(None),
            other => anyhow::bail!("ACP returned unexpected outcome: {other}"),
        }
    }

    /// Form-mode elicitation path — issues `elicitation/create` with a
    /// single-select schema. Used when the client advertises
    /// `clientCapabilities.elicitation.form`.
    async fn request_choice_via_elicitation(
        &self,
        question: &str,
        choices: &[String],
        timeout: Duration,
    ) -> anyhow::Result<Option<String>> {
        let req = ElicitationRequest {
            session_id: self.session_id.clone(),
            mode: ElicitationMode::Form,
            message: question.to_string(),
            requested_schema: single_select_schema(choices),
        };
        debug_assert!(
            matches!(req.mode, ElicitationMode::Form),
            "Phase 1 must not emit URL-mode elicitation"
        );

        let params = serde_json::to_value(&req)?;
        let call = self.rpc.request("elicitation/create", params);
        let response_value = match tokio::time::timeout(timeout, call).await {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => {
                anyhow::bail!("ACP elicitation/create failed: {} ({})", e.message, e.code)
            }
            Err(_) => anyhow::bail!("ACP elicitation/create timed out after {timeout:?}"),
        };

        let parsed: ElicitationResponse = serde_json::from_value(response_value)
            .map_err(|e| anyhow::Error::msg(format!("malformed elicitation response: {e}")))?;
        match parsed {
            ElicitationResponse::Accept { content } => {
                let text =
                    zeroclaw_api::elicitation::decode_single_select_accept(&content, choices)?;
                Ok(Some(text))
            }
            ElicitationResponse::Decline | ElicitationResponse::Cancel => Ok(None),
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for AcpChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
        )
    }
    fn alias(&self) -> &str {
        &self.name
    }
}

/// Map a tool name to the ACP `kind` field for approval prompts.
/// `file_edit` / `file_write` are `"edit"` so clients render a diff view;
/// everything else falls back to `"execute"`.
fn map_approval_kind(tool_name: &str) -> &'static str {
    match tool_name {
        "file_edit" | "file_write" => "edit",
        _ => "execute",
    }
}

fn build_approval_raw_input(
    tool_name: &str,
    raw_arguments: &Option<serde_json::Value>,
) -> serde_json::Value {
    if let Some(args) = raw_arguments {
        match tool_name {
            "file_edit" => {
                let path = args.get("path").cloned().unwrap_or(serde_json::Value::Null);
                let old_text = args
                    .get("old_string")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let new_text = args
                    .get("new_string")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                return json!({ "path": path, "oldText": old_text, "newText": new_text });
            }
            "file_write" => {
                let path = args.get("path").cloned().unwrap_or(serde_json::Value::Null);
                let new_text = args
                    .get("content")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                return json!({ "path": path, "newText": new_text });
            }
            _ => {}
        }
    }
    json!({ "tool": tool_name })
}

fn build_approval_content(
    tool_name: &str,
    raw_arguments: &Option<serde_json::Value>,
    fallback_summary: &str,
) -> serde_json::Value {
    if let Some(args) = raw_arguments {
        match tool_name {
            "file_edit" => {
                let path = args.get("path").cloned().unwrap_or(serde_json::Value::Null);
                let old_text = args
                    .get("old_string")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let new_text = args
                    .get("new_string")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                return json!([{
                    "type": "diff",
                    "path": path,
                    "oldText": old_text,
                    "newText": new_text,
                }]);
            }
            "file_write" => {
                let path = args.get("path").cloned().unwrap_or(serde_json::Value::Null);
                let new_text = args
                    .get("content")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                return json!([{
                    "type": "diff",
                    "path": path,
                    "newText": new_text,
                }]);
            }
            _ => {}
        }
    }
    json!([{
        "type": "content",
        "content": {
            "type": "text",
            "text": fallback_summary,
        }
    }])
}

#[async_trait]
impl Channel for AcpChannel {
    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing indicator in the ACP session protocol; updates stream via session/update.
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing indicator in the ACP session protocol; updates stream via session/update.
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // Surface the message inline in the ACP client as a normal agent
        // message chunk. This is intentionally one-way — there's no inbound
        // counterpart for free-form replies (see `listen`).
        self.rpc
            .notify(
                "session/update",
                json!({
                    "sessionId": self.session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {
                            "type": "text",
                            "text": message.content,
                        }
                    }
                }),
            )
            .await;
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        anyhow::bail!(
            "AcpChannel.listen is not supported (free-form ask_user awaits ACP elicitation Phase 2)"
        )
    }

    fn supports_free_form_ask(&self) -> bool {
        false
    }

    async fn add_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _emoji: &str,
    ) -> anyhow::Result<()> {
        // ACP renders agent output as message chunks — there's no per-message
        // reaction primitive in the protocol, so silently no-oping (the trait
        // default) would falsely report success to the agent. Surface as Err
        // so the `reaction` tool's caller sees the truth.
        anyhow::bail!("AcpChannel does not support reactions")
    }

    async fn remove_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _emoji: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("AcpChannel does not support reactions")
    }

    async fn request_choice(
        &self,
        question: &str,
        choices: &[String],
        timeout: Duration,
    ) -> anyhow::Result<Option<String>> {
        if choices.is_empty() {
            // Caller should already gate on this via supports_free_form_ask,
            // but be defensive — both downstream paths require at least one
            // option to present.
            anyhow::bail!("AcpChannel.request_choice requires at least one choice")
        }
        if self.client_caps.form {
            self.request_choice_via_elicitation(question, choices, timeout)
                .await
        } else {
            self.request_choice_via_permission(question, choices, timeout)
                .await
        }
    }

    async fn request_multi_choice(
        &self,
        question: &str,
        choices: &[String],
        min_items: usize,
        max_items: usize,
        timeout: Duration,
    ) -> anyhow::Result<Option<Vec<String>>> {
        if choices.is_empty() {
            anyhow::bail!("AcpChannel.request_multi_choice requires at least one choice")
        }
        if !self.client_caps.form {
            // No legacy fallback for multi-select — session/request_permission
            // is single-select-only. Signal Ok(None) so the caller (poll tool)
            // takes its own non-ACP fallback path.
            return Ok(None);
        }

        let req = ElicitationRequest {
            session_id: self.session_id.clone(),
            mode: ElicitationMode::Form,
            message: question.to_string(),
            requested_schema: multi_select_schema(choices, min_items, max_items),
        };
        let params = serde_json::to_value(&req)?;
        let call = self.rpc.request("elicitation/create", params);
        let response_value = match tokio::time::timeout(timeout, call).await {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => anyhow::bail!(
                "ACP elicitation/create (multi) failed: {} ({})",
                e.message,
                e.code
            ),
            Err(_) => {
                anyhow::bail!("ACP elicitation/create (multi) timed out after {timeout:?}")
            }
        };

        let parsed: ElicitationResponse = serde_json::from_value(response_value)
            .map_err(|e| anyhow::Error::msg(format!("malformed elicitation response: {e}")))?;
        match parsed {
            ElicitationResponse::Accept { content } => {
                let texts =
                    zeroclaw_api::elicitation::decode_multi_select_accept(&content, choices)?;
                Ok(Some(texts))
            }
            ElicitationResponse::Decline | ElicitationResponse::Cancel => Ok(None),
        }
    }

    async fn request_approval(
        &self,
        _recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let is_edit_tool = matches!(request.tool_name.as_str(), "file_edit" | "file_write");
        let mut options = vec![
            json!({
                "optionId": "allow-once",
                "name": "Allow once",
                "kind": "allow_once",
            }),
            json!({
                "optionId": "allow-always",
                "name": "Always allow",
                "kind": "allow_always",
            }),
        ];
        if is_edit_tool {
            options.push(json!({
                "optionId": "reject-with-edit",
                "name": "Reject with edit",
                "kind": "reject_with_edit",
            }));
        }
        options.push(json!({
            "optionId": "reject-once",
            "name": "Reject",
            "kind": "reject_once",
        }));

        let tool_call_id = format!("approval-{}", uuid::Uuid::new_v4());
        let title = format!("Approve {}?", request.tool_name);
        let kind = map_approval_kind(&request.tool_name);
        let raw_input = build_approval_raw_input(&request.tool_name, &request.raw_arguments);
        let content = build_approval_content(
            &request.tool_name,
            &request.raw_arguments,
            &request.arguments_summary,
        );

        // For edit tools, also surface the new_string (or content) directly so that
        // "reject-with-edit" can present exactly the proposed replacement for editing,
        // without the surrounding path/old_string fields and with newlines preserved.
        let mut tool_call = json!({
            "toolCallId": tool_call_id,
            "title": title,
            "kind": kind,
            "status": "pending",
            "rawInput": raw_input,
            "content": content,
        });
        if is_edit_tool
            && let Some(args) = &request.raw_arguments
            && let Some(new_text) = args.get("new_string").or_else(|| args.get("content"))
            && let Some(s) = new_text.as_str()
        {
            tool_call["proposedEdit"] = json!(s);
        }
        let params = json!({
            "sessionId": self.session_id,
            "options": options,
            "toolCall": tool_call,
        });

        let call = self.rpc.request("session/request_permission", params);
        let response = match tokio::time::timeout(self.approval_timeout, call).await {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => {
                anyhow::bail!("ACP request_permission failed: {} ({})", e.message, e.code)
            }
            Err(_) => anyhow::bail!(
                "ACP request_permission timed out after {:?}",
                self.approval_timeout
            ),
        };

        let outcome = response.get("outcome");
        let kind = outcome
            .and_then(|o| o.get("outcome"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        match kind {
            "selected" => {
                let option_id = outcome
                    .and_then(|o| o.get("optionId"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                match option_id {
                    "allow-once" => Ok(Some(ChannelApprovalResponse::Approve)),
                    "allow-always" => Ok(Some(ChannelApprovalResponse::AlwaysApprove)),
                    "reject-once" | "reject-always" => Ok(Some(ChannelApprovalResponse::Deny)),
                    "reject-with-edit" => {
                        let replacement = outcome
                            .and_then(|o| o.get("replacementContent"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string();
                        Ok(Some(ChannelApprovalResponse::DenyWithEdit { replacement }))
                    }
                    other => anyhow::bail!("ACP returned unknown permission optionId: {other}"),
                }
            }
            "cancelled" => Ok(Some(ChannelApprovalResponse::Deny)),
            other => anyhow::bail!("ACP returned unexpected permission outcome: {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use zeroclaw_api::elicitation::single_select_schema_with_property_name;
    use zeroclaw_api::jsonrpc::JSONRPC_VERSION;

    fn make_rpc() -> (Arc<RpcOutbound>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>(16);
        (Arc::new(RpcOutbound::new(tx)), rx)
    }

    #[tokio::test]
    async fn name_returns_provided_name() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            rpc,
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        assert_eq!(ch.name(), "acp");
    }

    #[tokio::test]
    async fn supports_free_form_ask_is_false() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            rpc,
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        assert!(!ch.supports_free_form_ask());
    }

    #[tokio::test]
    async fn send_emits_agent_message_chunk_notification() {
        let (rpc, mut rx) = make_rpc();
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            rpc,
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );

        ch.send(&SendMessage::new("hello", "")).await.unwrap();

        let line = rx.recv().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["jsonrpc"], JSONRPC_VERSION);
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["sessionId"], "sess-1");
        assert_eq!(
            v["params"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
        assert_eq!(v["params"]["update"]["content"]["text"], "hello");
        // Notifications must not have an id.
        assert!(v.get("id").is_none());
    }

    #[tokio::test]
    async fn add_reaction_returns_error() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            rpc,
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let res = ch.add_reaction("chan", "msg", "👍").await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn remove_reaction_returns_error() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            rpc,
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let res = ch.remove_reaction("chan", "msg", "👍").await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn listen_returns_error() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            rpc,
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let (tx, _) = mpsc::channel(1);
        let res = ch.listen(tx).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn request_choice_emits_request_permission_and_resolves_selection() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );

        let choices = vec![
            "Option A".to_string(),
            "Option B".to_string(),
            "Cancel".to_string(),
        ];

        // Spawn the request; capture the outbound id, then dispatch a
        // matching "selected" response so the await resolves.
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Confirm?", &choices, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "session/request_permission");
        assert_eq!(req["params"]["options"].as_array().unwrap().len(), 3);
        assert_eq!(req["params"]["options"][0]["name"], "Option A");
        assert_eq!(req["params"]["options"][2]["kind"], "reject_once");
        let id = req["id"].as_str().unwrap().to_string();

        // Simulate the ACP client picking "Option B" (choice-1).
        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"outcome": {"outcome": "selected", "optionId": "choice-1"}})),
            None,
        );

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, Some("Option B".to_string()));
    }

    #[tokio::test]
    async fn request_choice_handles_cancel_outcome() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );

        let choices = vec!["Yes".to_string(), "No".to_string()];

        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Confirm?", &choices, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"outcome": {"outcome": "cancelled"}})),
            None,
        );

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn request_choice_times_out_when_no_response() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            rpc,
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let choices = vec!["Yes".to_string(), "No".to_string()];
        let res = ch
            .request_choice("Confirm?", &choices, Duration::from_millis(50))
            .await;
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        assert!(msg.contains("timed out"), "unexpected error: {msg}");
    }

    // -- Elicitation-path tests (Task 5) ---------------------------------

    fn form_caps() -> ElicitationCapabilities {
        ElicitationCapabilities {
            form: true,
            url: false,
        }
    }

    #[tokio::test]
    async fn request_choice_uses_elicitation_when_capability_present() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );

        let choices = vec!["Alpha".to_string(), "Beta".to_string()];

        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Pick one", &choices, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();

        assert_eq!(req["method"], "elicitation/create");
        assert_eq!(req["params"]["sessionId"], "sess-1");
        assert_eq!(req["params"]["mode"], "form");
        assert_eq!(req["params"]["message"], "Pick one");

        // Schema is a single-select form with const-indexed options.
        let schema = &req["params"]["requestedSchema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "choice");
        let one_of = schema["properties"]["choice"]["oneOf"]
            .as_array()
            .expect("oneOf array");
        assert_eq!(one_of.len(), 2);
        assert_eq!(one_of[0]["const"], "choice-0");
        assert_eq!(one_of[0]["title"], "Alpha");
        assert_eq!(one_of[1]["const"], "choice-1");
        assert_eq!(one_of[1]["title"], "Beta");

        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"action": "accept", "content": {"choice": "choice-1"}})),
            None,
        );

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, Some("Beta".to_string()));
    }

    #[tokio::test]
    async fn request_choice_falls_back_to_permission_when_capability_absent() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );

        let choices = vec!["First".to_string(), "Second".to_string()];

        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Confirm?", &choices, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();

        // Backward-compatibility contract: legacy clients must still see
        // session/request_permission, NOT elicitation/create.
        assert_eq!(req["method"], "session/request_permission");
        assert_ne!(req["method"], "elicitation/create");
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"outcome": {"outcome": "selected", "optionId": "choice-0"}})),
            None,
        );

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, Some("First".to_string()));
    }

    #[tokio::test]
    async fn request_choice_decline_returns_none() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );

        let choices = vec!["A".to_string(), "B".to_string()];
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Q?", &choices, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "elicitation/create");
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(&id, Some(json!({"action": "decline"})), None);

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn request_choice_cancel_returns_none() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );

        let choices = vec!["A".to_string(), "B".to_string()];
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Q?", &choices, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "elicitation/create");
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(&id, Some(json!({"action": "cancel"})), None);

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn request_choice_accept_with_unknown_const_is_error() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );

        let choices = vec!["A".to_string(), "B".to_string()];
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Q?", &choices, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"action": "accept", "content": {"choice": "choice-99"}})),
            None,
        );

        let res = task.await.unwrap();
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        let lower = msg.to_lowercase();
        assert!(
            lower.contains("unknown") || msg.contains("choice-99"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn request_choice_empty_choices_is_error() {
        // Both paths must enforce the no-empty-choices invariant.
        let (rpc, _rx) = make_rpc();
        let ch_form = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );
        let res = ch_form
            .request_choice("Q?", &[], Duration::from_secs(1))
            .await;
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        assert!(
            msg.contains("at least one choice"),
            "unexpected error: {msg}"
        );

        let ch_legacy = AcpChannel::new(
            "acp",
            "sess-1",
            rpc,
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let res = ch_legacy
            .request_choice("Q?", &[], Duration::from_secs(1))
            .await;
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        assert!(
            msg.contains("at least one choice"),
            "unexpected error: {msg}"
        );
    }

    // -- End elicitation-path tests --------------------------------------

    #[tokio::test]
    async fn request_approval_emits_request_permission_and_resolves_approve() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let request = ChannelApprovalRequest {
            tool_name: "git".to_string(),
            arguments_summary: "git status --short".to_string(),
            raw_arguments: None,
        };

        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "session/request_permission");
        assert_eq!(req["params"]["sessionId"], "sess-1");
        assert_eq!(req["params"]["options"].as_array().unwrap().len(), 3);
        assert_eq!(req["params"]["options"][0]["optionId"], "allow-once");
        assert_eq!(req["params"]["options"][1]["kind"], "allow_always");
        assert_eq!(req["params"]["toolCall"]["title"], "Approve git?");
        assert_eq!(req["params"]["toolCall"]["status"], "pending");
        assert_eq!(
            req["params"]["toolCall"]["content"][0]["content"]["text"],
            "git status --short"
        );
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"outcome": {"outcome": "selected", "optionId": "allow-once"}})),
            None,
        );

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, Some(ChannelApprovalResponse::Approve));
    }

    #[tokio::test]
    async fn request_approval_maps_always_and_cancel() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let request = ChannelApprovalRequest {
            tool_name: "git".to_string(),
            arguments_summary: "git commit".to_string(),
            raw_arguments: None,
        };

        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });
        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"outcome": {"outcome": "selected", "optionId": "allow-always"}})),
            None,
        );
        assert_eq!(
            task.await.unwrap().unwrap(),
            Some(ChannelApprovalResponse::AlwaysApprove)
        );

        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let request = ChannelApprovalRequest {
            tool_name: "git".to_string(),
            arguments_summary: "git push".to_string(),
            raw_arguments: None,
        };
        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });
        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();
        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"outcome": {"outcome": "cancelled"}})),
            None,
        );
        assert_eq!(
            task.await.unwrap().unwrap(),
            Some(ChannelApprovalResponse::Deny)
        );
    }

    #[tokio::test]
    async fn file_edit_approval_emits_diff_content_item() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let request = ChannelApprovalRequest {
            tool_name: "file_edit".to_string(),
            arguments_summary: "old_string: let x = 1;, new_string: let x = 2;".to_string(),
            raw_arguments: Some(serde_json::json!({
                "path": "src/foo.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;"
            })),
        };

        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });
        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();

        // kind must be "edit" for diff rendering
        assert_eq!(req["params"]["toolCall"]["kind"], "edit");

        // content must carry a Diff item, not a plain text fallback
        let content = &req["params"]["toolCall"]["content"];
        assert_eq!(
            content[0]["type"], "diff",
            "file_edit approval must emit a diff content item"
        );
        assert_eq!(content[0]["path"], "src/foo.rs");
        assert_eq!(content[0]["oldText"], "let x = 1;");
        assert_eq!(content[0]["newText"], "let x = 2;");

        let id = req["id"].as_str().unwrap().to_string();
        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"outcome": {"outcome": "selected", "optionId": "allow-once"}})),
            None,
        );
        assert_eq!(
            task.await.unwrap().unwrap(),
            Some(ChannelApprovalResponse::Approve)
        );
    }

    #[test]
    fn build_approval_content_returns_diff_for_file_edit() {
        let args = serde_json::json!({
            "path": "README.md",
            "old_string": "# Old Title",
            "new_string": "# New Title"
        });
        let content = build_approval_content("file_edit", &Some(args), "fallback");
        let arr = content.as_array().expect("content must be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "diff");
        assert_eq!(arr[0]["path"], "README.md");
        assert_eq!(arr[0]["oldText"], "# Old Title");
        assert_eq!(arr[0]["newText"], "# New Title");
    }

    #[test]
    fn build_approval_content_falls_back_to_text_for_other_tools() {
        let content = build_approval_content("shell", &None, "ls -la");
        let arr = content.as_array().expect("content must be an array");
        assert_eq!(arr[0]["type"], "content");
        assert_eq!(arr[0]["content"]["type"], "text");
        assert_eq!(arr[0]["content"]["text"], "ls -la");
    }

    #[tokio::test]
    async fn request_approval_maps_reject_with_edit_to_deny_with_edit() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let request = ChannelApprovalRequest {
            tool_name: "file_edit".to_string(),
            arguments_summary: "edit foo.rs".to_string(),
            raw_arguments: Some(serde_json::json!({
                "path": "foo.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;"
            })),
        };

        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(serde_json::json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": "reject-with-edit",
                    "replacementContent": "let x = 99;"
                }
            })),
            None,
        );

        let result = task.await.unwrap().unwrap();
        match result {
            Some(ChannelApprovalResponse::DenyWithEdit { replacement }) => {
                assert_eq!(replacement, "let x = 99;");
            }
            other => panic!("expected DenyWithEdit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn file_edit_approval_includes_reject_with_edit_option() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let request = ChannelApprovalRequest {
            tool_name: "file_edit".to_string(),
            arguments_summary: "edit foo.rs".to_string(),
            raw_arguments: Some(serde_json::json!({
                "path": "foo.rs",
                "old_string": "a",
                "new_string": "b"
            })),
        };

        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();

        let options = req["params"]["options"].as_array().unwrap();
        let has_reject_edit = options.iter().any(|o| o["optionId"] == "reject-with-edit");
        assert!(
            has_reject_edit,
            "file_edit approval must offer reject-with-edit"
        );

        let id = req["id"].as_str().unwrap().to_string();
        rpc_for_resp.dispatch_response(
            &id,
            Some(serde_json::json!({"outcome": {"outcome": "cancelled"}})),
            None,
        );
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn reject_with_edit_missing_replacement_defaults_to_empty() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let request = ChannelApprovalRequest {
            tool_name: "file_edit".to_string(),
            arguments_summary: "edit foo.rs".to_string(),
            raw_arguments: None,
        };

        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();

        // Response has optionId but no replacementContent.
        rpc_for_resp.dispatch_response(
            &id,
            Some(serde_json::json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": "reject-with-edit"
                }
            })),
            None,
        );

        let result = task.await.unwrap().unwrap();
        // Absent replacementContent defaults to empty string — caller must guard.
        assert!(
            matches!(result, Some(ChannelApprovalResponse::DenyWithEdit { replacement }) if replacement.is_empty())
        );
    }

    #[tokio::test]
    async fn file_write_approval_includes_reject_with_edit_option() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );
        let request = ChannelApprovalRequest {
            tool_name: "file_write".to_string(),
            arguments_summary: "write bar.rs".to_string(),
            raw_arguments: None,
        };

        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();

        let options = req["params"]["options"].as_array().unwrap();
        let has_reject_edit = options.iter().any(|o| o["optionId"] == "reject-with-edit");
        assert!(
            has_reject_edit,
            "file_write approval must offer reject-with-edit"
        );

        let id = req["id"].as_str().unwrap().to_string();
        rpc_for_resp.dispatch_response(
            &id,
            Some(serde_json::json!({"outcome": {"outcome": "cancelled"}})),
            None,
        );
        task.await.unwrap().unwrap();
    }

    #[test]
    fn single_select_schema_has_object_shape() {
        let schema = single_select_schema(&[
            "Conservative".to_string(),
            "Balanced".to_string(),
            "Aggressive".to_string(),
        ]);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["choice"]));
        let choice = &schema["properties"]["choice"];
        assert_eq!(choice["type"], "string");
        let one_of = choice["oneOf"].as_array().expect("oneOf array");
        assert_eq!(one_of.len(), 3);
        assert_eq!(one_of[0]["const"], "choice-0");
        assert_eq!(one_of[0]["title"], "Conservative");
        assert_eq!(one_of[2]["const"], "choice-2");
        assert_eq!(one_of[2]["title"], "Aggressive");
    }

    #[test]
    fn single_select_schema_preserves_choice_text_via_index() {
        // Empty / duplicate display strings must not collide because the
        // wire-format `const` is index-based.
        let schema = single_select_schema(&["".to_string(), "".to_string()]);
        let one_of = schema["properties"]["choice"]["oneOf"].as_array().unwrap();
        assert_eq!(one_of[0]["const"], "choice-0");
        assert_eq!(one_of[1]["const"], "choice-1");
    }

    #[test]
    #[should_panic(expected = "sensitive")]
    fn single_select_schema_rejects_sensitive_property_names_in_debug() {
        // The trip-wire is debug-only — production builds skip the assert.
        // This test exists so a future caller renaming "choice" to "password"
        // (or building a schema with such a property) fails loudly in CI.
        let _ = single_select_schema_with_property_name("password", &["x".to_string()]);
    }

    #[test]
    fn multi_select_schema_has_array_anyof_items() {
        let schema = multi_select_schema(
            &["Red".to_string(), "Green".to_string(), "Blue".to_string()],
            1,
            2,
        );
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["choices"]));
        let choices = &schema["properties"]["choices"];
        assert_eq!(choices["type"], "array");
        assert_eq!(choices["minItems"], 1);
        assert_eq!(choices["maxItems"], 2);
        let any_of = choices["items"]["anyOf"].as_array().expect("anyOf array");
        assert_eq!(any_of.len(), 3);
        assert_eq!(any_of[0]["const"], "choice-0");
        assert_eq!(any_of[0]["title"], "Red");
        assert_eq!(any_of[1]["const"], "choice-1");
        assert_eq!(any_of[1]["title"], "Green");
        assert_eq!(any_of[2]["const"], "choice-2");
        assert_eq!(any_of[2]["title"], "Blue");
    }

    #[tokio::test]
    async fn request_multi_choice_uses_elicitation_when_capability_present() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );

        let choices = vec!["Red".to_string(), "Green".to_string(), "Blue".to_string()];

        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_multi_choice("Pick colors", &choices, 1, 2, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();

        assert_eq!(req["method"], "elicitation/create");
        assert_eq!(req["params"]["sessionId"], "sess-1");
        assert_eq!(req["params"]["mode"], "form");
        assert_eq!(req["params"]["message"], "Pick colors");

        // Schema is a multi-select array with anyOf items.
        let schema = &req["params"]["requestedSchema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "choices");
        let arr = &schema["properties"]["choices"];
        assert_eq!(arr["type"], "array");
        assert_eq!(arr["minItems"], 1);
        assert_eq!(arr["maxItems"], 2);
        let any_of = arr["items"]["anyOf"].as_array().expect("anyOf array");
        assert_eq!(any_of.len(), 3);
        assert_eq!(any_of[0]["const"], "choice-0");
        assert_eq!(any_of[0]["title"], "Red");
        assert_eq!(any_of[2]["const"], "choice-2");
        assert_eq!(any_of[2]["title"], "Blue");

        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({
                "action": "accept",
                "content": {"choices": ["choice-0", "choice-2"]},
            })),
            None,
        );

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, Some(vec!["Red".to_string(), "Blue".to_string()]));
    }

    #[tokio::test]
    async fn request_multi_choice_returns_none_without_capability() {
        let (rpc, mut rx) = make_rpc();
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            ElicitationCapabilities::default(),
        );

        let choices = vec!["A".to_string(), "B".to_string()];

        // No legacy fallback for multi-select — must return Ok(None) directly
        // without emitting any RPC. So we can await inline.
        let result = ch
            .request_multi_choice("Q?", &choices, 1, 2, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(result, None);

        // No outbound RPC should have been sent.
        match rx.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            Ok(line) => panic!("unexpected outbound RPC line: {line}"),
            Err(e) => panic!("unexpected rx state: {e:?}"),
        }
    }

    #[tokio::test]
    async fn request_multi_choice_decline_returns_none() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );

        let choices = vec!["A".to_string(), "B".to_string()];
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_multi_choice("Q?", &choices, 1, 2, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "elicitation/create");
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(&id, Some(json!({"action": "decline"})), None);

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn request_multi_choice_cancel_returns_none() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );

        let choices = vec!["A".to_string(), "B".to_string()];
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_multi_choice("Q?", &choices, 1, 2, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "elicitation/create");
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(&id, Some(json!({"action": "cancel"})), None);

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn request_multi_choice_accept_with_unknown_const_is_error() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new(
            "acp",
            "sess-1",
            Arc::clone(&rpc),
            Duration::from_secs(30),
            form_caps(),
        );

        let choices = vec!["A".to_string(), "B".to_string()];
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_multi_choice("Q?", &choices, 1, 2, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response(
            &id,
            Some(json!({"action": "accept", "content": {"choices": ["choice-99"]}})),
            None,
        );

        let res = task.await.unwrap();
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        let lower = msg.to_lowercase();
        assert!(
            lower.contains("unknown") || msg.contains("choice-99"),
            "unexpected error: {msg}"
        );
    }
}
