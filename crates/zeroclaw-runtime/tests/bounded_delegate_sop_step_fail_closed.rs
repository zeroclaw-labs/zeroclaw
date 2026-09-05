//! A SOP step that names a DIFFERENT agent must be refused - not silently run
//! with the surrounding agent's context - when the executing run path cannot
//! re-assemble that agent's own execution surface.
//!
//! A bounded delegate sub-loop is exactly such a path: it drives its own tool
//! loop without a per-agent re-assembly handle, and `sop_execute` is reusable
//! across a bounded boundary, so the target can hold a live SOP tool. Running
//! a cross-agent step there would borrow the delegate agent's tools, policy and
//! MCP scope for an agent that was never assembled - the escalation the whole
//! per-agent seam exists to prevent.
//!
//! The refusal is the ONLY thing standing between that path and the
//! escalation, and nothing pinned it: the refusal text existed solely at its
//! own definition site. THIS TEST MUST FAIL if the bounded sub-loop starts
//! supplying a re-assembly handle it cannot honour, or if the branch that
//! currently refuses is changed to fall back to the surrounding agent's
//! context instead. Neutralize it by making that branch reuse the parent
//! context rather than raising, and this test must go red.
//!
//! Note what this does NOT assert: it does not verify that a ceiling is
//! applied to a cross-agent step, only that the path refuses to run one
//! without the means to isolate it. Those are different properties.

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

/// Agent named by the SOP step, distinct from both the caller and the bounded
/// target, so the step can only run by re-assembling a third execution surface.
const STEP_AGENT: &str = "stepagent";

const SOP_NAME: &str = "crossagenthandoff";

/// Fragment of the refusal raised when a run path cannot isolate a cross-agent
/// step. Matched as a substring so wording may evolve without silently
/// weakening the assertion into a tautology.
const REFUSAL_MARKER: &str = "no per-agent re-assembly handle";

#[derive(Clone)]
struct Script {
    captured: Arc<AsyncMutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-sop",
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
        "id": "chatcmpl-sop",
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

/// Caller delegates once; the bounded target then starts the SOP. Everything
/// after answers plainly so both loops unwind.
async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    match call {
        0 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"start the procedure"}"#,
        ),
        1 => native_tool_call("sop_execute", &format!(r#"{{"name":"{SOP_NAME}"}}"#)),
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
# Supervised parks before step 1 for approval, which would retire the run
# before the cross-agent handoff is ever reached.
execution_mode = "auto"

[[triggers]]
type = "manual"

[[steps]]
number = 1
title = "handoff"
body = "carry out the handed-off work"
agent = "{STEP_AGENT}"
"#
    );
    std::fs::write(sop_dir.join("SOP.toml"), manifest).expect("sop manifest");
}

fn sop_handoff_config(provider_uri: &str, root: &std::path::Path) -> Config {
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
        permissive(vec!["delegate".to_string(), "sop_execute".to_string()]),
    );
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec!["sop_execute".to_string()]),
    );
    risk_profiles.insert("step_profile".to_string(), permissive(vec![]));

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
    // Configured and reachable, so the refusal cannot be mistaken for "the
    // step named an agent that does not exist".
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

    // The data directory is scanned for the recorded refusal, so it must NOT
    // contain the SOP source - the manifest names the step agent and would
    // satisfy the specificity assertion without the step ever running.
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

/// Recursively collects every file under `dir`, so the assertion can look for
/// the refusal wherever the run recorded it.
fn files_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// A refused step never reaches the model: it is recorded as a failed
/// `SopStepResult` through the SOP audit trail, which the agent's own memory
/// backend persists under the data directory. That store is the observation
/// channel - reading the provider transcript would miss it entirely.
fn data_dir_contains(root: &std::path::Path, needle: &str) -> (bool, Vec<String>) {
    let mut files = Vec::new();
    files_under(root, &mut files);
    let mut seen = Vec::new();
    let mut found = false;
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let hit = String::from_utf8_lossy(&bytes).contains(needle);
        found |= hit;
        seen.push(format!(
            "{} ({} bytes){}",
            path.display(),
            bytes.len(),
            if hit { " <- MATCH" } else { "" }
        ));
    }
    (found, seen)
}

async fn drive_delegated_sop() -> (bool, bool, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider().await;
    let config = sop_handoff_config(&format!("http://{addr}"), tmp.path());

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the procedure to the target agent".to_string()),
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
    let data_dir = tmp.path().join("data");
    let (refused, scanned) = data_dir_contains(&data_dir, REFUSAL_MARKER);
    let (named_agent, _) = data_dir_contains(&data_dir, STEP_AGENT);
    let report = format!(
        "outcome {outcome:?}; refusal_recorded={refused}; step_agent_named={named_agent}; last body {:?}; files {scanned:?}",
        bodies.last()
    );
    (refused, named_agent, report)
}

/// Nested delegation plus a SOP sub-turn stacks futures well past the test
/// harness default per-thread stack.
fn drive_blocking() -> (bool, bool, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_delegated_sop())
            .await
            .expect("chain task joins")
    })
}

#[test]
fn a_cross_agent_sop_step_is_refused_inside_a_bounded_delegate_sub_loop() {
    let (refused, named_agent, report) = drive_blocking();

    assert!(
        refused,
        "the cross-agent SOP step was not refused inside the bounded sub-loop; {report}"
    );
    // Specificity: a generic failure would satisfy the check above only by
    // accident. The refusal must name the agent it declined to assemble.
    assert!(
        named_agent,
        "the refusal did not name the step agent, so it may be an unrelated failure; {report}"
    );
}
