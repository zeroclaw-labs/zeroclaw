//! RpcApprovalChannel — bridges Channel::request_approval(),
//! Channel::request_choice(), and Channel::request_multi_choice() to the
//! daemon Unix socket RPC stream so Zerocode's Code tab can both gate
//! tool calls (the original purpose) and surface ACP-style elicitation

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::elicitation::{
    ElicitationCapabilities, ElicitationMode, ElicitationRequest, ElicitationResponse,
    decode_multi_select_accept, decode_single_select_accept, multi_select_schema,
    single_select_schema,
};
use zeroclaw_api::jsonrpc::RpcOutbound;

use super::context::ApprovalPendingMap;

const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct RpcApprovalChannel {
    name: String,
    session_id: String,
    rpc: Arc<RpcOutbound>,
    pending: Arc<ApprovalPendingMap>,
    approval_timeout: Duration,
    client_caps: ElicitationCapabilities,
    /// The daemon's subscription hub. While the session's turn delivers
    /// through its ring (session turn lifetime), approval prompts go there
    /// too, so any attached viewer can answer them.
    subscriptions: Option<Arc<crate::rpc::subscription::SubscriptionHub>>,
}

impl RpcApprovalChannel {
    pub fn new(
        name: impl Into<String>,
        session_id: impl Into<String>,
        rpc: Arc<RpcOutbound>,
        pending: Arc<ApprovalPendingMap>,
        client_caps: ElicitationCapabilities,
    ) -> Self {
        Self {
            name: name.into(),
            session_id: session_id.into(),
            rpc,
            pending,
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            client_caps,
            subscriptions: None,
        }
    }

    /// Route approval prompts through the session's ring while its turn is
    /// session-owned.
    #[must_use]
    pub fn with_subscriptions(
        mut self,
        subscriptions: Arc<crate::rpc::subscription::SubscriptionHub>,
    ) -> Self {
        self.subscriptions = Some(subscriptions);
        self
    }
}

/// Only an answered prompt is an operator decision. The other outcomes deny
/// because the client went away or never answered, and say so.
fn attributed_outcome(
    outcome: Result<
        Result<ChannelApprovalResponse, tokio::sync::oneshot::error::RecvError>,
        tokio::time::error::Elapsed,
    >,
) -> zeroclaw_api::channel::AttributedApprovalResponse {
    match outcome {
        Ok(Ok(response)) => zeroclaw_api::channel::AttributedApprovalResponse::operator(response),
        Ok(Err(_)) => unreachable_deny(),
        Err(_) => zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
            ChannelApprovalResponse::Deny,
            zeroclaw_api::channel::ApprovalSource::TimedOut,
        ),
    }
}

fn unreachable_deny() -> zeroclaw_api::channel::AttributedApprovalResponse {
    zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
        ChannelApprovalResponse::Deny,
        zeroclaw_api::channel::ApprovalSource::Unreachable,
    )
}

impl Attributable for RpcApprovalChannel {
    fn role(&self) -> Role {
        Role::Channel(ChannelKind::AcpChannel)
    }

    fn alias(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl Channel for RpcApprovalChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    /// `send` above is a deliberate no-op: the Code tab renders agent output
    /// from the RPC turn stream, not from generic channel sends. Declaring it
    /// here lets surfaces that must genuinely deliver (`poll`'s text fallback,
    /// `escalate_to_human`) refuse this channel instead of reporting success
    /// for a message the user never saw.
    fn supports_outbound_send(&self) -> bool {
        false
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        anyhow::bail!("RpcApprovalChannel.listen is not supported")
    }

    /// Free-form text elicitation is Phase 2 of the elicitation rollout —
    /// the same answer as `AcpChannel`. Until that lands, tools like
    /// `ask_user` (with no choices) and `escalate_to_human` (with
    /// `wait_for_response`) fail fast on the Code tab.
    fn supports_free_form_ask(&self) -> bool {
        false
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        self.request_approval_with_timeout(recipient, request, self.approval_timeout)
            .await
    }

    async fn request_approval_attributed(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        self.request_approval_attributed_with_timeout(recipient, request, self.approval_timeout)
            .await
    }

    async fn request_choice(
        &self,
        question: &str,
        choices: &[String],
        timeout: Duration,
    ) -> anyhow::Result<Option<String>> {
        if choices.is_empty() {
            // Defensive — callers should gate on `supports_free_form_ask`,
            // but a structured-choice request with zero options is always
            // a bug, not an interactive prompt we can render.
            anyhow::bail!("RpcApprovalChannel.request_choice requires at least one choice")
        }
        if !self.client_caps.form {
            return Ok(None);
        }
        self.request_choice_via_elicitation(question, choices, timeout)
            .await
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
            anyhow::bail!("RpcApprovalChannel.request_multi_choice requires at least one choice")
        }
        if !self.client_caps.form {
            return Ok(None);
        }
        self.request_multi_choice_via_elicitation(question, choices, min_items, max_items, timeout)
            .await
    }
}

