//! WebSocket-backed [`Channel`] implementation that surfaces tool approval
//! prompts to the gateway client and waits for the operator's decision.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::channel::{
    ApprovalReplyResult, ApprovalSource, AttributedApprovalResponse, Channel,
    ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};

/// Persist a rejected approval reply through the bound session owner. A
/// missing native port is the legacy non-persistent path and has no event
/// store to write to.
pub async fn record_rejected_reply(
    port: Option<&Arc<dyn zeroclaw_api::session_events::capture::SessionExecutionPort>>,
    request_id: Option<String>,
    classification: &str,
    evidence: serde_json::Value,
) -> anyhow::Result<()> {
    let Some(port) = port else {
        return Ok(());
    };
    port.record_approval_reply_rejected(request_id, classification.to_owned(), evidence)
        .await
}

/// Shared map keyed by `request_id`. Consumed by the receive loop to resolve
/// the oneshot when an `approval_response` frame arrives.
pub type PendingApprovals = Arc<Mutex<HashMap<String, PendingApproval>>>;

pub struct PendingApproval {
    pub(crate) registration_id: Uuid,
    pub(crate) sender: oneshot::Sender<ApprovalReplyResult>,
    pub(crate) audit: Option<Arc<dyn zeroclaw_api::session_events::capture::PreparedToolPort>>,
}

struct PendingApprovalGuard {
    pending: PendingApprovals,
    request_id: String,
    registration_id: Uuid,
}

impl PendingApprovalGuard {
    fn register(
        pending: PendingApprovals,
        request_id: String,
        sender: oneshot::Sender<ApprovalReplyResult>,
    ) -> Self {
        let registration_id = Uuid::new_v4();
        pending.lock().insert(
            request_id.clone(),
            PendingApproval {
                registration_id,
                sender,
                audit: zeroclaw_api::session_events::capture::current_tool_preparation(),
            },
        );
        Self {
            pending,
            request_id,
            registration_id,
        }
    }
}

impl Drop for PendingApprovalGuard {
    fn drop(&mut self) {
        let mut pending = self.pending.lock();
        if pending
            .get(&self.request_id)
            .is_some_and(|entry| entry.registration_id == self.registration_id)
        {
            pending.remove(&self.request_id);
        }
    }
}

/// Both the idle and mid-turn receive loops use the same delivery boundary.
pub(crate) async fn resolve_approval_reply(
    pending: &PendingApprovals,
    port: Option<&Arc<dyn zeroclaw_api::session_events::capture::SessionExecutionPort>>,
    request_id: &str,
    decision: ChannelApprovalResponse,
) -> anyhow::Result<()> {
    let entry = pending.lock().remove(request_id);
    let (classification, audit) = match entry {
        Some(entry) => {
            let reply_event_id = if !entry.sender.is_closed()
                && let Some(audit) = &entry.audit
            {
                match audit
                    .record_approval_reply(
                        zeroclaw_api::session_events::EventActor::System {
                            component: "approval_identity_unavailable".into(),
                        },
                        decision.clone(),
                        serde_json::json!({
                            "transport":"websocket", "request_id":request_id,
                            "identity_unavailable_reason":"websocket_principal_not_bound",
                        }),
                    )
                    .await
                {
                    Ok(reply_event_id) => Some(reply_event_id),
                    Err(error) => {
                        let (handler_error, waiter_error) =
                            zeroclaw_api::session_events::capture::share_capture_failure(error);
                        let _ = entry.sender.send(Err(waiter_error));
                        return Err(handler_error);
                    }
                }
            } else {
                None
            };
            if entry
                .sender
                .send(Ok(AttributedApprovalResponse::operator(decision)
                    .with_reply_event_id(reply_event_id)))
                .is_ok()
            {
                return Ok(());
            }
            ("receiver_closed", entry.audit)
        }
        None => ("unknown_or_expired_request", None),
    };
    let evidence = serde_json::json!({"transport":"websocket","classification":classification});
    if let Some(audit) = audit {
        return audit
            .record_approval_reply_rejected(classification.into(), evidence)
            .await;
    }
    record_rejected_reply(port, Some(request_id.to_owned()), classification, evidence).await
}

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

