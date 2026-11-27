use super::*;
use crate::rpc::dispatch::{
    Method,
    connection_test_support::{
        GoalParentAdmissionCase, GoalReattachPeer, REATTACH_OWNER, REATTACH_SID,
        prepare_held_goal_parent_fixture, prepare_held_goal_verifier,
    },
};
use crate::rpc::session::SessionStore;
use crate::rpc::types::{GoalResponseWire, SessionGoalResult};
use serde_json::json;
use std::sync::atomic::Ordering;
use zeroclaw_api::channel::{ApprovalSource, ChannelApprovalRequest, ChannelApprovalResponse};
use zeroclaw_infra::session_queue::SessionActorQueue;

const TOKEN: &str = "synthetic-approval-token";
const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum LatestCandidate {
    Disconnects,
    Installs,
}

#[tokio::test]
async fn goal_approval_disconnected_latest_claim_preserves_installed_wss_route() {
    exercise_approval_rebind(LatestCandidate::Disconnects).await;
}

#[tokio::test]
async fn goal_approval_latest_claim_wins_reordered_install_and_late_wss_teardown() {
    exercise_approval_rebind(LatestCandidate::Installs).await;
}

async fn exercise_approval_rebind(latest: LatestCandidate) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tmp = tempfile::tempdir().unwrap();
    let mut config = zeroclaw_config::schema::Config {
        data_dir: tmp.path().to_owned(),
        config_path: tmp.path().join("config.toml"),
        ..Default::default()
    };
    config.gateway.paired_tokens = vec![TOKEN.into()];
    let sessions = Arc::new(SessionStore::new(
        64,
        Arc::new(SessionActorQueue::new(4, 30, 60)),
    ));
    let mut ctx = RpcContext::minimal(config, sessions);
    std::fs::write(tmp.path().join(".secret_key"), "42".repeat(32)).unwrap();
    Arc::get_mut(&mut ctx).unwrap().tui_registry =
        Arc::new(crate::rpc::tui_identity::TuiRegistry::new(tmp.path()));
    let (_, parent) = prepare_held_goal_parent_fixture(
        &mut ctx,
        tmp.path(),
        REATTACH_SID,
        REATTACH_OWNER,
        GoalParentAdmissionCase::DirectResume,
    )
    .await;
    let verifier = prepare_held_goal_verifier(&ctx).await;

    let materials = zeroclaw_tls::ensure_server_materials(tmp.path(), &[]).unwrap();
    let client = zeroclaw_tls::issue_client_cert(
        &std::fs::read_to_string(&materials.ca_cert_path).unwrap(),
        &std::fs::read_to_string(&materials.ca_key_path).unwrap(),
        "goal-approval-rebind",
    )
    .unwrap();
    let acceptor = build_tls_acceptor(
        materials.server_cert_path.to_str().unwrap(),
        materials.server_key_path.to_str().unwrap(),
        materials.ca_cert_path.to_str().unwrap(),
        &[],
        "",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("wss://{}/", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let cancel = CancellationToken::new();
    let server_ctx = Arc::clone(&ctx);
    let server_count = Arc::clone(&count);
    let server_cancel = cancel.clone();
    let server = zeroclaw_spawn::spawn!(serve_wss_listener(
        server_ctx,
        server_cancel,
        server_count,
        acceptor,
        listener,
        WssLimits {
            max_sessions_per_client: 3,
            ..WssLimits::default()
        }
    ));
    let connect = || async {
        let connector =
            super::accept_error_tests::test_client_connector(&client.cert_pem, &client.key_pem);
        let mut peer = tokio::time::timeout(
            WAIT,
            tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector)),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
        peer.initialize(&ctx, REATTACH_OWNER, Some(TOKEN)).await;
        peer
    };

    let mut original = connect().await;
    resume_approval_session(&mut original).await;
    let started = original
        .result(
            Method::SessionGoal,
            json!({"session_id": REATTACH_SID, "command": "/goal resume"}),
            3,
        )
        .await;
    assert!(matches!(
        serde_json::from_value::<SessionGoalResult>(started)
            .unwrap()
            .response,
        GoalResponseWire::Resumed(_)
    ));
    tokio::time::timeout(WAIT, parent.turn.started.notified())
        .await
        .expect("the live Goal must hold the Agent");
    let agent = ctx.sessions.get_agent(REATTACH_SID).await.unwrap();
    let generation = ctx.sessions.get_generation(REATTACH_SID).await;

    let mut older = connect().await;
    let (older_waiting, permit_older) = ctx.sessions.set_test_effect_pause("approval-agent-wait");
    resume_approval_session(&mut older).await;
    tokio::time::timeout(WAIT, older_waiting.notified())
        .await
        .expect("the older installer must park before Agent acquisition");
    // Live stream ownership is deliberately different from approval ownership
    // in both branches. The older peer reads history; the latest never does.
    let history = older
        .result(
            Method::SessionMessages,
            json!({"session_id": REATTACH_SID}),
            3,
        )
        .await;
    assert_eq!(history["messages"].as_array().unwrap().len(), 2);

    let mut newest = connect().await;
    resume_approval_session(&mut newest).await;
    assert!(
        agent.try_lock().is_err(),
        "both resumes precede parent release"
    );
    assert_eq!(ctx.sessions.get_generation(REATTACH_SID).await, generation);
    assert_eq!(count.load(Ordering::Acquire), 3);
    match latest {
        LatestCandidate::Disconnects => {
            newest.close(None).await.unwrap();
            drop(newest);
            wait_for_connections(&count, 2).await;
            assert!(
                agent.try_lock().is_err(),
                "canceled installer must drain before Agent release"
            );
            assert!(!parent.turn.completed.load(Ordering::Acquire));

            let (older_acquired, finish_older) =
                ctx.sessions.set_test_effect_pause("approval-install");
            parent.turn.release.notify_one();
            permit_older.notify_one();
            tokio::time::timeout(WAIT, older_acquired.notified())
                .await
                .unwrap();
            finish_older.notify_one();
            assert_operator_denial(&ctx, &mut original, 4).await;
        }
        LatestCandidate::Installs => {
            let (newest_acquired, finish_newest) =
                ctx.sessions.set_test_effect_pause("approval-install");
            parent.turn.release.notify_one();
            tokio::time::timeout(WAIT, newest_acquired.notified())
                .await
                .unwrap();
            finish_newest.notify_one();
            assert_operator_denial(&ctx, &mut newest, 4).await;

            let (older_acquired, finish_older) =
                ctx.sessions.set_test_effect_pause("approval-install");
            permit_older.notify_one();
            tokio::time::timeout(WAIT, older_acquired.notified())
                .await
                .unwrap();
            finish_older.notify_one();
            assert_operator_denial(&ctx, &mut newest, 5).await;

            older.close(None).await.unwrap();
            drop(older);
            wait_for_connections(&count, 2).await;
            assert_operator_denial(&ctx, &mut newest, 6).await;
        }
    }

    // Release the loopback verifier and settle the fixture before checking
    // that no installer or parent task retains the retired listener state.
    verifier.turn.release.notify_one();
    ctx.goal_runtime
        .dispose_session(REATTACH_SID)
        .await
        .unwrap();
    verifier.finish().await;
    cancel.cancel();
    tokio::time::timeout(WAIT, server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    wait_for_connections(&count, 0).await;
    let weak_context = Arc::downgrade(&ctx);
    drop(ctx);
    assert!(
        weak_context.upgrade().is_none(),
        "the listener generation must drain"
    );
}

async fn resume_approval_session(peer: &mut impl GoalReattachPeer) {
    let opened = peer
        .result(
            Method::SessionNew,
            json!({"session_id": REATTACH_SID, "agent_alias": "test-agent", "chat_mode": "chat"}),
            2,
        )
        .await;
    assert_eq!(opened["session_id"], REATTACH_SID);
}

async fn assert_operator_denial(ctx: &RpcContext, peer: &mut impl GoalReattachPeer, id: u64) {
    let agent = ctx.sessions.get_agent(REATTACH_SID).await.unwrap();
    // Read the current canonical Agent route, not one captured before handoff.
    let channel = tokio::time::timeout(WAIT, agent.lock())
        .await
        .unwrap()
        .channel_handles()
        .get_channel("rpc")
        .expect("the Agent must have a usable approval route");
    let pending = zeroclaw_spawn::spawn!(async move {
        channel
            .request_approval_attributed(
                "",
                &ChannelApprovalRequest {
                    tool_name: "synthetic-approval-fixture".into(),
                    arguments_summary: "No operation will execute.".into(),
                    raw_arguments: None,
                    position: None,
                },
            )
            .await
    });
    let frame = peer.next_frame().await;
    assert_eq!(frame["method"], "session/update", "{frame}");
    assert_eq!(frame["params"]["type"], "approval_request", "{frame}");
    assert_eq!(frame["params"]["session_id"], REATTACH_SID, "{frame}");
    assert_eq!(
        frame["params"]["tool_name"], "synthetic-approval-fixture",
        "{frame}"
    );
    let request_id = frame["params"]["request_id"].as_str().unwrap();
    let result = peer
        .result(
            Method::SessionApprove,
            json!({"session_id": REATTACH_SID, "request_id": request_id, "decision": "reject"}),
            id,
        )
        .await;
    assert_eq!(result["request_id"], request_id);
    assert_eq!(result["acknowledged"], true);
    let answer = tokio::time::timeout(WAIT, pending)
        .await
        .expect("the approval waiter must resolve through session/approve")
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(answer.response, ChannelApprovalResponse::Deny);
    assert_eq!(answer.source, ApprovalSource::Operator);
}

async fn wait_for_connections(count: &AtomicUsize, expected: usize) {
    tokio::time::timeout(WAIT, async {
        while count.load(Ordering::Acquire) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the listener's counted connections must drain");
}