impl RpcApprovalChannel {
    /// Kept as-is for callers that do not need provenance; delegates to
    /// [`Self::request_approval_attributed_with_timeout`].
    pub async fn request_approval_with_timeout(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
        timeout: Duration,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        Ok(self
            .request_approval_attributed_with_timeout(recipient, request, timeout)
            .await?
            .map(|attributed| attributed.response))
    }

    pub async fn request_approval_attributed_with_timeout(
        &self,
        _recipient: &str,
        request: &ChannelApprovalRequest,
        timeout: Duration,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<ChannelApprovalResponse>();
        // Bind the approval to this channel's session so session/approve is
        // authorized against the session's owner.
        let mut pending_request =
            self.pending
                .register(request_id.clone(), self.session_id.clone(), tx);

        let frame = json!({
            "type": "approval_request",
            "session_id": self.session_id,
            "request_id": request_id,
            "tool_name": request.tool_name,
            "arguments_summary": request.arguments_summary,
            "timeout_secs": timeout.as_secs(),
        });
        let ring = self.subscriptions.as_ref().and_then(|hub| {
            hub.routed_source(&self.session_id)
                .map(|source| (hub, source))
        });
        let outcome = match ring {
            None => {
                self.rpc.notify("session/update", frame).await;
                tokio::time::timeout(timeout, rx).await
            }
            // A session-owned turn: any attached viewer may answer. With
            // nobody attached, or once the last viewer detaches, the prompt
            // fails closed as unreachable instead of parking for the whole
            // timeout.
            Some((hub, _)) if hub.viewer_count(&self.session_id) == 0 => {
                return Ok(Some(unreachable_deny()));
            }
            Some((hub, source)) => {
                hub.publish(source, frame);
                tokio::select! {
                    outcome = tokio::time::timeout(timeout, rx) => outcome,
                    () = hub.viewers_gone(&self.session_id) => {
                        return Ok(Some(unreachable_deny()));
                    }
                }
            }
        };
        if matches!(outcome, Ok(Ok(_))) {
            pending_request.disarm();
        }
        Ok(Some(attributed_outcome(outcome)))
    }

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
                anyhow::bail!("RPC elicitation/create failed: {} ({})", e.message, e.code)
            }
            Err(_) => anyhow::bail!("RPC elicitation/create timed out after {timeout:?}"),
        };
        let parsed: ElicitationResponse = serde_json::from_value(response_value)
            .map_err(|e| anyhow::Error::msg(format!("malformed elicitation response: {e}")))?;
        match parsed {
            ElicitationResponse::Accept { content } => {
                let text = decode_single_select_accept(&content, choices)?;
                Ok(Some(text))
            }
            ElicitationResponse::Decline | ElicitationResponse::Cancel => Ok(None),
        }
    }

    /// Form-mode elicitation multi-select path — same wire shape as
    /// `AcpChannel::request_multi_choice`.
    async fn request_multi_choice_via_elicitation(
        &self,
        question: &str,
        choices: &[String],
        min_items: usize,
        max_items: usize,
        timeout: Duration,
    ) -> anyhow::Result<Option<Vec<String>>> {
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
                "RPC elicitation/create (multi) failed: {} ({})",
                e.message,
                e.code
            ),
            Err(_) => {
                anyhow::bail!("RPC elicitation/create (multi) timed out after {timeout:?}")
            }
        };
        let parsed: ElicitationResponse = serde_json::from_value(response_value)
            .map_err(|e| anyhow::Error::msg(format!("malformed elicitation response: {e}")))?;
        match parsed {
            ElicitationResponse::Accept { content } => {
                let texts = decode_multi_select_accept(&content, choices)?;
                Ok(Some(texts))
            }
            ElicitationResponse::Decline | ElicitationResponse::Cancel => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use zeroclaw_api::channel::{ChannelApprovalRequest, ChannelApprovalResponse};
    use zeroclaw_api::jsonrpc::RpcOutbound;

    fn make_rpc() -> (Arc<RpcOutbound>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>(16);
        (Arc::new(RpcOutbound::new(tx)), rx)
    }

    fn make_pending() -> Arc<crate::rpc::context::ApprovalPendingMap> {
        Arc::new(crate::rpc::context::ApprovalPendingMap::default())
    }

    /// Default test fixture — channel without elicitation capability.
    /// Mirrors a TUI that hasn't advertised `clientCapabilities.elicitation.form`.
    fn make_channel_no_caps(
        rpc: Arc<RpcOutbound>,
        pending: Arc<crate::rpc::context::ApprovalPendingMap>,
    ) -> RpcApprovalChannel {
        RpcApprovalChannel::new(
            "rpc",
            "sess-1",
            rpc,
            pending,
            ElicitationCapabilities::default(),
        )
    }

    /// Test fixture — channel with `elicitation.form` advertised.
    fn make_channel_form_caps(
        rpc: Arc<RpcOutbound>,
        pending: Arc<crate::rpc::context::ApprovalPendingMap>,
    ) -> RpcApprovalChannel {
        RpcApprovalChannel::new(
            "rpc",
            "sess-1",
            rpc,
            pending,
            ElicitationCapabilities {
                form: true,
                url: false,
            },
        )
    }

    #[tokio::test]
    async fn sends_approval_request_notification_and_awaits_response() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_no_caps(Arc::clone(&rpc), Arc::clone(&pending));

        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls /tmp".to_string(),
            raw_arguments: None,
            position: None,
        };

        let pending_for_resolve = Arc::clone(&pending);
        let task = zeroclaw_spawn::spawn!(async move { ch.request_approval("", &request).await });

        let line = write_rx.recv().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["type"], "approval_request");
        assert_eq!(v["params"]["session_id"], "sess-1");
        assert_eq!(v["params"]["tool_name"], "shell");

        let request_id = v["params"]["request_id"].as_str().unwrap().to_string();
        pending_for_resolve.resolve(&request_id, ChannelApprovalResponse::Approve);

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, Some(ChannelApprovalResponse::Approve));
    }

    #[tokio::test]
    async fn times_out_and_auto_denies() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_no_caps(Arc::clone(&rpc), Arc::clone(&pending));
        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "rm -rf /".to_string(),
            raw_arguments: None,
            position: None,
        };
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_approval_with_timeout("", &request, std::time::Duration::from_millis(50))
                .await
        });

        let line = write_rx.recv().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let request_id = v["params"]["request_id"].as_str().unwrap().to_string();
        assert!(pending.contains(&request_id));

        let result = task.await.unwrap().unwrap();
        assert_eq!(result, Some(ChannelApprovalResponse::Deny));
        assert!(
            !pending.contains(&request_id),
            "timed-out approval request must be removed from the pending map"
        );
        assert!(
            !pending.resolve(&request_id, ChannelApprovalResponse::Approve),
            "late approval after timeout must be a no-op"
        );
    }

    #[tokio::test]
    async fn dropped_request_future_removes_pending_request() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_no_caps(Arc::clone(&rpc), Arc::clone(&pending));
        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "sleep 60".to_string(),
            raw_arguments: None,
            position: None,
        };
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_approval_with_timeout("", &request, std::time::Duration::from_secs(60))
                .await
        });

        let line = write_rx.recv().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let request_id = v["params"]["request_id"].as_str().unwrap().to_string();
        assert!(pending.contains(&request_id));

        task.abort();
        let _ = task.await;
        assert!(
            !pending.contains(&request_id),
            "dropping the approval future must remove the pending request"
        );
    }

    // ── Elicitation (request_choice / request_multi_choice) ────────

    #[tokio::test]
    async fn request_choice_without_capability_returns_none() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_no_caps(Arc::clone(&rpc), Arc::clone(&pending));
        let result = ch
            .request_choice(
                "Pick one",
                &["A".to_string(), "B".to_string()],
                Duration::from_millis(50),
            )
            .await
            .unwrap();
        assert_eq!(result, None);
        // No frame on the wire — verify by trying a non-blocking recv.
        assert!(write_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn request_choice_with_capability_sends_elicitation_request() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_form_caps(Arc::clone(&rpc), Arc::clone(&pending));

        let rpc_for_response = Arc::clone(&rpc);
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice(
                "Pick one",
                &[
                    "Apple".to_string(),
                    "Banana".to_string(),
                    "Cherry".to_string(),
                ],
                Duration::from_secs(2),
            )
            .await
        });

        // Read the outbound request frame.
        let line = write_rx.recv().await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["jsonrpc"], "2.0");
        assert_eq!(frame["method"], "elicitation/create");
        let id = frame["id"]
            .as_str()
            .expect("request must carry a string id");
        let params = &frame["params"];
        assert_eq!(params["sessionId"], "sess-1");
        assert_eq!(params["mode"], "form");
        assert_eq!(params["message"], "Pick one");
        let one_of = &params["requestedSchema"]["properties"]["choice"]["oneOf"];
        assert_eq!(one_of[0]["const"], "choice-0");
        assert_eq!(one_of[1]["title"], "Banana");

        // Resolve the pending request with an `accept`.
        rpc_for_response.dispatch_response(
            id,
            Some(json!({ "action": "accept", "content": { "choice": "choice-1" } })),
            None,
        );

        let answer = task.await.unwrap().unwrap();
        assert_eq!(answer.as_deref(), Some("Banana"));
    }

    #[tokio::test]
    async fn request_choice_decline_returns_none() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_form_caps(Arc::clone(&rpc), Arc::clone(&pending));
        let rpc_for_response = Arc::clone(&rpc);
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Pick one", &["A".to_string()], Duration::from_secs(2))
                .await
        });
        let line = write_rx.recv().await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = frame["id"].as_str().unwrap();
        rpc_for_response.dispatch_response(id, Some(json!({ "action": "decline" })), None);
        let answer = task.await.unwrap().unwrap();
        assert_eq!(answer, None);
    }

    #[tokio::test]
    async fn request_choice_cancel_returns_none() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_form_caps(Arc::clone(&rpc), Arc::clone(&pending));
        let rpc_for_response = Arc::clone(&rpc);
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Pick one", &["A".to_string()], Duration::from_secs(2))
                .await
        });
        let line = write_rx.recv().await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = frame["id"].as_str().unwrap();
        rpc_for_response.dispatch_response(id, Some(json!({ "action": "cancel" })), None);
        let answer = task.await.unwrap().unwrap();
        assert_eq!(answer, None);
    }

    #[tokio::test]
    async fn request_choice_accept_with_unknown_const_errors() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_form_caps(Arc::clone(&rpc), Arc::clone(&pending));
        let rpc_for_response = Arc::clone(&rpc);
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_choice("Pick one", &["A".to_string()], Duration::from_secs(2))
                .await
        });
        let line = write_rx.recv().await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        let id = frame["id"].as_str().unwrap();
        rpc_for_response.dispatch_response(
            id,
            Some(json!({ "action": "accept", "content": { "choice": "choice-99" } })),
            None,
        );
        let result = task.await.unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn request_multi_choice_with_capability_sends_array_schema() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_form_caps(Arc::clone(&rpc), Arc::clone(&pending));
        let rpc_for_response = Arc::clone(&rpc);
        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_multi_choice(
                "Pick colors",
                &["Red".to_string(), "Green".to_string(), "Blue".to_string()],
                1,
                2,
                Duration::from_secs(2),
            )
            .await
        });
        let line = write_rx.recv().await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["method"], "elicitation/create");
        let id = frame["id"].as_str().unwrap();
        let params = &frame["params"];
        assert_eq!(params["mode"], "form");
        let choices_schema = &params["requestedSchema"]["properties"]["choices"];
        assert_eq!(choices_schema["type"], "array");
        assert_eq!(choices_schema["minItems"], 1);
        assert_eq!(choices_schema["maxItems"], 2);
        rpc_for_response.dispatch_response(
            id,
            Some(json!({ "action": "accept", "content": { "choices": ["choice-0", "choice-2"] } })),
            None,
        );
        let answer = task.await.unwrap().unwrap();
        assert_eq!(answer, Some(vec!["Red".to_string(), "Blue".to_string()]));
    }

    #[tokio::test]
    async fn request_multi_choice_without_capability_returns_none() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_no_caps(Arc::clone(&rpc), Arc::clone(&pending));
        let result = ch
            .request_multi_choice(
                "Pick colors",
                &["Red".to_string(), "Green".to_string()],
                1,
                2,
                Duration::from_millis(50),
            )
            .await
            .unwrap();
        assert_eq!(result, None);
        assert!(write_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn supports_free_form_ask_is_false() {
        let (rpc, _write_rx) = make_rpc();
        let pending = make_pending();
        let ch = make_channel_form_caps(Arc::clone(&rpc), Arc::clone(&pending));
        // Free-form text remains Phase 2 of the elicitation rollout,
        // matching `AcpChannel`. Even with the form capability advertised
        // the channel cannot yet answer a no-choices `ask_user`.
        assert!(!ch.supports_free_form_ask());
    }

    // ── Session-owned turns ───────────────────────────────────────────

    fn shell_request() -> ChannelApprovalRequest {
        ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls".to_string(),
            raw_arguments: None,
            position: None,
        }
    }

    fn ring_channel(
        rpc: Arc<RpcOutbound>,
        pending: Arc<crate::rpc::context::ApprovalPendingMap>,
        hub: &Arc<crate::rpc::subscription::SubscriptionHub>,
    ) -> RpcApprovalChannel {
        make_channel_no_caps(rpc, pending).with_subscriptions(Arc::clone(hub))
    }

    #[tokio::test]
    async fn a_session_owned_prompt_with_no_viewer_fails_closed_as_unreachable() {
        let (rpc, mut write_rx) = make_rpc();
        let hub = Arc::new(crate::rpc::subscription::SubscriptionHub::new());
        let _route = hub.route_session("sess-1");
        let ch = ring_channel(rpc, make_pending(), &hub);

        let outcome = ch
            .request_approval_attributed_with_timeout("", &shell_request(), Duration::from_secs(60))
            .await
            .unwrap()
            .expect("an outcome");
        assert_eq!(outcome.response, ChannelApprovalResponse::Deny);
        assert!(matches!(
            outcome.source,
            zeroclaw_api::channel::ApprovalSource::Unreachable
        ));
        assert!(
            write_rx.try_recv().is_err(),
            "a session-owned prompt never goes to the creating connection"
        );
    }

    #[tokio::test]
    async fn a_session_owned_prompt_goes_to_the_ring_and_any_viewer_can_answer() {
        let (rpc, mut write_rx) = make_rpc();
        let pending = make_pending();
        let hub = Arc::new(crate::rpc::subscription::SubscriptionHub::new());
        let _route = hub.route_session("sess-1");
        hub.add_viewer(
            "sess-1",
            "viewer",
            1,
            tokio_util::sync::CancellationToken::new(),
        );
        let source = hub.session_source("sess-1");
        let ch = ring_channel(rpc, Arc::clone(&pending), &hub);

        let task = zeroclaw_spawn::spawn!(async move {
            ch.request_approval_attributed_with_timeout(
                "",
                &shell_request(),
                Duration::from_secs(60),
            )
            .await
        });
        let frame = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let crate::rpc::subscription::Read::Frames(frames) = hub.read(source, 1, 8)
                    && let Some((_, frame)) = frames.into_iter().next()
                {
                    return frame;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the prompt reaches the ring");
        assert_eq!(frame["type"], "approval_request");
        let request_id = frame["request_id"].as_str().unwrap().to_string();
        assert!(pending.resolve(&request_id, ChannelApprovalResponse::Approve));

        let outcome = task.await.unwrap().unwrap().expect("an outcome");
        assert_eq!(outcome.response, ChannelApprovalResponse::Approve);
        assert!(matches!(
            outcome.source,
            zeroclaw_api::channel::ApprovalSource::Operator
        ));
        assert!(write_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_pending_session_owned_prompt_fails_closed_when_the_last_viewer_detaches() {
        let (rpc, _write_rx) = make_rpc();
        let pending = make_pending();
        let hub = Arc::new(crate::rpc::subscription::SubscriptionHub::new());
        let _route = hub.route_session("sess-1");
        hub.add_viewer(
            "sess-1",
            "viewer",
            1,
            tokio_util::sync::CancellationToken::new(),
        );
        let ch = ring_channel(rpc, Arc::clone(&pending), &hub);
        let task = {
            let hub = Arc::clone(&hub);
            zeroclaw_spawn::spawn!(async move {
                let outcome = ch
                    .request_approval_attributed_with_timeout(
                        "",
                        &shell_request(),
                        Duration::from_secs(60),
                    )
                    .await;
                drop(hub);
                outcome
            })
        };
        let source = hub.session_source("sess-1");
        tokio::time::timeout(Duration::from_secs(5), async {
            while hub.head_seq(source) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the prompt is published before the viewer leaves");
        let crate::rpc::subscription::Read::Frames(frames) = hub.read(source, 1, 8) else {
            panic!("the ring holds the prompt");
        };
        let request_id = frames[0].1["request_id"].as_str().unwrap().to_string();
        hub.remove_viewer("sess-1", "viewer");

        let outcome = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the prompt must not park for its whole timeout")
            .unwrap()
            .unwrap()
            .expect("an outcome");
        assert_eq!(outcome.response, ChannelApprovalResponse::Deny);
        assert!(matches!(
            outcome.source,
            zeroclaw_api::channel::ApprovalSource::Unreachable
        ));
        assert!(
            !pending.contains(&request_id),
            "the pending entry is dropped"
        );
    }
}
