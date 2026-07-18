//! Integration tests for approval gate + Herdr observer interaction.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use zeroclaw_api::channel::{Channel, ChannelApprovalRequest, ChannelApprovalResponse};
use zeroclaw_config::policy::AutonomyLevel;
use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig};
use zeroclaw_runtime::agent::turn::approval_gate::{ApprovalGateOutcome, gate_tool_approval};
use zeroclaw_runtime::agent::turn::context::TurnCtx;
use zeroclaw_runtime::approval::ApprovalManager;
use zeroclaw_runtime::integrations::herdr::HerdrObserver;
use zeroclaw_runtime::integrations::herdr::tests::{HerdrSpy, make_spy_reporter};
use zeroclaw_runtime::observability::{clear_broadcast_hook, set_scoped_broadcast_hook};

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

#[tokio::test]
async fn approval_gate_herdr_approve_transitions_blocked_to_working() {
    clear_broadcast_hook();

    // 1. Create spy and HerdrObserver
    let spy = HerdrSpy::new();
    let (reporter, calls) = make_spy_reporter(spy);
    let herdr_observer = Arc::new(HerdrObserver::new_with_reporter(reporter));
    let _guard = set_scoped_broadcast_hook(herdr_observer.clone());

    // 2. Session ID for report_agent payload
    zeroclaw_runtime::integrations::herdr::update_session_id("test-session-1");

    // 3. Non-interactive backchannel approval manager (shell in always_ask)
    let risk = RiskProfileConfig {
        level: AutonomyLevel::Supervised,
        always_ask: vec!["shell".into()],
        ..Default::default()
    };
    let approval = ApprovalManager::for_non_interactive_backchannel(&risk);

    // 4. Mock channel that approves
    let approval_count = Arc::new(AtomicUsize::new(0));
    let channel = Arc::new(TestChannel::new(
        ChannelApprovalResponse::Approve,
        approval_count.clone(),
    )) as Arc<dyn Channel>;

    // 5. Build TurnCtx
    let (tx, _rx) = mpsc::channel(8);
    let pacing = PacingConfig::default();
    let ctx = TurnCtx {
        observer: &*herdr_observer,
        provider_name: "test-provider",
        model: "test-model",
        temperature: None,
        approval: Some(&approval),
        channel_name: "test-channel",
        channel_reply_target: Some("user"),
        cancellation_token: None,
        on_delta: Some(&tx),
        event_tx: None,
        hooks: None,
        dedup_exempt_tools: &[],
        pacing: &pacing,
        strict_tool_parsing: false,
        channel: Some(&*channel),
        turn_id: "turn-1",
        agent_alias: None,
    };

    // 6. Call gate_tool_approval
    let outcome =
        gate_tool_approval(&ctx, "shell", &serde_json::json!({"cmd": "echo hi"}), 0).await;

    // 7. Verify outcome
    assert!(matches!(
        outcome,
        ApprovalGateOutcome::Proceed { approved: true }
    ));

    // 8. Verify HerdrObserver state transitions: Blocked -> Working
    let captured = calls.lock().drain(..).collect::<Vec<_>>();
    let state_sequence: Vec<&str> = captured
        .iter()
        .filter(|c| c.method == "pane.report_agent")
        .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
        .collect();

    assert_eq!(
        state_sequence,
        vec!["blocked", "working"],
        "Expected state sequence [blocked, working], got {:?}",
        state_sequence
    );
}

#[tokio::test]
async fn approval_gate_herdr_deny_transitions_blocked_to_idle() {
    clear_broadcast_hook();

    // 1. Create spy and HerdrObserver
    let spy = HerdrSpy::new();
    let (reporter, calls) = make_spy_reporter(spy);
    let herdr_observer = Arc::new(HerdrObserver::new_with_reporter(reporter));
    let _guard = set_scoped_broadcast_hook(herdr_observer.clone());

    // 2. Session ID
    zeroclaw_runtime::integrations::herdr::update_session_id("test-session-2");

    // 3. Non-interactive backchannel approval manager
    let risk = RiskProfileConfig {
        level: AutonomyLevel::Supervised,
        always_ask: vec!["shell".into()],
        ..Default::default()
    };
    let approval = ApprovalManager::for_non_interactive_backchannel(&risk);

    // 4. Mock channel that denies
    let approval_count = Arc::new(AtomicUsize::new(0));
    let channel = Arc::new(TestChannel::new(
        ChannelApprovalResponse::Deny,
        approval_count.clone(),
    )) as Arc<dyn Channel>;

    // 5. Build TurnCtx
    let (tx, _rx) = mpsc::channel(8);
    let pacing = PacingConfig::default();
    let ctx = TurnCtx {
        observer: &*herdr_observer,
        provider_name: "test-provider",
        model: "test-model",
        temperature: None,
        approval: Some(&approval),
        channel_name: "test-channel",
        channel_reply_target: Some("user"),
        cancellation_token: None,
        on_delta: Some(&tx),
        event_tx: None,
        hooks: None,
        dedup_exempt_tools: &[],
        pacing: &pacing,
        strict_tool_parsing: false,
        channel: Some(&*channel),
        turn_id: "turn-1",
        agent_alias: None,
    };

    // 6. Call gate_tool_approval
    let outcome =
        gate_tool_approval(&ctx, "shell", &serde_json::json!({"cmd": "rm -rf /"}), 0).await;

    // 7. Verify outcome is Deny
    assert!(matches!(outcome, ApprovalGateOutcome::Deny(_)));

    // 8. Verify HerdrObserver state transitions: Blocked -> Idle
    let captured = calls.lock().drain(..).collect::<Vec<_>>();
    let state_sequence: Vec<&str> = captured
        .iter()
        .filter(|c| c.method == "pane.report_agent")
        .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
        .collect();

    assert_eq!(
        state_sequence,
        vec!["blocked", "idle"],
        "Expected state sequence [blocked, idle], got {:?}",
        state_sequence
    );
}
