//! A denied `sop_approve` placeholder must not become real approval authority
//! wherever the bounded ceiling crosses into a freshly-built, unbounded
//! registry.
//!
//! The bounded rebuild always substitutes `sop_approve` with
//! `BoundedSopApproveDenied`, a stub that always refuses — that immediate-turn
//! substitution is pinned by
//! `bounded_cross_profile_sop_approve_is_replaced_by_refusing_stub` in
//! `delegate.rs`. But the stub reports the same `name()` as the real
//! `SopApproveTool`, and the caller ceiling is sealed from names alone. Two
//! paths rebuild a normal (unbounded) registry from scratch and admit tools by
//! matching that ceiling's names, rather than by reusing the bounded turn's own
//! substituted instances:
//!
//!   - a cron job stored without an explicit `allowed_tools` list, which caps
//!     to the WHOLE ceiling and is later replayed by the scheduler through
//!     `agent::run`'s ordinary registry construction;
//!   - `spawn_subagent`, which calls `agent::run` directly, in the same turn,
//!     with the ceiling as `allowed_tools`.
//!
//! If `sop_approve` is in that sealed set, both paths construct the REAL
//! `SopApproveTool` and retain it by name, handing the replayed or spawned turn
//! approval authority the original bounded turn was explicitly denied.
//!
//! THIS TEST MUST FAIL if the ceiling derivation in `delegate.rs` stops
//! excluding a denied placeholder's name. Neutralize by sealing the ceiling
//! from every assembled name unconditionally (drop the `as_any` filter) and
//! `sop_approve` reappears in the stored job and in the spawned child's offered
//! tools below.
//!
//! Every assertion pairs the negative (no `sop_approve`) with a positive half
//! (`WITHIN_CEILING` present), because under a ceiling that admitted nothing at
//! all the negative would hold for the wrong reason.

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

/// The name whose authority must not cross either boundary. Both caller and
/// target hold it (as a permission), but the bounded instance is a refusing
/// stub, not the capability the name suggests.
const SOP_APPROVE: &str = "sop_approve";

/// Held by both caller and target and carried unmodified through the ceiling,
/// so the positive half of every assertion has something to observe.
const WITHIN_CEILING: &str = "cron_add";

const JOB_NAME: &str = "bounded-sop-ceiling-job";
const JOB_PROMPT: &str = "ZC-SOP-CEILING-JOB-PROMPT carry out the scheduled work";
const CHILD_MARKER: &str = "ZC-SOP-CEILING-CHILD-MARKER";

#[derive(Clone)]
struct Script {
    calls: Arc<AtomicUsize>,
    captured: Arc<AsyncMutex<Vec<String>>>,
    replies: Arc<Vec<String>>,
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

fn sop_approve_ceiling_config(provider_uri: &str, root: &std::path::Path) -> Config {
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
    // The caller holds `sop_approve` as a PERMISSION — it is never the caller's
    // own turn that is at risk (that path already substitutes the stub), it is
    // whatever rebuilds a fresh registry later from the name this profile lets
    // through the initial filter.
    risk_profiles.insert(
        "caller_profile".to_string(),
        permissive(vec![
            "delegate".to_string(),
            "spawn_subagent".to_string(),
            WITHIN_CEILING.to_string(),
            SOP_APPROVE.to_string(),
        ]),
    );
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec![
            "spawn_subagent".to_string(),
            WITHIN_CEILING.to_string(),
            SOP_APPROVE.to_string(),
        ]),
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

    // A non-empty `sops_dir` is the sole activation switch for the SOP runtime
    // (`SopConfig::runtime_enabled`) — no procedure needs to exist inside it for
    // `SopApproveTool` to be constructed and registered by name.
    let sops_dir = root.join("sops");
    std::fs::create_dir_all(&sops_dir).expect("sops dir");

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

/// Nested delegation stacks futures past the harness default per-thread stack.
fn drive_cron_add_blocking() -> (Option<Vec<String>>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_bounded_cron_add_without_explicit_tools())
            .await
            .expect("chain task joins")
    })
}

fn drive_replay_blocking() -> (Option<BTreeSet<String>>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_bounded_cron_then_replay())
            .await
            .expect("chain task joins")
    })
}

fn drive_spawn_blocking() -> (Option<BTreeSet<String>>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_bounded_spawn_subagent())
            .await
            .expect("chain task joins")
    })
}

