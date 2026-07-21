//! Integration tests for approval gate + Herdr observer interaction.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use zeroclaw_api::channel::{Channel, ChannelApprovalRequest, ChannelApprovalResponse};
use zeroclaw_config::policy::AutonomyLevel;
use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig};

use crate::agent::turn::approval_gate::{ApprovalGateOutcome, gate_tool_approval};
use crate::agent::turn::context::TurnCtx;
use crate::agent::turn::events::DraftEvent;
use crate::approval::ApprovalManager;
use crate::integrations::herdr::HerdrObserver;
use crate::integrations::herdr::tests::{HerdrSpy, make_spy_reporter};
use crate::observability::{clear_broadcast_hook, set_scoped_broadcast_hook};

struct TestChannel {
    response: ChannelApprovalResponse,
    approval_requests: Arc<AtomicUsize>,
}

impl TestChannel {
    fn new(response: ChannelApprovalResponse, approval_requests: Arc<AtomicUsize>) -> Self {
        Self {
            response,
            approval_requests,
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for TestChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
        )
    }
    fn alias(&self) -> &str {
        "test-channel"
    }
}

#[async_trait::async_trait]
impl Channel for TestChannel {
    fn name(&self) -> &str {
        "test-channel"
    }

    async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn request_approval(
        &self,
        _recipient: &str,
        _request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        self.approval_requests.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.response.clone()))
    }
}

fn build_ctx<'a>(
    observer: &'a HerdrObserver,
    approval: &'a ApprovalManager,
    channel: &'a Arc<dyn Channel>,
    tx: &'a tokio::sync::mpsc::Sender<DraftEvent>,
    pacing: &'a PacingConfig,
) -> TurnCtx<'a> {
    TurnCtx {
        observer,
        provider_name: "test-provider",
        model: "test-model",
        temperature: None,
        approval: Some(approval),
        channel_name: "test-channel",
        channel_reply_target: Some("user"),
        cancellation_token: None,
        on_delta: Some(tx),
        event_tx: None,
        hooks: None,
        dedup_exempt_tools: &[],
        pacing,
        strict_tool_parsing: false,
        channel: Some(channel.as_ref()),
        turn_id: "turn-1",
        agent_alias: None,
        parent_agent_alias: None,
    }
}

fn make_risk() -> RiskProfileConfig {
    RiskProfileConfig {
        level: AutonomyLevel::Supervised,
        always_ask: vec!["shell".into()],
        ..Default::default()
    }
}

macro_rules! assert_state_sequence {
    ($calls:expr, $expected:expr) => {{
        let captured: Vec<_> = $calls.lock().drain(..).collect();
        let seq: Vec<&str> = captured
            .iter()
            .filter(|c| c.method == "pane.report_agent")
            .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
            .collect();
        assert_eq!(seq, $expected, "state sequence mismatch: {:?}", seq);
    }};
}

#[tokio::test]
async fn approval_gate_herdr_approve_transitions_blocked_to_working() {
    clear_broadcast_hook();
    let spy = HerdrSpy::new();
    let (client, calls) = make_spy_reporter(spy);
    let herdr = Arc::new(HerdrObserver::new(client, None));
    let _guard = set_scoped_broadcast_hook(herdr.clone());

    let approval = ApprovalManager::for_non_interactive_backchannel(&make_risk());
    let channel = Arc::new(TestChannel::new(
        ChannelApprovalResponse::Approve,
        Arc::new(AtomicUsize::new(0)),
    )) as Arc<dyn Channel>;
    let (tx, _rx) = mpsc::channel(8);
    let pacing = PacingConfig::default();
    let ctx = build_ctx(&herdr, &approval, &channel, &tx, &pacing);

    let outcome =
        gate_tool_approval(&ctx, "shell", &serde_json::json!({"cmd": "echo hi"}), 0).await;
    assert!(matches!(
        outcome,
        ApprovalGateOutcome::Proceed { approved: true }
    ));
    assert_state_sequence!(calls, vec!["blocked", "working"]);
}

#[tokio::test]
async fn approval_gate_herdr_deny_transitions_blocked_to_idle() {
    clear_broadcast_hook();
    let spy = HerdrSpy::new();
    let (client, calls) = make_spy_reporter(spy);
    let herdr = Arc::new(HerdrObserver::new(client, None));
    let _guard = set_scoped_broadcast_hook(herdr.clone());

    let approval = ApprovalManager::for_non_interactive_backchannel(&make_risk());
    let channel = Arc::new(TestChannel::new(
        ChannelApprovalResponse::Deny,
        Arc::new(AtomicUsize::new(0)),
    )) as Arc<dyn Channel>;
    let (tx, _rx) = mpsc::channel(8);
    let pacing = PacingConfig::default();
    let ctx = build_ctx(&herdr, &approval, &channel, &tx, &pacing);

    let outcome =
        gate_tool_approval(&ctx, "shell", &serde_json::json!({"cmd": "rm -rf /"}), 0).await;
    assert!(matches!(outcome, ApprovalGateOutcome::Deny(_)));
    assert_state_sequence!(calls, vec!["blocked", "idle"]);
}
