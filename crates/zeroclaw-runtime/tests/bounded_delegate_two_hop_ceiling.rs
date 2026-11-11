//! A bounded ceiling must survive TWO consecutive `delegate` hops.
//!
//! `bounded_delegate_transitive_ceiling.rs` exercises `delegate -> spawn_subagent`:
//! its target profile never names `delegate`, so the hop `delegate -> delegate`
//! (a bounded target that itself delegates to a second bounded target) is not
//! reached there. This file drives exactly that chain:
//!
//! ```text
//! caller --delegate(bounded)--> mid --delegate(bounded)--> leaf
//! ```
//!
//! `leaf`'s profile grants `file_write`, which neither the caller nor `mid`
//! hold, so no level may ever be offered it. The set offered while descending may
//! only narrow. The tests also guard against passing for the wrong reason: the
//! middle level must actually have been offered `delegate` and the chain must
//! have reached the leaf.
//!
//! MUST FAIL if the second hop rebuilds the leaf from its own profile instead of
//! from the ceiling the first hop carried.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, DelegateExecutionMode, DelegateTargetConfig, RiskProfileConfig,
    RuntimeProfileConfig,
};
use zeroclaw_runtime::agent::loop_::AgentRunOverrides;

/// Granted only to the leaf's profile.
const BREACH_TOOL: &str = "file_write";
/// Granted to every profile, so a ceiling that denied everything cannot pass.
const SHARED_TOOL: &str = "calculator";

#[derive(Clone)]
struct Script {
    captured: Arc<AsyncMutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-ceiling",
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
        "id": "chatcmpl-ceiling",
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

/// Request 0 (the caller) delegates to `mid`; request 1 (inside `mid`)
/// delegates to `leaf`; every later request answers plainly.
async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    match call {
        0 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"mid","prompt":"work the subtask"}"#,
        ),
        1 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"leaf","prompt":"work the second subtask"}"#,
        ),
        _ => plain_content("done"),
    }
}

