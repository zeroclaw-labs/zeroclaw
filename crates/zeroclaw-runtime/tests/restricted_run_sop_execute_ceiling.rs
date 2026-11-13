//! `sop_execute`'s park-cancellation guard, reached through the registry
//! factory of a RESTRICTED `agent::run`, not through a bounded delegate.
//!
//! `agent::run` seals a ceiling from its `allowed_tools` and builds the
//! registry with it. That one entry point is what a `spawn_subagent` child
//! and a cron-fired job both go through, so a `sop_execute` built without the
//! ceiling there parks a Supervised run for an external resume that no
//! ceiling follows. The sibling test `bounded_delegate_sop_run_park_ceiling`
//! covers the bounded-delegate rebuild; this one covers the factory.
//!
//! THIS TEST MUST FAIL if `all_tools_with_runtime` stops handing the ceiling
//! to `SopExecuteTool`: the restricted run below would then park the run
//! instead of refusing and cancelling it. The unrestricted control proves the
//! same chain really does park when no ceiling is in force, so the refusal is
//! the ceiling and not a SOP that cannot start.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
};
use zeroclaw_runtime::agent::loop_::AgentRunOverrides;

const SOP_NAME: &str = "restrictedrunpark";
const WITHIN_CEILING: &str = "calculator";

#[derive(Clone)]
struct Script {
    calls: Arc<AtomicUsize>,
    captured: Arc<AsyncMutex<Vec<String>>>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-soppark",
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
        "id": "chatcmpl-soppark",
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

/// 0: the agent calls `sop_execute`. Everything after answers plainly.
async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    match call {
        0 => native_tool_call("sop_execute", &format!(r#"{{"name":"{SOP_NAME}"}}"#)),
        _ => plain_content("done"),
    }
}

async fn spawn_stub_provider() -> (SocketAddr, Arc<AsyncMutex<Vec<String>>>) {
    let script = Script {
        calls: Arc::new(AtomicUsize::new(0)),
        captured: Arc::new(AsyncMutex::new(Vec::new())),
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

/// Content of every `role: "tool"` message in a captured request body, in
/// order. A tool result lands in the conversation history of the NEXT request
/// after the call that produced it, so the refusal text is looked for there,
/// not in the request that issued the call.
fn tool_result_texts(body: &str) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    parsed
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|messages| {
            messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("tool"))
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A `Supervised` SOP: `start_run` parks it on `WaitApproval` immediately, no
/// second step or approval plumbing needed to reach the mechanism under test.
fn plant_supervised_sop(sops_dir: &std::path::Path) {
    let sop_dir = sops_dir.join(SOP_NAME);
    std::fs::create_dir_all(&sop_dir).expect("sop dir");
    let manifest = format!(
        r#"
[sop]
name = "{SOP_NAME}"
description = "parks for approval before its only step"
version = "1.0.0"
execution_mode = "supervised"

[[triggers]]
type = "manual"

[[steps]]
number = 1
title = "only step"
body = "should never be reached by a bounded caller"
"#
    );
    std::fs::write(sop_dir.join("SOP.toml"), manifest).expect("sop manifest");
}

fn restricted_run_config(provider_uri: &str, root: &std::path::Path) -> Config {
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

    // The profile auto-approves the tools it allows, so nothing prompts for
    // approval; that grants no tool the profile does not already list.
    let tools = vec!["sop_execute".to_string(), WITHIN_CEILING.to_string()];
    let mut risk_profiles = HashMap::new();
    risk_profiles.insert(
        "agent_profile".to_string(),
        RiskProfileConfig {
            auto_approve: tools.clone(),
            allowed_tools: tools,
            delegation_policy: DelegationPolicy {
                mode: DelegationMode::Allow,
            },
            ..RiskProfileConfig::default()
        },
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
        "agent".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "agent_profile".into(),
            runtime_profile: "agentic".into(),
            ..AliasedAgentConfig::default()
        },
    );

    let sops_dir = root.join("sops");
    plant_supervised_sop(&sops_dir);
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    let mut config = Config {
        data_dir,
        config_path: root.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        runtime_profiles,
        ..Config::default()
    };
    config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    config
}

/// Runs the agent once with the given `allowed_tools` and returns the tool
/// results it saw, plus a report for assertion failures.
async fn drive_chain(allowed_tools: Option<Vec<String>>) -> (Vec<String>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider().await;
    let config = restricted_run_config(&format!("http://{addr}"), tmp.path());

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "agent",
        Some("run the procedure".to_string()),
        None,
        None,
        None,
        vec![],
        false,
        None,
        allowed_tools,
        zeroclaw_api::ingress::TurnOrigin::SubTurn,
        AgentRunOverrides::default(),
    )
    .await;

    let bodies = captured.lock().await.clone();
    // Every request repeats the history before it; the one carrying the most
    // tool results is the last, so it holds each result once, in order.
    let results: Vec<String> = bodies
        .iter()
        .map(|b| tool_result_texts(b))
        .max_by_key(Vec::len)
        .unwrap_or_default();
    let report = format!(
        "outcome {outcome:?}; turns={}; tool results={results:?}",
        bodies.len()
    );
    (results, report)
}

fn drive_blocking(allowed_tools: Option<Vec<String>>) -> (Vec<String>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_chain(allowed_tools))
            .await
            .expect("chain task joins")
    })
}

#[test]
fn a_restricted_runs_sop_execute_refuses_a_parking_run() {
    let (results, report) = drive_blocking(Some(vec![
        "sop_execute".to_string(),
        WITHIN_CEILING.to_string(),
    ]));

    // Positive half first: `sop_execute` must have been called and returned a
    // result, so the negative assertion cannot hold for a chain that never
    // reached the tool.
    assert!(
        !results.is_empty(),
        "sop_execute produced no tool result at all; {report}"
    );
    assert!(
        results[0].contains("refused") && results[0].contains("cancelled"),
        "a restricted run's sop_execute on a Supervised SOP must be refused and the run \
         cancelled, not left parked; got: {}; {report}",
        results[0]
    );
}

#[test]
fn an_unrestricted_runs_sop_execute_still_parks_the_run() {
    // Control: with no `allowed_tools` there is no ceiling, and the same call
    // starts the run and leaves it waiting, as it always did.
    let (results, report) = drive_blocking(None);

    assert!(
        !results.is_empty(),
        "sop_execute produced no tool result at all; {report}"
    );
    assert!(
        !results[0].contains("cancelled") && results[0].contains("waiting for approval"),
        "without a ceiling the run must start and wait for approval, not be cancelled; \
         got: {}; {report}",
        results[0]
    );
}