impl ::zeroclaw_api::attribution::Attributable for WsApprovalChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "ws_approval"
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

    /// `send` above is a deliberate no-op (see comment). Surfaces that must
    /// genuinely deliver — `poll`'s formatted-text fallback and
    /// `escalate_to_human` — check this so they fail honestly instead of
    /// claiming success for a message that was never rendered.
    fn supports_outbound_send(&self) -> bool {
        false
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // The gateway WS path does not act as a message source for the
        // channel orchestrator; turns are driven directly by the WS
        // handler loop. Listen is a no-op for this transport.
        Ok(())
    }

    fn supports_free_form_ask(&self) -> bool {
        false
    }

    /// Delegates to [`Self::request_approval_attributed`] and drops the
    /// provenance, so the prompt/timeout logic lives in exactly one place.
    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        Ok(self
            .request_approval_attributed(recipient, request)
            .await?
            .map(|attributed| attributed.response))
    }

    async fn request_approval_attributed(
        &self,
        _recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<AttributedApprovalResponse>> {
        let request_id = Uuid::new_v4().to_string();
        let (tx, mut rx) = oneshot::channel();
        let registration =
            PendingApprovalGuard::register(self.pending.clone(), request_id.clone(), tx);

        let event = TurnEvent::ApprovalRequest {
            request_id: request_id.clone(),
            tool_name: request.tool_name.clone(),
            arguments_summary: request.arguments_summary.clone(),
            timeout_secs: self.timeout.as_secs(),
        };
        let send_prompt = async { self.event_tx.send(event).await.map_err(anyhow::Error::new) };
        match zeroclaw_api::channel::await_approval_prompt_send(send_prompt, &mut rx).await {
            Ok(zeroclaw_api::channel::ApprovalPromptOutcome::Sent(())) => {}
            Ok(zeroclaw_api::channel::ApprovalPromptOutcome::Replied(response)) => {
                return Ok(Some(response));
            }
            Err(error) if zeroclaw_api::session_events::capture::is_capture_stopped(&error) => {
                return Err(error);
            }
            Err(_) => {
                return Ok(Some(AttributedApprovalResponse::from_runtime(
                    ChannelApprovalResponse::Deny,
                    ApprovalSource::Unreachable,
                )));
            }
        }

        let result = match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(decision)) => decision.map(Some),
            Ok(Err(_)) => {
                // Sender dropped without responding (connection closed
                // mid-prompt). Treat as deny rather than None so the agent
                // does not silently fall back to "no channel handled this" —
                // but mark it Unreachable, because nobody answered.
                Ok(Some(AttributedApprovalResponse::from_runtime(
                    ChannelApprovalResponse::Deny,
                    ApprovalSource::Unreachable,
                )))
            }
            Err(_) => {
                // Timeout: pop and deny. Mirrors Telegram / Slack behaviour
                // when the operator does not tap a button in time. This deny is
                // the runtime's, not the operator's.
                Ok(Some(AttributedApprovalResponse::from_runtime(
                    ChannelApprovalResponse::Deny,
                    ApprovalSource::TimedOut,
                )))
            }
        };
        drop(registration);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use zeroclaw_api::session_events::capture::{
        EngineTurnFuture, EngineTurnInput, SessionExecutionPort,
    };

    struct RejectionRecorder {
        pending: PendingApprovals,
        records: Mutex<Vec<(Option<String>, String, serde_json::Value)>>,
        fail: bool,
    }

    #[async_trait]
    impl SessionExecutionPort for RejectionRecorder {
        async fn execute(
            &self,
            _input: EngineTurnInput,
            _engine: EngineTurnFuture<'_>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("approval_test_must_not_execute")
        }

        async fn record_approval_reply_rejected(
            &self,
            request_id: Option<String>,
            classification: String,
            evidence: serde_json::Value,
        ) -> anyhow::Result<()> {
            assert!(
                self.pending.try_lock().is_some(),
                "transport must release the registry lock before awaiting the owner"
            );
            anyhow::ensure!(!self.fail, "approval_audit_unavailable");
            self.records
                .lock()
                .push((request_id, classification, evidence));
            Ok(())
        }
    }

    fn rejection_recorder(pending: &PendingApprovals, fail: bool) -> Arc<RejectionRecorder> {
        Arc::new(RejectionRecorder {
            pending: pending.clone(),
            records: Mutex::new(Vec::new()),
            fail,
        })
    }

    #[tokio::test]
    async fn dropped_prompt_waiters_remove_registrations_and_reject_late_replies() {
        for blocked_prompt_send in [false, true] {
            let (tx, _events) = mpsc::channel(1);
            if blocked_prompt_send {
                tx.try_send(TurnEvent::ApprovalRequest {
                    request_id: "earlier-prompt".into(),
                    tool_name: "file_write".into(),
                    arguments_summary: "path=earlier.txt".into(),
                    timeout_secs: 30,
                })
                .expect("fill outbound queue");
            }
            let pending = new_pending_approvals();
            let channel = WsApprovalChannel::new(tx, pending.clone(), Duration::from_secs(30));
            let request = approval_request();
            let mut waiter = Box::pin(channel.request_approval_attributed("operator", &request));
            tokio::select! {
                biased;
                result = &mut waiter => panic!("approval should remain pending: {result:?}"),
                () = std::future::ready(()) => {}
            }
            let request_id = pending.lock().keys().next().expect("registered").clone();
            drop(waiter);
            assert!(pending.lock().is_empty());

            let recorder = rejection_recorder(&pending, false);
            let port: Arc<dyn SessionExecutionPort> = recorder.clone();
            resolve_approval_reply(
                &pending,
                Some(&port),
                &request_id,
                ChannelApprovalResponse::Approve,
            )
            .await
            .expect("late reply audit");
            let records = recorder.records.lock();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].0.as_deref(), Some(request_id.as_str()));
            assert_eq!(records[0].1, "unknown_or_expired_request");
        }
    }

    #[test]
    fn stale_registration_guard_preserves_replacement() {
        let pending = new_pending_approvals();
        let (old_tx, mut old_rx) = oneshot::channel();
        let old = PendingApprovalGuard::register(pending.clone(), "request".into(), old_tx);
        let (new_tx, mut new_rx) = oneshot::channel();
        let new = PendingApprovalGuard::register(pending.clone(), "request".into(), new_tx);
        assert!(matches!(
            old_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        drop(old);
        assert_eq!(pending.lock().len(), 1);
        assert!(matches!(
            new_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        drop(new);
        assert!(pending.lock().is_empty());
        assert!(matches!(
            new_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn closed_receiver_reply_is_audited_and_audit_failure_propagates() {
        for fail in [false, true] {
            let pending = new_pending_approvals();
            let (tx, rx) = oneshot::channel();
            let registration = PendingApprovalGuard::register(pending.clone(), "closed".into(), tx);
            drop(rx);
            let recorder = rejection_recorder(&pending, fail);
            let port: Arc<dyn SessionExecutionPort> = recorder.clone();
            let result = resolve_approval_reply(
                &pending,
                Some(&port),
                "closed",
                ChannelApprovalResponse::AlwaysApprove,
            )
            .await;
            assert!(pending.lock().is_empty());
            if fail {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "approval_audit_unavailable"
                );
                assert!(recorder.records.lock().is_empty());
            } else {
                result.expect("rejection audit succeeds");
                let records = recorder.records.lock();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].0.as_deref(), Some("closed"));
                assert_eq!(records[0].1, "receiver_closed");
                assert_eq!(records[0].2["transport"], "websocket");
            }
            drop(registration);
        }
    }

    #[tokio::test]
    async fn live_reply_reaches_waiter_without_rejection_audit() {
        let pending = new_pending_approvals();
        let (tx, rx) = oneshot::channel();
        let registration = PendingApprovalGuard::register(pending.clone(), "live".into(), tx);
        let recorder = rejection_recorder(&pending, true);
        let port: Arc<dyn SessionExecutionPort> = recorder.clone();
        resolve_approval_reply(
            &pending,
            Some(&port),
            "live",
            ChannelApprovalResponse::Approve,
        )
        .await
        .expect("successful delivery does not audit a rejection");
        assert_eq!(
            rx.await
                .expect("operator reply")
                .expect("approval captured")
                .response,
            ChannelApprovalResponse::Approve
        );
        assert!(pending.lock().is_empty());
        assert!(recorder.records.lock().is_empty());
        drop(registration);
    }

    #[tokio::test]
    async fn original_reply_is_recorded_before_delivery_and_survives_receiver_or_registration_changes()
     {
        use zeroclaw_api::session_events::EventActor;
        use zeroclaw_api::session_events::capture::{
            PreparedToolPort, ToolObservationPort, scope_tool_preparation,
        };

        struct Audit {
            arrived: tokio::sync::Notify,
            release: tokio::sync::Notify,
            fail: bool,
            replies: Mutex<Vec<(EventActor, ChannelApprovalResponse, serde_json::Value)>>,
            rejections: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl PreparedToolPort for Audit {
            async fn verify_action(&self, _: &str, _: &serde_json::Value) -> anyhow::Result<()> {
                unreachable!()
            }
            async fn request_approval(
                &self,
                _: zeroclaw_api::session_events::capture::ApprovalRequestIntent,
            ) -> anyhow::Result<()> {
                unreachable!()
            }
            async fn resolve_approval(
                &self,
                _: ChannelApprovalResponse,
                _: &str,
                _: Option<zeroclaw_api::session_events::EventId>,
                _: serde_json::Value,
            ) -> anyhow::Result<()> {
                unreachable!()
            }
            async fn invalidate(&self, _: &str) -> anyhow::Result<()> {
                unreachable!()
            }
            async fn start(&self) -> anyhow::Result<Arc<dyn ToolObservationPort>> {
                unreachable!()
            }
            async fn record_approval_reply(
                &self,
                actor: EventActor,
                response: ChannelApprovalResponse,
                evidence: serde_json::Value,
            ) -> anyhow::Result<zeroclaw_api::session_events::EventId> {
                self.arrived.notify_one();
                self.release.notified().await;
                if self.fail {
                    return Err(anyhow::Error::new(std::io::Error::other(
                        "reply_observation_failed",
                    )));
                }
                self.replies.lock().push((actor, response, evidence));
                Ok(zeroclaw_api::session_events::EventId::parse(
                    "fixture-recorded-reply",
                )?)
            }
            async fn record_approval_reply_rejected(
                &self,
                classification: String,
                _: serde_json::Value,
            ) -> anyhow::Result<()> {
                self.rejections.lock().push(classification);
                Ok(())
            }
        }
        for case in ["delivered", "failed", "receiver_closed", "replacement"] {
            let pending = new_pending_approvals();
            let audit = Arc::new(Audit {
                arrived: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                fail: case == "failed",
                replies: Mutex::new(Vec::new()),
                rejections: Mutex::new(Vec::new()),
            });
            let capture: Arc<dyn PreparedToolPort> = audit.clone();
            let (tx, rx) = oneshot::channel();
            let mut rx = Some(rx);
            let guard = scope_tool_preparation(capture, async {
                PendingApprovalGuard::register(pending.clone(), "reply".into(), tx)
            })
            .await;
            let task = {
                let pending = pending.clone();
                zeroclaw_spawn::spawn!(async move {
                    resolve_approval_reply(
                        &pending,
                        None,
                        "reply",
                        ChannelApprovalResponse::AlwaysApprove,
                    )
                    .await
                })
            };
            tokio::time::timeout(Duration::from_secs(10), audit.arrived.notified())
                .await
                .expect("audit reached");
            assert!(matches!(
                rx.as_mut().expect("receiver").try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            assert!(pending.lock().is_empty());
            let replacement = if case == "replacement" {
                let (tx, rx) = oneshot::channel();
                Some((
                    PendingApprovalGuard::register(pending.clone(), "reply".into(), tx),
                    rx,
                ))
            } else {
                None
            };
            if case == "receiver_closed" {
                drop(rx.take());
            }
            audit.release.notify_one();
            let result = tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("reply completed")
                .expect("task");
            if case == "failed" {
                let handler_error = result.expect_err("audit error");
                let waiter_error = rx
                    .take()
                    .expect("receiver")
                    .await
                    .expect("typed failure delivered")
                    .expect_err("audit failure");
                for error in [&handler_error, &waiter_error] {
                    assert!(zeroclaw_api::session_events::capture::is_capture_stopped(
                        error
                    ));
                    assert!(error.chain().any(|source| {
                        source
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|error| error.to_string() == "reply_observation_failed")
                    }));
                }
                assert!(audit.replies.lock().is_empty());
            } else {
                result.expect("reply observed");
                if let Some(rx) = rx.take() {
                    let delivered = rx.await.expect("reply").expect("approval captured");
                    assert_eq!(delivered.response, ChannelApprovalResponse::AlwaysApprove);
                    assert_eq!(
                        delivered.reply_event_id.as_ref().map(|id| id.as_str()),
                        Some("fixture-recorded-reply")
                    );
                }
                let replies = audit.replies.lock();
                assert_eq!(replies.len(), 1);
                assert_eq!(
                    replies[0].0,
                    EventActor::System {
                        component: "approval_identity_unavailable".into()
                    }
                );
                assert_eq!(replies[0].1, ChannelApprovalResponse::AlwaysApprove);
                assert_eq!(
                    replies[0].2["identity_unavailable_reason"],
                    "websocket_principal_not_bound"
                );
                let expected_rejections = if case == "receiver_closed" {
                    vec!["receiver_closed".to_owned()]
                } else {
                    vec![]
                };
                assert_eq!(*audit.rejections.lock(), expected_rejections);
            }
            drop(guard);
            if let Some((replacement, mut rx)) = replacement {
                assert_eq!(pending.lock().len(), 1);
                assert!(matches!(
                    rx.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
                drop(replacement);
            }
            assert!(pending.lock().is_empty());
        }
    }

    #[test]
    fn ws_approval_channel_declines_free_form_ask() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let pending = new_pending_approvals();
        let channel = WsApprovalChannel::new(tx, pending, Duration::from_secs(30));
        assert!(
            !channel.supports_free_form_ask(),
            "WsApprovalChannel must refuse free-form ask_user; \
             its send() is a no-op and listen() drops immediately"
        );
    }

    fn approval_request() -> ChannelApprovalRequest {
        ChannelApprovalRequest {
            tool_name: "file_write".to_string(),
            arguments_summary: "path=a.txt".to_string(),
            raw_arguments: None,
            position: None,
        }
    }

    #[tokio::test]
    async fn closed_prompt_transport_denies_as_unreachable_and_cleans_registration() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let pending = new_pending_approvals();
        let channel = WsApprovalChannel::new(tx, pending.clone(), Duration::from_secs(30));

        let attributed = channel
            .request_approval_attributed("operator", &approval_request())
            .await
            .expect("closed transport yields a runtime decision")
            .expect("the configured approval transport remains supported");
        assert_eq!(attributed.response, ChannelApprovalResponse::Deny);
        assert_eq!(attributed.source, ApprovalSource::Unreachable);
        assert!(pending.lock().is_empty());

        assert_eq!(
            channel
                .request_approval("operator", &approval_request())
                .await
                .expect("legacy caller receives the same runtime denial"),
            Some(ChannelApprovalResponse::Deny)
        );
        assert!(pending.lock().is_empty());
    }

    /// The operator never taps anything and the prompt times out. The channel
    /// still synthesizes `Some(Deny)` so the agent does not fall through to
    /// "no channel handled this" — but that deny is the RUNTIME's, and the
    /// provenance has to say so. Before this, the identical `Some(Deny)` was
    /// reported to the model as "Denied by user." on a run where no human was
    /// ever asked.
    #[tokio::test]
    async fn timed_out_prompt_denies_with_runtime_provenance() {
        // Keep the receiver alive so the send succeeds and we reach the timeout
        // rather than the "forward task gone" early return.
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let pending = new_pending_approvals();
        let channel = WsApprovalChannel::new(tx, pending, Duration::from_millis(50));

        let attributed = channel
            .request_approval_attributed("operator", &approval_request())
            .await
            .expect("timeout is not an error")
            .expect("a timeout still yields a deny, not None");

        assert_eq!(attributed.response, ChannelApprovalResponse::Deny);
        assert_eq!(
            attributed.source,
            ApprovalSource::TimedOut,
            "a prompt nobody answered is not an operator decision"
        );
        assert!(attributed.source.is_runtime_fail_closed());
    }

    /// The connection closes mid-prompt: the pending sender is dropped. Same
    /// contract as the timeout — a deny, but not the operator's.
    #[tokio::test]
    async fn dropped_responder_denies_with_runtime_provenance() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let pending = new_pending_approvals();
        let channel = WsApprovalChannel::new(tx, Arc::clone(&pending), Duration::from_secs(30));

        let call = async {
            channel
                .request_approval_attributed("operator", &approval_request())
                .await
        };
        // Drop the registered oneshot sender out from under the waiter, which is
        // what a closing WebSocket does.
        let dropper = async {
            for _ in 0..100 {
                {
                    let mut guard = pending.lock();
                    if let Some(key) = guard.keys().next().cloned() {
                        guard.remove(&key);
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let (result, ()) = tokio::join!(call, dropper);

        let attributed = result
            .expect("a dropped responder is not an error")
            .expect("a dropped responder still yields a deny, not None");
        assert_eq!(attributed.response, ChannelApprovalResponse::Deny);
        assert_eq!(
            attributed.source,
            ApprovalSource::Unreachable,
            "a responder that went away is not an operator decision"
        );
    }

    /// The legacy entry point must keep its existing shape: provenance is an
    /// addition, not a change to what `request_approval` returns.
    #[tokio::test]
    async fn legacy_request_approval_still_returns_a_bare_deny_on_timeout() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let pending = new_pending_approvals();
        let channel = WsApprovalChannel::new(tx, pending, Duration::from_millis(50));

        let response = channel
            .request_approval("operator", &approval_request())
            .await
            .expect("timeout is not an error");
        assert_eq!(response, Some(ChannelApprovalResponse::Deny));
    }
}