async fn spawn_stub_provider() -> (SocketAddr, Arc<AsyncMutex<Vec<String>>>) {
    let script = Script {
        captured: Arc::new(AsyncMutex::new(Vec::new())),
        calls: Arc::new(AtomicUsize::new(0)),
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

/// True when the request's user message is exactly `prompt`.
fn is_user_prompt(body: &str, prompt: &str) -> bool {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    parsed
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|msgs| {
            msgs.iter().any(|m| {
                m.get("role").and_then(|r| r.as_str()) == Some("user")
                    && m.get("content").and_then(|c| c.as_str()) == Some(prompt)
            })
        })
}

/// Tool names a captured request offered the model, read off the OpenAI-style
/// `tools[].function.name` array.
fn offered_tool_names(body: &str) -> BTreeSet<String> {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return BTreeSet::new(),
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

fn profile(allowed: &[&str]) -> RiskProfileConfig {
    RiskProfileConfig {
        allowed_tools: allowed.iter().map(|n| (*n).to_string()).collect(),
        // Without this the runtime denies the `delegate` call itself ("requires
        // approval and no operator decision was available"), the chain stops at
        // `mid`, and the leaf is never reached.
        auto_approve: allowed.iter().map(|n| (*n).to_string()).collect(),
        delegation_policy: DelegationPolicy {
            mode: DelegationMode::Allow,
        },
        ..RiskProfileConfig::default()
    }
}

fn agent(risk_profile: &str, delegates: &[&str]) -> AliasedAgentConfig {
    AliasedAgentConfig {
        enabled: true,
        model_provider: "custom.default".into(),
        risk_profile: risk_profile.into(),
        runtime_profile: "agentic".into(),
        delegates: delegates
            .iter()
            .map(|name| DelegateTargetConfig {
                agent: (*name).to_string(),
                mode: DelegateExecutionMode::Bounded,
            })
            .collect(),
        ..AliasedAgentConfig::default()
    }
}

fn chain_config(provider_uri: &str, root: &std::path::Path) -> Config {
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
    let mut risk_profiles = HashMap::new();
    risk_profiles.insert(
        "caller_profile".to_string(),
        profile(&["delegate", SHARED_TOOL]),
    );
    risk_profiles.insert(
        "mid_profile".to_string(),
        profile(&["delegate", SHARED_TOOL]),
    );
    risk_profiles.insert(
        "leaf_profile".to_string(),
        profile(&[BREACH_TOOL, SHARED_TOOL, "delegate"]),
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
    agents.insert("caller".to_string(), agent("caller_profile", &["mid"]));
    agents.insert("mid".to_string(), agent("mid_profile", &["leaf"]));
    agents.insert("leaf".to_string(), agent("leaf_profile", &[]));
    let mut config = Config {
        data_dir: root.to_path_buf(),
        config_path: root.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        runtime_profiles,
        ..Config::default()
    };
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    config
}

async fn drive_chain() -> Vec<BTreeSet<String>> {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider().await;
    let config = chain_config(&format!("http://{addr}"), tmp.path());
    let _ = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the subtask to the mid agent".to_string()),
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
    let bodies = captured.lock().await;
    // The leaf's request is the one whose user message is the prompt `mid` handed
    // it. A request count cannot stand in for this: after a refused delegation
    // `mid` makes another request of its own, which looks like a third level.
    let leaf = bodies
        .iter()
        .position(|body| is_user_prompt(body, "work the second subtask"))
        .unwrap_or_else(|| {
            panic!(
                "the chain never reached the leaf: {} model requests, none carrying the \
                 leaf's prompt",
                bodies.len()
            )
        });
    [0, 1, leaf]
        .iter()
        .map(|&i| offered_tool_names(&bodies[i]))
        .collect()
}

/// Nested delegation stacks large futures; run on a worker with a raised stack
/// (same reason as `bounded_delegate_transitive_ceiling.rs`).
fn drive_chain_blocking() -> Vec<BTreeSet<String>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async move {
        zeroclaw_spawn::spawn!(async move { drive_chain().await })
            .await
            .expect("chain task joins")
    })
}

/// `drive_chain` returns exactly the caller's, `mid`'s and the leaf's requests, in
/// that order, and panics when the leaf's prompt never reaches the provider.
const DESCENT_LEVELS: usize = 3;

fn descent() -> Vec<BTreeSet<String>> {
    let levels = drive_chain_blocking();
    assert!(
        levels[1].contains("delegate"),
        "the middle level was never offered `delegate`, so the second hop is not what \
         this file exercises; sets {levels:?}"
    );
    levels
}

#[test]
fn a_second_delegate_hop_cannot_restore_a_tool_neither_caller_nor_mid_had() {
    let levels = descent();
    let breached: Vec<usize> = levels
        .iter()
        .enumerate()
        .filter(|(_, names)| names.contains(BREACH_TOOL))
        .map(|(index, _)| index)
        .collect();
    assert!(
        breached.is_empty(),
        "levels {breached:?} were offered `{BREACH_TOOL}`, which only the leaf's own profile \
         grants; sets {levels:?}"
    );
}

#[test]
fn the_offered_tool_set_never_grows_across_two_delegate_hops() {
    let levels = descent();
    for step in 1..DESCENT_LEVELS {
        let grown: Vec<&String> = levels[step].difference(&levels[step - 1]).collect();
        assert!(
            grown.is_empty(),
            "descending from level {} to {step} ADDED {grown:?}; a ceiling may only narrow. \
             sets {levels:?}",
            step - 1
        );
    }
}

#[test]
fn the_leaf_still_receives_the_tool_every_level_grants() {
    // Guard against a vacuous pass: a ceiling that emptied the leaf would satisfy
    // every absence assertion above.
    let levels = descent();
    assert!(
        levels[DESCENT_LEVELS - 1].contains(SHARED_TOOL),
        "the leaf lost `{SHARED_TOOL}`, which every profile grants; sets {levels:?}"
    );
}
