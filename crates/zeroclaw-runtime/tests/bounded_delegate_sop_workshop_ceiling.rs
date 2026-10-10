//! `sop_workshop`'s ceiling refusal, reached through a REAL `Bounded` delegate
//! assembly rather than a `SopWorkshopTool` built directly with a ceiling.
//!
//! `sop_workshop` persists state a later run consumes outside the caller's
//! tool ceiling (`apply` writes a SOP definition and reloads the shared
//! engine; `propose`, `capture_run`, `reject` and `quarantine` save proposals).
//! `delegate.rs`'s `Bounded` assembly therefore downcasts the caller's
//! instance and calls `rebound_with_ceiling` on it, and the rebound tool
//! refuses everything except `list` and `inspect`.
//!
//! THIS TEST MUST FAIL if that downcast branch is removed or if the refusal in
//! `sop_workshop.rs` is dropped: with the refusal gone `propose` below would
//! succeed and return the stored proposal instead of the refusal text. The
//! `list` call that follows is the control - it must still work, so a target
//! that merely lost the tool entirely cannot pass by accident.

use std::collections::HashMap;
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

const SOP_NAME: &str = "workshopceiling";
const WITHIN_CEILING: &str = "calculator";
const REFUSAL: &str = "not available under a caller tool ceiling";

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

/// 0: caller delegates to the bounded target. 1: the target proposes a SOP
/// (a persisting action, with every argument a real proposal needs, so an
/// unbounded call would succeed). 2: the target lists proposals (read-only
/// control). Everything after answers plainly so both turns unwind.
async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    match call {
        0 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"record a procedure"}"#,
        ),
        1 => native_tool_call(
            "sop_workshop",
            r#"{"action":"propose","sop_name":"leftbehind","description":"outlives the turn","procedure_markdown":"leftbehind procedure"}"#,
        ),
        2 => native_tool_call("sop_workshop", r#"{"action":"list"}"#),
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

fn workshop_ceiling_config(provider_uri: &str, root: &std::path::Path) -> Config {
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

    // A bounded child has no operator to answer an approval prompt, so a
    // tool its profile would prompt for is denied before it runs. These
    // regressions are about the ceiling, so each profile auto-approves the
    // tools it allows; that grants no tool the profile does not already list.
    let permissive = |tools: Vec<String>| RiskProfileConfig {
        auto_approve: tools.clone(),
        allowed_tools: tools,
        delegation_policy: DelegationPolicy {
            mode: DelegationMode::Allow,
        },
        ..RiskProfileConfig::default()
    };

    let mut risk_profiles = HashMap::new();
    // Both profiles must list `sop_workshop`: `bounded_base_tools` filters the
    // caller's registry by the TARGET's own policy before any rebinding runs.
    risk_profiles.insert(
        "caller_profile".to_string(),
        permissive(vec![
            "delegate".to_string(),
            "sop_workshop".to_string(),
            WITHIN_CEILING.to_string(),
        ]),
    );
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec!["sop_workshop".to_string(), WITHIN_CEILING.to_string()]),
    );

    let mut runtime_profiles = HashMap::new();
    runtime_profiles.insert(
        "agentic".to_string(),
        RuntimeProfileConfig {
            agentic: true,
            max_tool_iterations: 6,
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
    config.sop.procedural_memory_enabled = true;
    config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    config
}

/// Returns every tool-result text observed across the whole chain, plus a
/// report for assertion failures.
async fn drive_chain() -> (Vec<String>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider().await;
    let config = workshop_ceiling_config(&format!("http://{addr}"), tmp.path());

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("delegate the procedure to the target".to_string()),
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

    let bodies = captured.lock().await.clone();
    // Every request repeats the history before it, so flattening them would
    // list the first result again after the second. The request carrying the
    // most tool results is the target's last one: `[propose, list]`, in order.
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

/// Two stacked loops (caller -> delegate -> target's `sop_workshop` turn)
/// overflow the harness default per-thread stack, the same reason the other
/// `bounded_delegate_sop_*` integration tests in this crate run on a runtime
/// with an explicit 64 MB stack.
fn drive_blocking() -> (Vec<String>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_chain())
            .await
            .expect("chain task joins")
    })
}

#[test]
fn a_bounded_targets_sop_workshop_refuses_persisting_actions_but_keeps_list() {
    let (results, report) = drive_blocking();

    assert!(
        results.len() >= 2,
        "the chain must reach both `sop_workshop` calls (propose, then list) — either          the model never called them or delegate.rs's Bounded assembly omitted the          tool; {report}"
    );

    // Negative half: the persisting action is refused with the ceiling text.
    assert!(
        results[0].contains(REFUSAL),
        "a bounded target's sop_workshop `propose` must be refused under the caller          ceiling, not stored; got: {}; {report}",
        results[0]
    );

    // Positive control: the read-only action still runs on the same instance, so
    // the refusal above is the per-action rule and not a missing tool.
    assert!(
        !results[1].contains(REFUSAL) && !results[1].to_lowercase().contains("unknown tool"),
        "`list` must stay available to a bounded target; got: {}; {report}",
        results[1]
    );
}
