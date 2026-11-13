//! A bounded delegate target must not defer shell execution its caller could
//! not have performed in the turn itself.
//!
//! The other bounded-delegation regressions in this directory follow a tool
//! NAME through a stored job's `allowed_tools`. A shell job has no such column:
//! it stores a command string, and `cron::scheduler` later runs it under the
//! OWNING agent's policy without consulting any tool list. So there is nothing
//! to intersect and nothing to carry forward — the bound has to be applied as a
//! refusal at the moment the job is written.
//!
//! The scenario: `caller` may `delegate`, `cron_add` and `schedule`, but has no
//! `shell`. `target` permits `shell` under its own risk profile. The caller
//! delegates bounded work, and inside that bounded sub-loop the model asks for a
//! SHELL job. Without the refusal the job is persisted under the target and the
//! scheduler runs a command the caller was never granted.
//!
//! THIS FILE MUST FAIL if `caller_ceiling::require_shell_within_ceiling` stops
//! refusing. Neutralize it by returning `Ok(())` as its first line: the two
//! negative tests below store the job again, while their positive controls stay
//! green — which is exactly why the positive controls cannot substitute for
//! them.
//!
//! Both halves are asserted, and here the positive half is doing more work than
//! usual. A shell job is refused by several unrelated gates before it is ever
//! stored — an unallowed command, a supervised risk profile, a rejected
//! schedule — so "no job was stored" is a conclusion many failures produce. The
//! positive control runs the SAME chain with the SAME config except for one
//! entry in the caller's `allowed_tools`, and requires the job to be stored with
//! its command intact. Only that difference isolates the ceiling as the cause.
//!
//! Honest limit, stated because the review asked for execution and this asserts
//! persistence: these tests stop at the store. They do not run the scheduler and
//! do not execute a command — the working rules for this repository forbid a
//! test that touches a real binary. Under the fail-closed contract that is the
//! whole observable: nothing in the scheduler changed, so a job that is never
//! written is never run, and a job that IS written runs exactly as it did
//! before. What is NOT covered here is a job stored before this change and
//! replayed afterwards; that case is unchanged by design and is declared in the
//! PR body.

use std::collections::HashMap;
use std::net::SocketAddr;
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

/// The capability the caller does not hold in the negative tests, and the only
/// thing that differs in the positive controls.
const SHELL: &str = "shell";

/// The command the deferred job would run. Matched in the store instead of the
/// job name, because `schedule` creates its jobs unnamed.
const JOB_COMMAND: &str = "echo deferred-work";

const JOB_NAME: &str = "deferred-shell-work";

#[derive(Clone)]
struct Script {
    calls: Arc<AtomicUsize>,
    captured: Arc<AsyncMutex<Vec<String>>>,
    /// Canned replies, consumed in order. Once exhausted every further turn
    /// answers plainly so each loop in the chain unwinds instead of hanging.
    replies: Arc<Vec<String>>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-shell",
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
        "id": "chatcmpl-shell",
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

/// The one variable under test is `caller_holds_shell`. Everything else —
/// including the target's own permission to run shell commands, its autonomy
/// level and its command allowlist — is identical in both directions, so a
/// difference in outcome can only come from the ceiling.
fn deferred_shell_config(
    provider_uri: &str,
    root: &std::path::Path,
    caller_holds_shell: bool,
) -> Config {
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

    // `Full` autonomy and a wildcard command allowlist on BOTH sides: the shell
    // command must clear every gate that is not the ceiling, or the negative
    // test would pass for a reason it does not name.
    let permissive = |tools: Vec<String>| RiskProfileConfig {
        allowed_tools: tools,
        level: AutonomyLevel::Full,
        allowed_commands: vec!["*".to_string()],
        delegation_policy: DelegationPolicy {
            mode: DelegationMode::Allow,
        },
        ..RiskProfileConfig::default()
    };

    let mut caller_tools = vec![
        "delegate".to_string(),
        "cron_add".to_string(),
        "schedule".to_string(),
    ];
    if caller_holds_shell {
        caller_tools.push(SHELL.to_string());
    }

    let mut risk_profiles = HashMap::new();
    risk_profiles.insert("caller_profile".to_string(), permissive(caller_tools));
    // The target may run shell commands in its own right. That is what makes
    // the escape reachable: the command validates against THIS profile.
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec![
            "cron_add".to_string(),
            "schedule".to_string(),
            SHELL.to_string(),
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
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    config
}

/// Drives `caller -> bounded target -> <scheduler tool>` and reports the stored
/// commands, so the assertion is about what survived to the store rather than
/// about what the tool answered.
async fn drive_bounded_shell_job(
    caller_holds_shell: bool,
    deferral: String,
) -> (Vec<String>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, _captured) = spawn_stub_provider(vec![
        native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"defer the work"}"#,
        ),
        deferral,
    ])
    .await;
    let config = deferred_shell_config(&format!("http://{addr}"), tmp.path(), caller_holds_shell);
    // `agent::run` consumes the config; the store is read back through this copy.
    let reader = config.clone();

    let outcome = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the deferral to the target agent".to_string()),
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
        "caller_holds_shell={caller_holds_shell}; outcome {outcome:?}; jobs {:?}",
        jobs.iter()
            .map(|j| (
                j.name.clone(),
                j.agent_alias.clone(),
                j.job_type.clone(),
                j.command.clone()
            ))
            .collect::<Vec<_>>()
    );
    let commands = jobs.into_iter().map(|job| job.command).collect();
    (commands, report)
}

