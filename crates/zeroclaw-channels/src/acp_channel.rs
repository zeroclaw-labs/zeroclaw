//! ACP (Agent Client Protocol) back-channel.
//!
//! Bridges ZeroClaw's [`Channel`] abstraction onto an active ACP session so
//! tools like `ask_user`, `escalate_to_human`, and `reaction` can talk back
//! to the IDE/CLI client (Toad, Zed, etc.) instead of returning
//! "no channels available".
//!
//! ## What this channel does
//!
//! - `send` emits an `agent_message_chunk` `session/update` notification —
//!   the ACP client renders it inline in the conversation.
//! - `request_choice` issues a `session/request_permission` JSON-RPC request
//!   with the question's choices mapped to permission options. Returns the
//!   selected option's text (or `Err` on cancellation/timeout).
//! - `listen` is **not implemented**. Free-form ACP "ask the user" has no
//!   first-class method until the [elicitation RFD][rfd] lands; until then
//!   `ask_user` callers under ACP must supply structured `choices`.
//!
//! [rfd]: https://github.com/zed-industries/agent-client-protocol/blob/main/docs/rfds/elicitation.mdx

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
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
}

impl AcpChannel {
    /// Build an ACP channel bound to a specific ACP session id and the
    /// server's outbound JSON-RPC plumbing.
    ///
    /// `approval_timeout` caps how long `request_approval` and `request_choice`
    /// will wait for a client response. Pass `session_timeout_secs` from
    /// `AcpServerConfig` so the bound is consistent with the session lifetime.
    pub fn new(
        name: impl Into<String>,
        session_id: impl Into<String>,
        rpc: Arc<RpcOutbound>,
        approval_timeout: Duration,
    ) -> Self {
        Self {
            name: name.into(),
            session_id: session_id.into(),
            rpc,
            approval_timeout,
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

/// Build the `rawInput` object for a `session/request_permission` approval.
///
/// This carries the raw tool arguments so clients that inspect `rawInput`
/// directly can read the original field names. Structured diff rendering is
/// driven by the `content` array (see `build_approval_content`).
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

/// Build the `content` array for a `session/request_permission` approval.
///
/// Zed and Toad render tool call content items from the `content` array, not
/// from `rawInput`. For file-editing tools, emit an ACP `Diff` content item
/// (`{ "type": "diff", "path": ..., "oldText": ..., "newText": ... }`) so the
/// client renders a side-by-side diff editor instead of raw JSON field names.
/// Other tools fall back to a plain-text content block containing the
/// pre-computed `arguments_summary`.
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
        // ACP has no first-class "next free-form user message in this session"
        // method. The elicitation RFD is the future fix; until it lands,
        // `ask_user` under ACP must supply structured `choices`, which routes
        // through `request_choice` → `session/request_permission` instead.
        // RFD: https://github.com/zed-industries/agent-client-protocol/blob/main/docs/rfds/elicitation.mdx
        anyhow::bail!(
            "AcpChannel.listen is not supported (free-form ask_user awaits ACP elicitation RFD)"
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
            // but be defensive — no choices means no permission options to
            // present, and `session/request_permission` requires at least one.
            anyhow::bail!("AcpChannel.request_choice requires at least one choice")
        }

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

    async fn request_approval(
        &self,
        _recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let options = [
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
            json!({
                "optionId": "reject-once",
                "name": "Reject",
                "kind": "reject_once",
            }),
        ];

        let tool_call_id = format!("approval-{}", uuid::Uuid::new_v4());
        let title = format!("Approve {}?", request.tool_name);
        let kind = map_approval_kind(&request.tool_name);
        let raw_input = build_approval_raw_input(&request.tool_name, &request.raw_arguments);
        let content = build_approval_content(
            &request.tool_name,
            &request.raw_arguments,
            &request.arguments_summary,
        );
        let params = json!({
            "sessionId": self.session_id,
            "options": options,
            "toolCall": {
                "toolCallId": tool_call_id,
                "title": title,
                "kind": kind,
                "status": "pending",
                "rawInput": raw_input,
                "content": content,
            }
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

    fn make_rpc() -> (Arc<RpcOutbound>, mpsc::Receiver<String>) {
        // Fabricate an RpcOutbound that writes into a test mpsc instead of
        // stdout. Uses RpcOutbound's public constructor surface via the
        // re-exported `for_testing` helper.
        let (tx, rx) = mpsc::channel::<String>(16);
        (Arc::new(RpcOutbound::for_testing(tx)), rx)
    }

    #[tokio::test]
    async fn name_returns_provided_name() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new("acp", "sess-1", rpc, Duration::from_secs(30));
        assert_eq!(ch.name(), "acp");
    }

    #[tokio::test]
    async fn supports_free_form_ask_is_false() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new("acp", "sess-1", rpc, Duration::from_secs(30));
        assert!(!ch.supports_free_form_ask());
    }

    #[tokio::test]
    async fn send_emits_agent_message_chunk_notification() {
        let (rpc, mut rx) = make_rpc();
        let ch = AcpChannel::new("acp", "sess-1", rpc, Duration::from_secs(30));

        ch.send(&SendMessage::new("hello", "")).await.unwrap();

        let line = rx.recv().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
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
        let ch = AcpChannel::new("acp", "sess-1", rpc, Duration::from_secs(30));
        let res = ch.add_reaction("chan", "msg", "👍").await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn remove_reaction_returns_error() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new("acp", "sess-1", rpc, Duration::from_secs(30));
        let res = ch.remove_reaction("chan", "msg", "👍").await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn listen_returns_error() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new("acp", "sess-1", rpc, Duration::from_secs(30));
        let (tx, _) = mpsc::channel(1);
        let res = ch.listen(tx).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn request_choice_rejects_empty_choices() {
        let (rpc, _rx) = make_rpc();
        let ch = AcpChannel::new("acp", "sess-1", rpc, Duration::from_secs(30));
        let res = ch
            .request_choice("Pick one", &[], Duration::from_secs(1))
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn request_choice_emits_request_permission_and_resolves_selection() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new("acp", "sess-1", Arc::clone(&rpc), Duration::from_secs(30));

        let choices = vec![
            "Option A".to_string(),
            "Option B".to_string(),
            "Cancel".to_string(),
        ];

        // Spawn the request; capture the outbound id, then dispatch a
        // matching "selected" response so the await resolves.
        let task = tokio::spawn(async move {
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
        rpc_for_resp.dispatch_response_for_test(
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
        let ch = AcpChannel::new("acp", "sess-1", Arc::clone(&rpc), Duration::from_secs(30));

        let choices = vec!["Yes".to_string(), "No".to_string()];

        let task = tokio::spawn(async move {
            ch.request_choice("Confirm?", &choices, Duration::from_secs(5))
                .await
        });

        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response_for_test(
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
        let ch = AcpChannel::new("acp", "sess-1", rpc, Duration::from_secs(30));
        let choices = vec!["Yes".to_string(), "No".to_string()];
        let res = ch
            .request_choice("Confirm?", &choices, Duration::from_millis(50))
            .await;
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        assert!(msg.contains("timed out"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn request_approval_emits_request_permission_and_resolves_approve() {
        let (rpc, mut rx) = make_rpc();
        let rpc_for_resp = Arc::clone(&rpc);
        let ch = AcpChannel::new("acp", "sess-1", Arc::clone(&rpc), Duration::from_secs(30));
        let request = ChannelApprovalRequest {
            tool_name: "git".to_string(),
            arguments_summary: "git status --short".to_string(),
            raw_arguments: None,
        };

        let task = tokio::spawn(async move { ch.request_approval("", &request).await });

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

        rpc_for_resp.dispatch_response_for_test(
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
        let ch = AcpChannel::new("acp", "sess-1", Arc::clone(&rpc), Duration::from_secs(30));
        let request = ChannelApprovalRequest {
            tool_name: "git".to_string(),
            arguments_summary: "git commit".to_string(),
            raw_arguments: None,
        };

        let task = tokio::spawn(async move { ch.request_approval("", &request).await });
        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();

        rpc_for_resp.dispatch_response_for_test(
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
        let ch = AcpChannel::new("acp", "sess-1", Arc::clone(&rpc), Duration::from_secs(30));
        let request = ChannelApprovalRequest {
            tool_name: "git".to_string(),
            arguments_summary: "git push".to_string(),
            raw_arguments: None,
        };
        let task = tokio::spawn(async move { ch.request_approval("", &request).await });
        let line = rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = req["id"].as_str().unwrap().to_string();
        rpc_for_resp.dispatch_response_for_test(
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
        let ch = AcpChannel::new("acp", "sess-1", Arc::clone(&rpc), Duration::from_secs(30));
        let request = ChannelApprovalRequest {
            tool_name: "file_edit".to_string(),
            arguments_summary: "old_string: let x = 1;, new_string: let x = 2;".to_string(),
            raw_arguments: Some(serde_json::json!({
                "path": "src/foo.rs",
                "old_string": "let x = 1;",
                "new_string": "let x = 2;"
            })),
        };

        let task = tokio::spawn(async move { ch.request_approval("", &request).await });
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
        rpc_for_resp.dispatch_response_for_test(
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
}
