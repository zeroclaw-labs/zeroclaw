//! Collaborative regression tests for the herdr integration.
//!
//! Test 1 — herdr_observer_state_machine_test:
//!   Drives HerdrObserver through the full state machine (Idle → Working →
//!   Idle → Blocked → Working → Released) by feeding it raw ObserverEvents.
//!   Verifies debouncing (duplicate events within the same state are ignored)
//!   and the terminal Released state (idempotent on re-entry).
//!
//! Test 2 — approval_gate_herdr_integration_test:
//!   End-to-end: an in-memory ApprovalGate flow emits AuthorizationRequested
//!   and AuthorizationResponded events through a shared BroadcastHook, and
//!   the HerdrObserver installed via try_install_hook translates them into
//!   the correct report_agent state transitions.
//!
//! Both tests are self-contained: they do not touch the network or a real
//! herdr daemon socket; instead they capture the JSON payloads the
//! HerdrClient would have written to the UDS.

use std::cell::RefCell;
use std::sync::Arc;

use zeroclaw_api::observability_traits::{
    AuthorizationResponseType, Observer,
    ObserverEvent,
};
use zeroclaw_config::schema::HerdrConfig;

mod herdr_integration {

    use super::*;
    use crate::observability::{BroadcastHookGuard, clear_broadcast_hook,
                               set_scoped_broadcast_hook};

    // ---------- helpers --------------------------------------------------------

    /// Minimal config that opts into the herdr integration. The env vars are
    /// injected per test, so the socket path is a dummy — the client is
    /// intercepted before any connect() call.
    fn herdr_cfg() -> HerdrConfig {
        HerdrConfig { enabled: true }
    }

    /// A spy client that captures every request the observer hands it instead
    /// of writing to a Unix domain socket.
    struct SpyClient {
        socket_path: String,
        pane_id: String,
        sent: RefCell<Vec<CapturedRequest>>,
    }

    #[derive(Debug, Clone)]
    struct CapturedRequest {
        method: String,
        params: serde_json::Value,
    }

    impl SpyClient {
        fn new(socket_path: impl Into<String>, pane_id: impl Into<String>)
        -> Self {
            Self {
                socket_path: socket_path.into(),
                pane_id: pane_id.into(),
                sent: RefCell::new(Vec::new()),
            }
        }

        fn drain(&self) -> Vec<CapturedRequest> {
            self.sent.borrow_mut().drain(..).collect()
        }

        /// Build a HerdrClient pointing at this spy without actually using it
        /// (we avoid HerdrClient::send by constructing the reporter manually).
    }

    // Re-export the real HerdrClient internals we need to drive the reporter.
    use crate::integrations::herdr::{
        DebouncedReporter, HerdrClient,
    };

    fn make_reporter() -> DebouncedReporter {
        let client = HerdrClient::new("/tmp/does-not-exist".into(),
                                     "pane-test".into());
        DebouncedReporter::new(client)
    }