/// Nested delegation stacks futures past the harness default per-thread stack.
fn drive_blocking(caller_holds_shell: bool, deferral: String) -> (Vec<String>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_bounded_shell_job(caller_holds_shell, deferral))
            .await
            .expect("chain task joins")
    })
}

/// `cron_add` with `job_type: "shell"`: the branch that never consulted the
/// ceiling, because the cap only ever ran on the agent branch's `allowed_tools`.
fn cron_add_shell_job() -> String {
    native_tool_call(
        "cron_add",
        &serde_json::json!({
            "name": JOB_NAME,
            "job_type": "shell",
            "schedule": {"kind": "cron", "expr": "*/5 * * * *"},
            "command": JOB_COMMAND,
        })
        .to_string(),
    )
}

/// `schedule` create: the tool the PR body used to declare out of scope on the
/// grounds that it writes no `allowed_tools` — which is true, and is exactly
/// why it needed a different bound rather than none.
fn schedule_shell_job() -> String {
    native_tool_call(
        "schedule",
        &serde_json::json!({
            "action": "add",
            "expression": "*/5 * * * *",
            "command": JOB_COMMAND,
        })
        .to_string(),
    )
}

#[test]
fn a_bounded_delegate_cannot_store_a_shell_job_through_cron_add() {
    let (commands, report) = drive_blocking(false, cron_add_shell_job());
    assert!(
        !commands.iter().any(|c| c == JOB_COMMAND),
        "a bounded target stored a shell job its caller could not have run: the \
         scheduler would execute `{JOB_COMMAND}` under the target's policy long \
         after the bounded turn ended; {report}"
    );
}

#[test]
fn cron_add_still_stores_the_shell_job_when_the_caller_held_shell() {
    // The positive control for the test above: same chain, same config, one
    // extra entry in the caller's `allowed_tools`. If this ever goes red the
    // negative test above stops proving anything, because "nothing stored"
    // would no longer be attributable to the ceiling.
    let (commands, report) = drive_blocking(true, cron_add_shell_job());
    assert!(
        commands.iter().any(|c| c == JOB_COMMAND),
        "a caller that holds `shell` may still defer it, but no job was stored, so \
         the refusal above cannot be attributed to the ceiling; {report}"
    );
}

#[test]
fn a_bounded_delegate_cannot_create_a_shell_job_through_schedule() {
    let (commands, report) = drive_blocking(false, schedule_shell_job());
    assert!(
        !commands.iter().any(|c| c == JOB_COMMAND),
        "a bounded target created a shell job through `schedule`, which stores no \
         `allowed_tools` and therefore carries no bound of its own; {report}"
    );
}

#[test]
fn schedule_still_creates_the_shell_job_when_the_caller_held_shell() {
    let (commands, report) = drive_blocking(true, schedule_shell_job());
    assert!(
        commands.iter().any(|c| c == JOB_COMMAND),
        "a caller that holds `shell` may still defer it through `schedule`, but no \
         job was stored, so the refusal above cannot be attributed to the ceiling; \
         {report}"
    );
}