/// Returns the stored `allowed_tools` of the scheduled job, plus a report
/// string for assertion failures.
async fn drive_bounded_cron_add_without_explicit_tools() -> (Option<Vec<String>>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, _captured) = spawn_stub_provider(vec![
        native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"schedule the follow-up"}"#,
        ),
        // No `allowed_tools` key at all: `cap_stored_allowed_tools` reads this as
        // "inherit", and what a bounded registration inherits is the WHOLE
        // sealed ceiling (`caller_ceiling.rs`, `None => ceiling.to_vec()`).
        native_tool_call(
            "cron_add",
            &serde_json::json!({
                "name": JOB_NAME,
                "schedule": {"kind": "cron", "expr": "*/5 * * * *"},
                "prompt": JOB_PROMPT,
            })
            .to_string(),
        ),
    ])
    .await;
    let config = sop_approve_ceiling_config(&format!("http://{addr}"), tmp.path());
    let reader = config.clone();

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the scheduling to the target agent".to_string()),
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

    let jobs = zeroclaw_runtime::cron::list_jobs(&reader).unwrap_or_default();
    let report = format!(
        "outcome {outcome:?}; jobs {:?}",
        jobs.iter()
            .map(|j| (
                j.name.clone(),
                j.agent_alias.clone(),
                j.allowed_tools.clone()
            ))
            .collect::<Vec<_>>()
    );
    let stored = jobs
        .into_iter()
        .find(|job| job.name.as_deref() == Some(JOB_NAME))
        .and_then(|job| job.allowed_tools);
    (stored, report)
}

/// The stored ceiling is what a scheduler replay reads back, long after this
/// turn — and every turn it could reach — is gone. Wrong here, and every test
/// after it in this file is moot.
#[test]
fn a_cron_job_scheduled_without_explicit_tools_does_not_store_sop_approve() {
    let (stored, report) = drive_cron_add_blocking();

    let stored = stored.unwrap_or_else(|| {
        panic!(
            "no job with an allowed_tools list was stored; the negative assertion \
             would hold vacuously. {report}"
        )
    });
    assert!(
        stored.iter().any(|t| t == WITHIN_CEILING),
        "the admitted tool did not survive into the stored ceiling, so the job was \
         not really scheduled through the bounded path; stored={stored:?}; {report}"
    );
    assert!(
        !stored.iter().any(|t| t == SOP_APPROVE),
        "a job scheduled from inside a bounded delegate stored `{SOP_APPROVE}`: the \
         denied stub's name was sealed into the ceiling as if it were the real \
         capability; stored={stored:?}; {report}"
    );
}

/// Runs the bounded chain to create the job, then REPLAYS it through the
/// scheduler and returns the tool set offered on the replayed turn's own model
/// request.
async fn drive_bounded_cron_then_replay() -> (Option<BTreeSet<String>>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider(vec![
        native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"schedule the follow-up"}"#,
        ),
        native_tool_call(
            "cron_add",
            &serde_json::json!({
                "name": JOB_NAME,
                "schedule": {"kind": "cron", "expr": "*/5 * * * *"},
                "prompt": JOB_PROMPT,
            })
            .to_string(),
        ),
    ])
    .await;
    let config = sop_approve_ceiling_config(&format!("http://{addr}"), tmp.path());
    let reader = config.clone();

    let first = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the scheduling to the target agent".to_string()),
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

    let jobs = zeroclaw_runtime::cron::list_jobs(&reader).unwrap_or_default();
    let job = jobs
        .into_iter()
        .find(|job| job.name.as_deref() == Some(JOB_NAME));
    let stored = job.as_ref().and_then(|j| j.allowed_tools.clone());

    // Everything captured up to here belongs to the first run. The scheduler
    // replay's OWN first request — the only one this test needs — is the very
    // next capture, since the stub replies "done" to it and nothing further is
    // scheduled or spawned.
    let first_run_turns = captured.lock().await.len();
    let replay = match job.as_ref() {
        Some(job) => Some(zeroclaw_runtime::cron::scheduler::execute_job_now(&reader, job).await),
        None => None,
    };

    let bodies = captured.lock().await.clone();
    let replay_turn = bodies.get(first_run_turns).map(|body| offered_tools(body));
    let report = format!(
        "first_run {first:?}; stored {stored:?}; replay {replay:?}; turns={}; \
         per-turn tools {:?}",
        bodies.len(),
        bodies.iter().map(|b| offered_tools(b)).collect::<Vec<_>>()
    );
    (replay_turn, report)
}

