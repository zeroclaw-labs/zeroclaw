//! The working directory a bounded target's prompt describes must be the one
//! its file tools actually resolve relative paths against.
//!
//! These are two independently correct facts that contradict each other when
//! combined. A bounded target whose risk profile matches the caller's keeps the
//! CALLER's session workspace on its execution policy - deliberately, so a
//! same-profile hand-off stays inside one session. The bounded prompt, equally
//! deliberately, describes the TARGET's own configured workspace, so the model
//! is told about the skills and layout that belong to the agent it is running
//! as. Each decision is defensible alone; together the model is told it works
//! in one directory while its writes land in another.
//!
//! Rather than assert either fact, these tests assert the property that must
//! hold whichever way the conflict is resolved: the directory NAMED in the
//! prompt is the directory WRITTEN to. That statement survives a refactor of
//! either side; "the prompt uses the target's configured workspace" would not.
//!
//! The observation is end to end on purpose: the prompt is read off the
//! captured provider request, and the write is located by looking for the file
//! on disk. Nothing here inspects a policy field, so a fix that changes which
//! workspace is authoritative still has to make the two agree.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_config::autonomy::{AutonomyLevel, DelegationMode, DelegationPolicy};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, DelegateExecutionMode, DelegateTargetConfig, RiskProfileConfig,
    RuntimeProfileConfig,
};
use zeroclaw_runtime::agent::loop_::AgentRunOverrides;

/// Written by the delegated target with a RELATIVE path, so where it lands is
/// decided entirely by the workspace its file tools resolve against.
const PROBE_FILE: &str = "workspace-probe.txt";

#[derive(Clone)]
struct Script {
    captured: Arc<AsyncMutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-ws",
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
        "id": "chatcmpl-ws",
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
    match call {
        0 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"write the probe file"}"#,
        ),
        1 => native_tool_call(
            "file_write",
            &format!(r#"{{"path":"{PROBE_FILE}","content":"probe"}}"#),
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

/// The `Working directory:` line the system prompt states, unquoted.
fn prompt_working_directory(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let system = parsed
        .get("messages")?
        .as_array()?
        .iter()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))?
        .get("content")?
        .as_str()?;
    let after = system.split("Working directory: `").nth(1)?;
    Some(after.split('`').next()?.to_string())
}

fn coherence_config(
    provider_uri: &str,
    root: &Path,
    same_profile: bool,
) -> (Config, PathBuf, PathBuf) {
    let caller_workspace = root.join("caller-workspace");
    let target_workspace = root.join("target-workspace");
    std::fs::create_dir_all(&caller_workspace).expect("caller workspace");
    std::fs::create_dir_all(&target_workspace).expect("target workspace");

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

    // Both profiles grant `file_write`, so the ceiling never becomes the
    // reason the probe fails to be written - the only variable under test is
    // WHICH workspace the write resolves against.
    let profile = || RiskProfileConfig {
        level: AutonomyLevel::Full,
        allowed_tools: vec!["delegate".to_string(), "file_write".to_string()],
        delegation_policy: DelegationPolicy {
            mode: DelegationMode::Allow,
        },
        ..RiskProfileConfig::default()
    };

    // Same-profile is the case that was never exercised: it is the branch where
    // the target's execution policy deliberately keeps the caller's workspace.
    let caller_profile_name = "shared_profile";
    let target_profile_name = if same_profile {
        "shared_profile"
    } else {
        "target_profile"
    };

    let mut risk_profiles = HashMap::new();
    risk_profiles.insert(caller_profile_name.to_string(), profile());
    risk_profiles.insert(target_profile_name.to_string(), profile());

    let mut runtime_profiles = HashMap::new();
    runtime_profiles.insert(
        "agentic".to_string(),
        RuntimeProfileConfig {
            agentic: true,
            max_tool_iterations: 3,
            ..RuntimeProfileConfig::default()
        },
    );

    let workspace_at = |path: &PathBuf| zeroclaw_config::multi_agent::AgentWorkspaceConfig {
        path: Some(path.clone()),
        ..Default::default()
    };

    let mut agents = HashMap::new();
    agents.insert(
        "caller".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: caller_profile_name.into(),
            runtime_profile: "agentic".into(),
            workspace: workspace_at(&caller_workspace),
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
            risk_profile: target_profile_name.into(),
            runtime_profile: "agentic".into(),
            workspace: workspace_at(&target_workspace),
            ..AliasedAgentConfig::default()
        },
    );

    let mut config = Config {
        data_dir: root.join("data"),
        config_path: root.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        runtime_profiles,
        ..Config::default()
    };
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    (config, caller_workspace, target_workspace)
}

