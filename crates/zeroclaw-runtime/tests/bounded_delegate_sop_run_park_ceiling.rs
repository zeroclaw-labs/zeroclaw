//! `sop_execute`'s park-cancellation guard, reached through a REAL `Bounded`
//! delegate assembly — not through a `SopExecuteTool` built directly with a
//! ceiling already set, the way every unit test in `tools::sop_execute`/
//! `tools::sop_advance` reaches it.
//!
//! `sop_execute` is `SAFE_FOR_BOUNDED_REUSE`: `delegate.rs`'s `Bounded`
//! assembly downcasts the caller's own instance and calls
//! `rebound_with_ceiling` on it (mirroring `McpToolWrapper::rebound`) instead
//! of reusing it unmodified. That downcast, and the sealed ceiling it passes
//! along, are exercised by NOTHING else in this crate's test suite: the 8
//! pre-existing `bounded_delegate_*` integration binaries never offer
//! `sop_execute`/`sop_advance` to a bounded target and drive it into a park,
//! and the unit tests construct the tool directly with
//! `.with_caller_ceiling(Some(..))`, which proves the guard's own logic but
//! not that `delegate.rs` ever wires a real ceiling into it.
//!
//! THIS TEST MUST FAIL if the `downcast_ref::<SopExecuteTool>()` branch in
//! `delegate.rs`'s `Bounded` filter_map is removed (or its `rebound_with_
//! ceiling` call dropped) — `sop_execute` would then fall through to the
//! plain `SAFE_FOR_BOUNDED_REUSE` reuse path, i.e. `tool.clone()` with NO
//! ceiling attached, and the run below would park normally instead of being
//! refused and cancelled.

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

const SOP_NAME: &str = "boundedparkceiling";
/// The bounded target's tool ceiling includes `sop_execute` but not `delegate`
/// itself — irrelevant to the mechanism, present only so the target's turn has
/// something else to fall back on if `sop_execute` were silently omitted
/// instead of refused (that failure mode would otherwise look identical to a
/// model that just chose not to call it).
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

/// 0: caller delegates to the bounded target. 1: the target calls
/// `sop_execute`. Everything after answers plainly so both turns unwind.
async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    match call {
        0 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"run the procedure"}"#,
        ),
        1 => native_tool_call("sop_execute", &format!(r#"{{"name":"{SOP_NAME}"}}"#)),
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

fn sop_park_ceiling_config(provider_uri: &str, root: &std::path::Path) -> Config {
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
    // CORRECTED, verified by running this test before the fix below: `sop_execute`
    // reaching a bounded target is gated TWICE, not once. `bounded_base_tools`
    // (`delegate.rs`) is `self.parent_tools` (the CALLER's full registry)
    // filtered by `Self::delegate_admits_with_mcp(&tool_policy, tool.name())` —
    // the TARGET's own policy — before `SAFE_FOR_BOUNDED_REUSE` ever runs on the
    // survivors. A target profile that omits `sop_execute` never makes it into
    // `bounded_base_tools` at all, so the `downcast_ref::<SopExecuteTool>()`
    // branch this test exists to exercise never sees it either. Both profiles
    // must list it, matching `bounded_delegate_sop_step_ceiling.rs`'s
    // `target_profile`, which already does this for the same reason.
    risk_profiles.insert(
        "caller_profile".to_string(),
        permissive(vec![
            "delegate".to_string(),
            "sop_execute".to_string(),
            WITHIN_CEILING.to_string(),
        ]),
    );
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec!["sop_execute".to_string(), WITHIN_CEILING.to_string()]),
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

/// Returns every tool-result text observed across the whole chain, plus a
/// report for assertion failures.
async fn drive_chain() -> (Vec<String>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider().await;
    let config = sop_park_ceiling_config(&format!("http://{addr}"), tmp.path());

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
    let results: Vec<String> = bodies.iter().flat_map(|b| tool_result_texts(b)).collect();
    let report = format!(
        "outcome {outcome:?}; turns={}; tool results={results:?}",
        bodies.len()
    );
    (results, report)
}

/// Two stacked loops (caller -> delegate -> target's `sop_execute` turn)
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
fn a_bounded_targets_sop_execute_refuses_a_parking_run_through_the_real_delegate_assembly() {
    let (results, report) = drive_blocking();

    // Positive half first: `sop_execute` must actually have been called and
    // returned SOME tool result. Without this the negative assertion below
    // would hold vacuously for a chain that never reached the tool at all
    // (e.g. because delegate.rs's rebind wiring silently omitted it).
    assert!(
        !results.is_empty(),
        "sop_execute produced no tool result at all — either the model never called \
         it or delegate.rs's Bounded assembly omitted it entirely; {report}"
    );

    // `sop_execute` is the only tool this chain calls before both turns settle
    // on a plain reply, so its result is the first one captured. Not matched by
    // name: a SUCCESS result ("SOP run started: ...") never mentions the tool
    // name at all — only the refusal text does — so a name-based lookup would
    // silently pass on a SUCCESS result that happens not to be found, which is
    // exactly the failure mode this test exists to catch. Found by the red
    // check itself: the first version of this assertion used `.find(|text|
    // text.contains("sop_execute"))` and panicked on "not found" against a
    // genuinely parked run, for the wrong reason.
    let sop_result = &results[0];

    // Negative half: the real, wired-through ceiling must have refused the
    // parking run rather than let the tool report success.
    assert!(
        sop_result.contains("refused") && sop_result.contains("cancelled"),
        "a bounded target's sop_execute call on a Supervised SOP must be refused and \
         the run cancelled, not left parked; got: {sop_result}; {report}"
    );
}
