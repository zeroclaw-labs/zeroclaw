//! WebSocket-backed [`Channel`] implementation that surfaces tool approval
//! prompts to the gateway client and waits for the operator's decision.
//!
//! The agent's tool loop calls
//! [`Channel::request_approval`](zeroclaw_api::channel::Channel::request_approval)
//! whenever a supervised-mode tool needs operator consent. This struct mints
//! a `request_id`, emits a [`TurnEvent::ApprovalRequest`] that the existing
//! forward loop serialises onto the wire, and parks on a oneshot until the
//! matching `approval_response` frame arrives.
//!
//! The pending-request map is shared with the connection's receive loop; on
//! `approval_response` the loop pops the oneshot sender keyed by `request_id`
//! and resolves the agent's pending future. If the operator does not respond
//! within `timeout_secs` the wait yields `Deny`, matching the policy of every
//! other channel that implements `request_approval`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};

/// Shared map keyed by `request_id`. Consumed by the receive loop to resolve
/// the oneshot when an `approval_response` frame arrives.
pub type PendingApprovals = Arc<Mutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>;

/// Construct an empty pending-approvals registry for a fresh connection.
pub fn new_pending_approvals() -> PendingApprovals {
    Arc::new(Mutex::new(HashMap::new()))
}

/// `Channel` implementation that emits approval frames over a connection's
/// existing `event_tx` and parks on a oneshot until the matching response
/// arrives or `timeout` elapses.
pub struct WsApprovalChannel {
    event_tx: mpsc::Sender<TurnEvent>,
    pending: PendingApprovals,
    timeout: Duration,
}

impl WsApprovalChannel {
    pub fn new(
        event_tx: mpsc::Sender<TurnEvent>,
        pending: PendingApprovals,
        timeout: Duration,
    ) -> Self {
        Self {
            event_tx,
            pending,
            timeout,
        }
    }
}

#[async_trait]
impl Channel for WsApprovalChannel {
    fn name(&self) -> &str {
        "ws"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        // The gateway WS path streams agent output via TurnEvent::Chunk /
        // ::Thinking / ::ToolCall / ::ToolResult; it does not deliver
        // free-form `send()` messages. Returning Ok here keeps any caller
        // that probes for a generic delivery target from erroring out.
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // The gateway WS path does not act as a message source for the
        // channel orchestrator; turns are driven directly by the WS
        // handler loop. Listen is a no-op for this transport.
        Ok(())
    }

    async fn request_approval(
        &self,
        _recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(request_id.clone(), tx);

        let event = TurnEvent::ApprovalRequest {
            request_id: request_id.clone(),
            tool_name: request.tool_name.clone(),
            arguments_summary: request.arguments_summary.clone(),
            timeout_secs: self.timeout.as_secs(),
        };
        if self.event_tx.send(event).await.is_err() {
            // Forward task has gone away; the WS is closing. Clean up the
            // pending entry and let the agent's caller treat this the same
            // as any other channel that returns None: fall through to
            // auto-deny per ApprovalManager policy.
            self.pending.lock().remove(&request_id);
            return Ok(None);
        }

        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(decision)) => Ok(Some(decision)),
            Ok(Err(_)) => {
                // Sender dropped without responding (connection closed
                // mid-prompt). Treat as deny rather than None so the agent
                // does not silently fall back to "no channel handled this".
                self.pending.lock().remove(&request_id);
                Ok(Some(ChannelApprovalResponse::Deny))
            }
            Err(_) => {
                // Timeout: pop and deny. Mirrors Telegram / Slack behaviour
                // when the operator does not tap a button in time.
                self.pending.lock().remove(&request_id);
                Ok(Some(ChannelApprovalResponse::Deny))
            }
        }
    }
}