struct Observation {
    /// Kept alive for the whole assertion. Dropping it deletes both
    /// workspaces, and then `canonicalize` fails on BOTH sides of the
    /// comparison, which compares equal and passes while proving nothing.
    _root: TempDir,
    /// Directory the delegated turn's prompt told the model it works in.
    prompt_dir: Option<String>,
    /// Directory the relative write actually landed in.
    written_dir: Option<PathBuf>,
    caller_workspace: PathBuf,
    target_workspace: PathBuf,
    outcome: String,
}

async fn observe(same_profile: bool) -> Observation {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider().await;
    let (config, caller_workspace, target_workspace) =
        coherence_config(&format!("http://{addr}"), tmp.path(), same_profile);

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the write to the target agent".to_string()),
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
    // Index 1 is the delegated turn: index 0 is the caller's own turn, which
    // issued the delegate call.
    let prompt_dir = bodies
        .get(1)
        .and_then(|body| prompt_working_directory(body));

    let written_dir = [&caller_workspace, &target_workspace]
        .into_iter()
        .find(|dir| dir.join(PROBE_FILE).exists())
        .cloned();

    Observation {
        _root: tmp,
        prompt_dir,
        written_dir,
        caller_workspace,
        target_workspace,
        outcome: format!("{outcome:?}"),
    }
}

fn observe_blocking(same_profile: bool) -> Observation {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async move {
        zeroclaw_spawn::spawn!(observe(same_profile))
            .await
            .expect("chain task joins")
    })
}

fn assert_prompt_matches_write(observation: &Observation, label: &str) {
    let Observation {
        _root: _,
        prompt_dir,
        written_dir,
        caller_workspace,
        target_workspace,
        outcome,
    } = observation;

    let context = format!(
        "{label}: prompt said {prompt_dir:?}; write landed in {written_dir:?}; \
         caller workspace {}; target workspace {}; outcome {outcome}",
        caller_workspace.display(),
        target_workspace.display()
    );

    // Both halves must be observed, or the comparison below would pass by
    // vacuity: an unwritten probe or a promptless turn proves nothing.
    let prompt_dir = prompt_dir
        .as_ref()
        .unwrap_or_else(|| panic!("no working directory in the delegated prompt; {context}"));
    let written_dir = written_dir.as_ref().unwrap_or_else(|| {
        panic!("the relative write did not land in either workspace; {context}")
    });

    let prompt_path = PathBuf::from(prompt_dir);
    assert_eq!(
        prompt_path.canonicalize().ok(),
        written_dir.canonicalize().ok(),
        "the prompt names a different working directory than the one written to; {context}"
    );
}

#[test]
fn same_profile_delegation_names_the_workspace_it_actually_writes_to() {
    // MUST FAIL while the execution policy keeps the CALLER's workspace for a
    // same-profile target while the prompt is built from the TARGET's own
    // configured workspace. Neutralize a fix by restoring either half
    // independently and this must go red again.
    assert_prompt_matches_write(&observe_blocking(true), "same-profile");
}

#[test]
fn cross_profile_delegation_still_names_the_workspace_it_writes_to() {
    // The half that already holds, kept as the pair's other side. Its job is to
    // block a fix that makes the two agree by collapsing the target's identity
    // into the caller's session - that would satisfy the same-profile case
    // above while regressing the cross-profile separation.
    //
    // MUST FAIL if a fix points the prompt at the execution workspace by making
    // every target inherit the caller's, instead of making the two consistent.
    assert_prompt_matches_write(&observe_blocking(false), "cross-profile");
}