    thread_local! {
        static SESSION_ID: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    fn set_session_id(sid: impl Into<String>) {
        SESSION_ID.with(|slot| *slot.borrow_mut() = Some(sid.into()));
    }

    fn clear_session_id() {
        SESSION_ID.with(|slot| *slot.borrow_mut() = None);
    }

    // A broadcast-hook observer that collects events in a Vec.
    #[derive(Debug, Default, Clone)]
    struct EventCollector {
        events: Arc<parking_lot::Mutex<Vec<ObserverEvent>>>,
    }

    impl Observer for EventCollector {
        fn record_event(&self, event: &ObserverEvent) {
            self.events.lock().push(event.clone());
        }

        fn flush(&self) {}

        fn name(&self) -> &str { "event-collector" }

        fn as_any(&self) -> &dyn std::any::Any { self }
    }

    // ------------------------------------------------------------------------
    // Test 1: state machine
    // ------------------------------------------------------------------------

    /// # [herdr_observer_state_machine_test]
    ///
    /// Installs a real HerdrObserver inside a scoped broadcast hook, then
    /// feeds it the full lifecycle sequence and inspects the captured
    /// requests to verify:
    ///   * Idle → Working on first activity event
    ///   * Working is debounced (repeated AgentStart does not re-send)
    ///   * TurnComplete transitions to Idle
    ///   * AuthorizationRequested transitions to Blocked
    ///   * AuthorizationResponded{granted} transitions back to Working
    ///   * AuthorizationResponded{!granted} transitions to Idle
    ///   * AgentEnd transitions to Released (terminal, re-entry is no-op)
    #[test]
    fn herdr_observer_state_machine_test() {
        clear_broadcast_hook();
        clear_session_id();

        // Build observer with a client pointed at a dummy socket; because
        // we never call send() through the real client we just care that the
        // Mutex-protected reporter advances through the right states.
        let reporter = std::sync::Mutex::new(make_reporter());
        let observer: Arc<dyn Observer> = Arc::new(crate::integrations::herdr::HerdrObserver::new_with_reporter(
            // We reach for the pub(crate) constructor exposed specifically
            // for tests via the `herdr` feature gate in the integration
            // module infra.  If it is gated, we build the reporter body
            // directly.
            reporter.lock().unwrap().clone(),
        ));

        let _guard: BroadcastHookGuard = set_scoped_broadcast_hook(observer.clone());

        // --- Set a session ID (simulates loop_.rs after memory_session_id) ---
        crate::integrations::herdr::update_session_id("sess-abc-123");

        // Helper to record an event through the wrapped observer that flows
        // through the TeeObserver → broadcast hook path used in production.
        let fire = |event: ObserverEvent| {
            // Grab whatever the broadcast hook points at and record into it
            // directly.  This mirrors what the agent loop does: it calls
            // ctx.observer.record_event(), and the TeeObserver inside that
            // ctx.observer fans out to the broadcast hook.
            if let Some(hook) = crate::observability::current_broadcast_hook() {
                hook.record_event(&event);
            }
        };

        // Track reporter state transitions by peeking at the internal
        // HerdrState via the reporter after each event.  We use drop-scope
        // to access the MutexGuard safely.
        let latest_state = Arc::new(parking_lot::Mutex::new(
            crate::integrations::herdr::HerdrState::Idle,
        ));

        fire(ObserverEvent::AgentStart {
            model_provider: "anthropic".into(),
            model: "claude".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });

        {
            let r = reporter.lock().unwrap();
            // We can't read the private `state` field, so we capture the
            // side-effects: after the first activity event there must have
            // been at least one report_working call recorded in the client.
            let sent = r.client.captured();
            assert!(
                sent.iter().any(|c| c.method == "pane.report_agent"
                    && c.params.get("state")
                        == Some(&serde_json::json!("working"))),
                "expected working state after AgentStart, got: {:?}",
                sent
            );
        }

        // Repeated AgentStart is debounced — no second working call.
        fire(ObserverEvent::AgentStart {
            model_provider: "anthropic".into(),
            model: "claude".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        {
            let r = reporter.lock().unwrap();
            let all: Vec<_> = r.client.captured();
            let working_count = all.iter()
                .filter(|c| c.method == "pane.report_agent"
                    && c.params.get("state")
                        == Some(&serde_json::json!("working")))
                .count();
            assert_eq!(working_count, 1,
                       "AgentStart must be debounced; got {} working calls",
                       working_count);
        }

        // TurnComplete → Idle
        fire(ObserverEvent::TurnComplete);
        {
            let r = reporter.lock().unwrap();
            let all: Vec<_> = r.client.captured();
            assert!(
                all.iter().any(|c| c.method == "pane.report_agent"
                    && c.params.get("state")
                        == Some(&serde_json::json!("idle"))),
                "expected idle after TurnComplete, got: {:?}",
                all
            );
        }

        // AuthorizationRequested → Blocked
        fire(ObserverEvent::AuthorizationRequested {
            tool_name: "shell".into(),
            arguments_summary: "rm -rf /".into(),
            channel: Some("cli".into()),
            agent_alias: None,
            turn_id: Some("turn-1".into()),
        });
        {
            let r = reporter.lock().unwrap();
            let all: Vec<_> = r.client.captured();
            assert!(
                all.iter().any(|c| c.method == "pane.report_agent"
                    && c.params.get("state")
                        == Some(&serde_json::json!("blocked"))),
                "expected blocked after AuthorizationRequested, got: {:?}",
                all
            );
        }

        // AuthorizationResponded{granted} → Working
        fire(ObserverEvent::AuthorizationResponded {
            tool_name: "shell".into(),
            granted: true,
            response_type: AuthorizationResponseType::Yes,
            channel: Some("cli".into()),
            agent_alias: None,
            turn_id: Some("turn-1".into()),
        });
        {
            let r = reporter.lock().unwrap();
            let all: Vec<_> = r.client.captured();
            assert!(
                all.iter().any(|c| c.method == "pane.report_agent"
                    && c.params.get("state")
                        == Some(&serde_json::json!("working"))),
                "expected working after granted AuthorizationResponded, got: {:?}",
                all
            );
        }

        // AuthorizationResponded{!granted} → Idle
        fire(ObserverEvent::AuthorizationResponded {
            tool_name: "shell".into(),
            granted: false,
            response_type: AuthorizationResponseType::No,
            channel: Some("cli".into()),
            agent_alias: None,
            turn_id: Some("turn-1".into()),
        });
        {
            let r = reporter.lock().unwrap();
            let all: Vec<_> = r.client.captured();
            assert!(
                all.iter().any(|c| c.method == "pane.report_agent"
                    && c.params.get("state")
                        == Some(&serde_json::json!("idle"))),
                "expected idle after denied AuthorizationResponded, got: {:?}",
                all
            );
        }

        // AgentEnd → Released (terminal)
        fire(ObserverEvent::AgentEnd {
            model_provider: "anthropic".into(),
            model: "claude".into(),
            duration: std::time::Duration::from_millis(100),
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        {
            let r = reporter.lock().unwrap();
            let all: Vec<_> = r.client.captured();
            assert!(
                all.iter().any(|c| c.method == "pane.release_agent"),
                "expected pane.release_agent after AgentEnd, got: {:?}",
                all
            );
        }

        // Re-firing AgentEnd on a Released observer must not panic.
        fire(ObserverEvent::AgentEnd {
            model_provider: "anthropic".into(),
            model: "claude".into(),
            duration: std::time::Duration::from_millis(50),
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });

        clear_broadcast_hook();
    }

    // ------------------------------------------------------------------------
    // Test 2: approval gate → herdr integration
    // ------------------------------------------------------------------------

    /// # [approval_gate_herdr_integration_test]
    ///
    /// Compiles the real `gate_tool_approval` path with a mock non-
    /// interactive channel that returns `ChannelApprovalResponse::Approve`,
    /// runs it through a `TurnCtx` wired to a HerdrObserver in the broadcast
    /// hook, and verifies that the observer transitions through
    /// `Working → Blocked → Working` as the approval gate emits the two
    /// authorization events.
    #[test]
    fn approval_gate_herdr_integration_test() {
        clear_broadcast_hook();
        clear_session_id();

        // Install a HerdrObserver in the broadcast hook the same way
        // try_install_hook would (env vars are injected).
        std::env::set_var("HERDR_ENV", "1");
        std::env::set_var("HERDR_SOCKET_PATH", "/tmp/herdr-noop.sock");
        std::env::set_var("HERDR_PANE_ID", "pane-it");

        // try_install_hook connects — the socket does not exist, so it will
        // return None.  Construct the observer directly instead.
        let reporter = std::sync::Mutex::new(make_reporter());
        let observer: Arc<dyn Observer> = Arc::new(crate::integrations::herdr::HerdrObserver::new_with_reporter(
            reporter.lock().unwrap().clone(),
        ));
        let _guard: BroadcastHookGuard = set_scoped_broadcast_hook(observer);

        crate::integrations::herdr::update_session_id("sess-int-1");

        // Build a minimal TurnCtx satisfying gate_tool_approval's needs:
        // - ctx.approval: Some(ApprovalManager in non-interactive mode)
        // - ctx.channel: Some(mock channel returning Approve)
        // - ctx.observer: TeeObserver that fans out to the broadcast hook.
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let mock_channel = MockChannel::new_approve();
        let cfg = zeroclaw_config::schema::ObservabilityConfig::default();
        let primary_observer = crate::observability::create_observer(&cfg);
        let ctx = TurnCtx::new(primary_observer, mock_channel, /* more fields */);

        // Drive the gate synchronously via the sync wrapper the runtime
        // uses in the CLI path.  The important part is that
        // AuthorizationRequested is emitted before the channel call and
        // AuthorizationResponded is emitted afterward with granted=true.
        let result = block_on(async {
            super::super::agent::turn::approval_gate::gate_tool_approval(
                &ctx, "shell", &serde_json::json!({"cmd": "echo hi"}), 0,
            ).await
        });

        assert!(matches!(result, ApprovalGateOutcome::Proceed { approved: true }));

        // Inspect the reporter's captured requests: must include blocked
        // (from AuthorizationRequested) and working (from
        // AuthorizationResponded{granted:true}).
        let all: Vec<_> = reporter.lock().unwrap().client.captured();
        let has_blocked = all.iter().any(|c|
            c.method == "pane.report_agent"
                && c.params.get("state") == Some(&serde_json::json!("blocked"))
        );
        let has_working = all.iter().any(|c|
            c.method == "pane.report_agent"
                && c.params.get("state") == Some(&serde_json::json!("working"))
        );
        assert!(has_blocked, "expected blocked state, got: {:?}", all);
        assert!(has_working, "expected working state after approval, got: {:?}", all);

        // Denial path: channel returns Deny → observer should transition
        // to Idle.
        let mock_channel_deny = MockChannel::new_deny();
        let ctx_deny = TurnCtx::new(
            crate::observability::create_observer(&cfg),
            mock_channel_deny,
        );
        let result_deny = block_on(async {
            super::super::agent::turn::approval_gate::gate_tool_approval(
                &ctx_deny, "shell", &serde_json::json!({"cmd": "rm -rf /"}), 0,
            ).await
        });
        assert!(matches!(result_deny, ApprovalGateOutcome::Deny(_)));
        let all2: Vec<_> = reporter.lock().unwrap().client.captured();
        let has_idle_after_deny = all2.iter().any(|c|
            c.method == "pane.report_agent"
                && c.params.get("state") == Some(&serde_json::json!("idle"))
        );
        assert!(has_idle_after_deny,
                "expected idle state after denied approval, got: {:?}",
                all2);

        std::env::remove_var("HERDR_ENV");
        std::env::remove_var("HERDR_SOCKET_PATH");
        std::env::remove_var("HERDR_PANE_ID");
        clear_broadcast_hook();
    }
}
