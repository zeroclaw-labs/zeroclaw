//! A cross-agent SOP step reached from inside a bounded delegation must run
//! under the caller's tool ceiling, not the step agent's own risk profile.
//!
//! The chain:
//!
//! ```text
//! caller --delegate(bounded)--> target --spawn_subagent--> child --sop_execute--> step agent
//! ```
//!
//! The direct bounded sub-loop has no per-agent re-assembly handle, so it
//! refuses a cross-agent step outright — that refusal is pinned by
//! `bounded_delegate_sop_step_fail_closed`, whose own header notes what it does
//! NOT assert: "it does not verify that a ceiling is applied to a cross-agent
//! step, only that the path refuses to run one without the means to isolate
//! it." This test covers exactly that gap.
//!
//! The spawned child DOES carry a re-assembly handle, so the step runs. It is
//! assembled by `assemble_owned_execution`, which built the step agent's
//! registry from the step agent's OWN policy. With the caller ceiling not
//! forwarded, a step agent whose profile grants a tool the caller never held
//! receives it, and the bounded chain executes it.
//!
//! THIS TEST MUST FAIL if `SopStepReassembly` stops carrying `caller_allowed`,
//! or if `assemble_owned_execution` goes back to passing `caller_allowed: None`
//! to the assembly. Neutralize either and the step agent's turn regains
//! `BEYOND_CEILING`.
//!
//! Both halves are asserted against the SAME turn — the step agent's own model
//! request — because a chain that never reached the step would satisfy a bare
//! "the tool is absent" check trivially:
//!   - positive: that turn must exist at all, and must offer `WITHIN_CEILING`;
//!   - negative: it must not offer `BEYOND_CEILING`.

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

/// Granted by the step agent's own risk profile and by nobody upstream. The
/// caller never held it, so the bounded chain must not gain it.
const BEYOND_CEILING: &str = "file_write";

/// Held by the caller and by the step agent, so the step's turn has something
/// to offer once the ceiling is applied. Without this the negative assertion
/// would also hold against a step that received no tools at all.
const WITHIN_CEILING: &str = "calculator";

const STEP_AGENT: &str = "stepagent";
const SOP_NAME: &str = "boundedstepceiling";
/// Distinctive text planted in the step body, used to find the step agent's own
/// model request among the captured turns.
const STEP_MARKER: &str = "ZC-STEP-CEILING-MARKER";

#[derive(Clone)]
struct Script {
    calls: Arc<AtomicUsize>,
    captured: Arc<AsyncMutex<Vec<String>>>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-sopceiling",
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
        "id": "chatcmpl-sopceiling",
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

/// 0: caller delegates. 1: the bounded target spawns a child. 2: the child
/// starts the SOP, whose only step names a third agent. Everything after
/// answers plainly so all four loops unwind.
async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    match call {
        0 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"carry this through a subagent"}"#,
        ),
        1 => native_tool_call("spawn_subagent", r#"{"prompt":"start the procedure"}"#),
        2 => native_tool_call("sop_execute", &format!(r#"{{"name":"{SOP_NAME}"}}"#)),
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

/// A one-step procedure whose only step hands off to a third agent.
fn plant_cross_agent_sop(sops_dir: &std::path::Path) {
    let sop_dir = sops_dir.join(SOP_NAME);
    std::fs::create_dir_all(&sop_dir).expect("sop dir");
    let manifest = format!(
        r#"
[sop]
name = "{SOP_NAME}"
description = "hands its only step to a different agent"
version = "1.0.0"
# Supervised would park before step 1 for approval and retire the run before
# the handoff is ever reached.
execution_mode = "auto"

[[triggers]]
type = "manual"

[[steps]]
number = 1
title = "handoff"
body = "{STEP_MARKER} carry out the handed-off work"
agent = "{STEP_AGENT}"
"#
    );
    std::fs::write(sop_dir.join("SOP.toml"), manifest).expect("sop manifest");
}

fn sop_ceiling_config(provider_uri: &str, root: &std::path::Path) -> Config {
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
    // The ceiling: the caller can delegate, spawn and run SOPs, and holds the
    // shared tool. It has no `file_write`.
    risk_profiles.insert(
        "caller_profile".to_string(),
        permissive(vec![
            "delegate".to_string(),
            "spawn_subagent".to_string(),
            "sop_execute".to_string(),
            WITHIN_CEILING.to_string(),
        ]),
    );
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec![
            "spawn_subagent".to_string(),
            "sop_execute".to_string(),
            WITHIN_CEILING.to_string(),
        ]),
    );
    // The step agent's OWN profile grants the tool the caller never had. This is
    // what an unbounded re-assembly would hand it.
    risk_profiles.insert(
        "step_profile".to_string(),
        permissive(vec![WITHIN_CEILING.to_string(), BEYOND_CEILING.to_string()]),
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
    agents.insert(
        STEP_AGENT.to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "step_profile".into(),
            runtime_profile: "agentic".into(),
            ..AliasedAgentConfig::default()
        },
    );

    let sops_dir = root.join("sops");
    plant_cross_agent_sop(&sops_dir);

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

/// Returns the tool set offered on the step agent's own model request, if that
/// turn happened at all, plus a report for assertion failures.
async fn drive_chain() -> (Option<BTreeSet<String>>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider().await;
    let config = sop_ceiling_config(&format!("http://{addr}"), tmp.path());

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand this to the target agent".to_string()),
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
    // The step agent's turn is the one carrying the planted step body. Matching
    // on that marker rather than on turn ordering keeps the observation stable
    // if the chain grows an extra model round-trip.
    let step_turn = bodies
        .iter()
        .find(|body| body.contains(STEP_MARKER))
        .map(|body| offered_tools(body));
    let report = format!(
        "outcome {outcome:?}; turns={}; per-turn tools {:?}",
        bodies.len(),
        bodies.iter().map(|b| offered_tools(b)).collect::<Vec<_>>()
    );
    (step_turn, report)
}

/// Four stacked loops (caller -> delegate -> spawn_subagent -> SOP sub-turn)
/// overflow the harness default per-thread stack.
fn drive_blocking() -> (Option<BTreeSet<String>>, String) {
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
fn a_cross_agent_sop_step_under_a_bounded_chain_runs_within_the_caller_ceiling() {
    let (step_turn, report) = drive_blocking();

    // Positive half first: the step must actually have run. Without this the
    // negative assertion would pass for a chain that never reached the step.
    let offered = step_turn.unwrap_or_else(|| {
        panic!(
            "no model request carried the planted step body, so the cross-agent SOP \
             step never ran and the ceiling assertion below would hold vacuously. \
             {report}"
        )
    });
    assert!(
        offered.contains(WITHIN_CEILING),
        "the step agent's turn offered no tool at all from within the ceiling, so the \
         absence of `{BEYOND_CEILING}` proves nothing; offered={offered:?}; {report}"
    );

    // Negative half: the step agent's own profile grants `BEYOND_CEILING`; the
    // inherited ceiling must have removed it.
    assert!(
        !offered.contains(BEYOND_CEILING),
        "a cross-agent SOP step reached from a bounded chain was offered \
         `{BEYOND_CEILING}`, which the caller was never granted: the ceiling did not \
         survive the step re-assembly; offered={offered:?}; {report}"
    );
}