/// The escape that survives the turn: even a stored ceiling correctly stripped
/// of `sop_approve` proves nothing on its own if the REPLAYED turn's own
/// registry construction re-admits it some other way. This follows the escape
/// the extra hop the stored-list test above does not reach.
///
/// THIS TEST MUST FAIL if the ceiling derivation stops excluding the denied
/// stub's name (the same neutralization as the test above, one hop further).
#[test]
fn a_replayed_bounded_cron_job_is_never_offered_sop_approve() {
    let (replay_turn, report) = drive_replay_blocking();

    let offered = replay_turn.unwrap_or_else(|| {
        panic!(
            "the scheduler replay produced no model request, so the ceiling \
             assertion below would hold vacuously. {report}"
        )
    });
    assert!(
        offered.contains(WITHIN_CEILING),
        "the replayed turn offered nothing from within the ceiling, so the absence \
         of `{SOP_APPROVE}` proves nothing; offered={offered:?}; {report}"
    );
    assert!(
        !offered.contains(SOP_APPROVE),
        "a REPLAYED bounded cron job was offered `{SOP_APPROVE}`: the caller's turn \
         was denied approval authority, but the replayed turn recovered it through \
         `agent::run`'s ordinary registry construction; offered={offered:?}; {report}"
    );
}

/// 0: caller delegates. 1: the bounded target spawns a child, planting
/// `CHILD_MARKER` so its own turn can be found by content. Everything after
/// answers plainly so both loops unwind.
async fn handle_chat_spawn(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    match call {
        0 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"hand this to a subagent"}"#,
        ),
        1 => native_tool_call(
            "spawn_subagent",
            &serde_json::json!({ "prompt": format!("{CHILD_MARKER} do the nested work") })
                .to_string(),
        ),
        _ => plain_content("done"),
    }
}

async fn spawn_stub_provider_for_spawn() -> (SocketAddr, Arc<AsyncMutex<Vec<String>>>) {
    let script = Script {
        calls: Arc::new(AtomicUsize::new(0)),
        captured: Arc::new(AsyncMutex::new(Vec::new())),
        replies: Arc::new(Vec::new()),
    };
    let captured = Arc::clone(&script.captured);
    let app = Router::new()
        .route("/chat/completions", post(handle_chat_spawn))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    zeroclaw_spawn::spawn!(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    (addr, captured)
}

/// Returns the tool set offered on the spawned child's own model request, plus
/// a report for assertion failures. No cron and no persistence boundary here:
/// `spawn_subagent` calls `agent::run` directly, in the SAME turn, with the
/// ceiling as `allowed_tools` — the shared-defect path the reviewer named
/// alongside the cron one.
async fn drive_bounded_spawn_subagent() -> (Option<BTreeSet<String>>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider_for_spawn().await;
    let config = sop_approve_ceiling_config(&format!("http://{addr}"), tmp.path());

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
    let child_turn = bodies
        .iter()
        .find(|body| body.contains(CHILD_MARKER))
        .map(|body| offered_tools(body));
    let report = format!(
        "outcome {outcome:?}; turns={}; per-turn tools {:?}",
        bodies.len(),
        bodies.iter().map(|b| offered_tools(b)).collect::<Vec<_>>()
    );
    (child_turn, report)
}

/// THIS TEST MUST FAIL if the ceiling derivation stops excluding the denied
/// stub's name: `spawn_subagent` carries the same sealed ceiling into its own
/// `agent::run` call, so the child's fresh registry would retain the real
/// `SopApproveTool` by name exactly as the cron replay does.
#[test]
fn a_child_spawned_from_a_bounded_delegate_is_never_offered_sop_approve() {
    let (child_turn, report) = drive_spawn_blocking();

    let offered = child_turn.unwrap_or_else(|| {
        panic!(
            "no model request carried the child marker, so spawn_subagent never \
             reached a child turn and the ceiling assertion below would hold \
             vacuously. {report}"
        )
    });
    assert!(
        offered.contains(WITHIN_CEILING),
        "the spawned child's turn offered nothing from within the ceiling, so the \
         absence of `{SOP_APPROVE}` proves nothing; offered={offered:?}; {report}"
    );
    assert!(
        !offered.contains(SOP_APPROVE),
        "a child spawned from a bounded delegate was offered `{SOP_APPROVE}`: \
         `spawn_subagent` carried the denied stub's sealed name into its own \
         `agent::run` call, which built the REAL approval tool and retained it by \
         name; offered={offered:?}; {report}"
    );
}
