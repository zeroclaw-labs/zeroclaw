//! A bounded delegate target that relays through `send_message_to_peer` must
//! not hand the recipient's turn more than the caller's own sealed ceiling —
//! the same invariant already enforced for `cron_add`/`cron_run`/
//! `cron_update`/`schedule`/`spawn_subagent`.
//!
//! `send_message_to_peer_tool` never received that treatment:
//! `SendMessageToPeerTool::execute` starts the recipient's turn through
//! `crate::agent::loop_::process_message`, which had no `allowed_tools`
//! parameter at all, so the recipient was always assembled from its OWN full
//! risk profile regardless of any ceiling in force on the sender's turn.
//!
//! Two production paths reach the tool with a sealed ceiling in force:
//!
//!   - the live `Bounded` delegate assembly in `delegate.rs`, which rebuilds
//!     `send_message_to_peer` bound to the target's own alias;
//!   - a bounded target's own turn is itself unbounded reused state for
//!     anything `all_tools_with_runtime` builds without a per-call ceiling —
//!     including, before this fix, `send_message_to_peer_tool` at its
//!     `mod.rs` call site, the same defect reached a second way.
//!
//! THIS TEST MUST FAIL if the ceiling stops being threaded into
//! `send_message_to_peer_tool` at either call site. Every assertion pairs the
//! negative (the out-of-ceiling tool) with a positive half (the in-ceiling
//! tool), because under a ceiling that stripped everything the negative would
//! hold for the wrong reason.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, DelegateExecutionMode, DelegateTargetConfig, RiskProfileConfig,
    RuntimeProfileConfig,
};
use zeroclaw_runtime::agent::loop_::AgentRunOverrides;

/// Held by caller, target AND peer — survives the ceiling and gives the
/// positive half of every assertion something to observe.
const WITHIN_CEILING: &str = "calculator";

/// Held by the PEER's own risk profile only. Never in the caller's or the
/// target's registry, so never in the sealed ceiling. Must not reach the
/// peer's relayed turn even though the peer's own profile would otherwise
/// grant it.
const OUT_OF_CEILING: &str = "weather";

const PEER_MARKER: &str = "ZC-PEER-CEILING-MARKER";

#[derive(Clone)]
struct Script {
    calls: Arc<AtomicUsize>,
    captured: Arc<AsyncMutex<Vec<String>>>,
    replies: Arc<Vec<String>>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-peerceiling",
        "object": "chat.completion",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": serde_json::Value::Null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string()
}

fn plain_content(text: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-peerceiling",
        "object": "chat.completion",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string()
}

async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    script
        .replies
        .get(call)
        .cloned()
        .unwrap_or_else(|| plain_content("done"))
}

async fn spawn_stub_provider(replies: Vec<String>) -> (SocketAddr, Arc<AsyncMutex<Vec<String>>>) {
    let script = Script {
        calls: Arc::new(AtomicUsize::new(0)),
        captured: Arc::new(AsyncMutex::new(Vec::new())),
        replies: Arc::new(replies),
    };
    let captured = Arc::clone(&script.captured);
    let app = Router::new()
        .route("/chat/completions", post(handle_chat))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    zeroclaw_spawn::spawn!(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    (addr, captured)
}

/// The tool names a captured request offered the model.
fn offered_tools(body: &str) -> BTreeSet<String> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return BTreeSet::new();
    };
    parsed
        .get("tools")
        .and_then(|tools| tools.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn peer_ceiling_config(provider_uri: &str, root: &std::path::Path) -> Config {
    let mut providers = zeroclaw_config::providers::Providers::default();
    {
        let base = providers
            .models
            .ensure("custom", "default")
            .expect("`custom` slot must exist on ModelProviders");
        base.api_key = Some("test-key".to_string());
        base.model = Some("test-model".to_string());
        base.uri = Some(provider_uri.to_string());
        base.native_tools = Some(true);
    }

    let permissive = |tools: Vec<String>| RiskProfileConfig {
        allowed_tools: tools,
        delegation_policy: DelegationPolicy {
            mode: DelegationMode::Allow,
        },
        ..RiskProfileConfig::default()
    };

    let mut risk_profiles = HashMap::new();
    risk_profiles.insert(
        "caller_profile".to_string(),
        permissive(vec![
            "delegate".to_string(),
            "send_message_to_peer".to_string(),
            WITHIN_CEILING.to_string(),
        ]),
    );
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec![
            "send_message_to_peer".to_string(),
            WITHIN_CEILING.to_string(),
        ]),
    );
    // The peer's OWN policy legitimately holds both — `OUT_OF_CEILING` is a
    // real permission of the peer's, never of the caller's or the target's.
    // The invariant under test is whether a RELAYED turn is bound by the
    // sender's ceiling, not whether the peer may ever use its own tool.
    risk_profiles.insert(
        "peer_profile".to_string(),
        permissive(vec![WITHIN_CEILING.to_string(), OUT_OF_CEILING.to_string()]),
    );

    let mut runtime_profiles = HashMap::new();
    runtime_profiles.insert(
        "agentic".to_string(),
        RuntimeProfileConfig {
            agentic: true,
            max_tool_iterations: 3,
            ..RuntimeProfileConfig::default()
        },
    );

    let mut agents = HashMap::new();
    agents.insert(
        "caller".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "caller_profile".into(),
            runtime_profile: "agentic".into(),
            delegates: vec![DelegateTargetConfig {
                agent: "target".to_string(),
                mode: DelegateExecutionMode::Bounded,
            }],
            ..AliasedAgentConfig::default()
        },
    );
    agents.insert(
        "target".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "target_profile".into(),
            runtime_profile: "agentic".into(),
            channels: vec!["telegram.prod".into()],
            ..AliasedAgentConfig::default()
        },
    );
    agents.insert(
        "peer".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "peer_profile".into(),
            runtime_profile: "agentic".into(),
            channels: vec!["telegram.prod".into()],
            ..AliasedAgentConfig::default()
        },
    );

    let mut peer_groups = HashMap::new();
    peer_groups.insert(
        "ops".to_string(),
        PeerGroupConfig {
            channel: "telegram".into(),
            agents: vec![AgentAlias::new("target"), AgentAlias::new("peer")],
            ..PeerGroupConfig::default()
        },
    );

    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    let mut config = Config {
        data_dir,
        config_path: root.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        runtime_profiles,
        peer_groups,
        ..Config::default()
    };
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    config
}

/// Nested delegation + a detached recipient turn stack futures past the
/// harness default per-thread stack — same rationale as the sibling
/// `bounded_delegate_sop_approve_ceiling.rs` file.
fn drive_relay_blocking() -> (Option<BTreeSet<String>>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_bounded_send_message_to_peer())
            .await
            .expect("chain task joins")
    })
}

/// Runs: caller delegates to target (bounded) -> target relays to peer via
/// `send_message_to_peer` -> the peer's own detached turn is captured.
/// Returns the tool set offered on the peer's own model request.
async fn drive_bounded_send_message_to_peer() -> (Option<BTreeSet<String>>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider(vec![
        native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"relay the status to the peer"}"#,
        ),
        native_tool_call(
            "send_message_to_peer",
            &serde_json::json!({
                "channel": "telegram.prod",
                "target": "peer",
                "message": format!("{PEER_MARKER} carry out the relayed work"),
            })
            .to_string(),
        ),
    ])
    .await;
    let config = peer_ceiling_config(&format!("http://{addr}"), tmp.path());

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the relay to the target agent".to_string()),
        None,
        None,
        None,
        vec![],
        false,
        None,
        None,
        zeroclaw_api::ingress::TurnOrigin::SubTurn,
        AgentRunOverrides::default(),
    )
    .await;

    // `execute()` does not join the detached peer turn. Poll for it with a
    // bounded, sleep-free yield loop, the same pattern already used for
    // detached-task synchronization in `send_message_to_peer.rs`'s own tests.
    //
    // The marker text alone is NOT enough to identify the peer's own turn:
    // `target`'s OWN follow-up request (after `send_message_to_peer` returns)
    // echoes the marker too, because it is an argument of the tool call
    // `target` itself just made, and that call re-enters `target`'s own
    // conversation history. The peer's own turn is the one whose offered
    // tools come from `peer_profile`, which never includes `delegate` or
    // `send_message_to_peer` — disjoint from every sender-side turn.
    let mut peer_turn = None;
    for _ in 0..20_000 {
        let bodies = captured.lock().await.clone();
        if let Some(body) = bodies.iter().find(|body| {
            body.contains(PEER_MARKER) && {
                let tools = offered_tools(body);
                !tools.contains("delegate") && !tools.contains("send_message_to_peer")
            }
        }) {
            peer_turn = Some(offered_tools(body));
            break;
        }
        tokio::task::yield_now().await;
    }

    let bodies = captured.lock().await.clone();
    let report = format!(
        "outcome {outcome:?}; turns={}; per-turn tools {:?}",
        bodies.len(),
        bodies.iter().map(|b| offered_tools(b)).collect::<Vec<_>>()
    );
    (peer_turn, report)
}

/// THIS TEST MUST FAIL if `send_message_to_peer_tool` stops carrying the
/// sealed ceiling into the recipient's own turn: today `execute()` calls
/// `process_message` with no `allowed_tools` at all, so the peer is always
/// assembled from its own full risk profile.
#[test]
fn a_peer_relayed_from_a_bounded_delegate_is_bound_by_the_callers_ceiling() {
    let (peer_turn, report) = drive_relay_blocking();

    let offered = peer_turn.unwrap_or_else(|| {
        panic!(
            "no model request carried the peer marker, so send_message_to_peer never \
             reached the peer's own turn and the ceiling assertion below would hold \
             vacuously. {report}"
        )
    });
    assert!(
        offered.contains(WITHIN_CEILING),
        "the peer's relayed turn offered nothing from within the ceiling, so the \
         absence of `{OUT_OF_CEILING}` proves nothing; offered={offered:?}; {report}"
    );
    assert!(
        !offered.contains(OUT_OF_CEILING),
        "a peer turn relayed from a bounded delegate target was offered \
         `{OUT_OF_CEILING}`: the target's own turn was capped to the caller's \
         ceiling, but `send_message_to_peer` handed the peer's turn the peer's own \
         FULL risk profile instead of the sealed ceiling; offered={offered:?}; {report}"
    );
}
