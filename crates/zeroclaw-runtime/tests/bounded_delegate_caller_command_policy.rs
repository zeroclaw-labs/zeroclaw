//! A bounded delegate must not gain command authority its caller's command
//! policy withholds, however that authority is later exercised.
//!
//! `docs/book/src/agents/delegation.md` makes the caller's command policy
//! authoritative for inherited command tools. Direct `shell` honours that (the
//! target's `shell` is gated by the caller's command fields). Every other route
//! by which the target ends up running a command was judged only against the
//! TARGET's policy:
//!
//! - `cron_add` / `schedule` / `cron_update` / `cron_run` store or re-arm a
//!   shell job whose command the scheduler runs later, under the target;
//! - a job reused WITHOUT a command patch (`cron_update` of another field,
//!   `schedule` resume) keeps a command nobody re-judged;
//! - a deferred AGENT job that keeps `shell` in `allowed_tools` starts a target
//!   agent whose `shell` carries no caller restriction at all;
//! - a delegation one hop further down (`caller -> target -> sub`) and a
//!   `spawn_subagent` child both take the target's raw policy as their "caller".
//!
//! Mechanism: *any command a bounded target leaves behind to run later, or runs
//! through a policy rebuilt one hop below the caller, is judged only by the
//! target's command policy and never by the caller's.*
//!
//! The one variable in every negative/control pair is the caller's
//! `allowed_commands`: `["echo"]` (denies the command) versus `["echo",
//! "touch"]` (allows it). The target permits `touch` in both, under `Full`
//! autonomy where no medium-risk approval gate applies, so a difference in
//! outcome can only come from the caller's command policy.
//!
//! No test here executes a command. Each one stops at the store (job written or
//! not, patch applied or not, flag flipped or not), which is also why
//! `cron_run` has no positive half in this file: running the job would spawn a
//! real binary. Its negative and its positive control live in
//! `tools::cron_run::tests`; the control launches `echo` through the native
//! runtime, exactly as that module's existing `force_runs_job_and_records_history`
//! does, so it is a unit test of the tool and not part of this file.
//!
//! Predicted red before the fix (written before running): every negative fails,
//! because the job is stored / the patch is applied / the flag flips; every
//! control passes today and must keep passing.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, extract::State, routing::post};
use tempfile::TempDir;
use zeroclaw_config::autonomy::{AutonomyLevel, DelegationMode, DelegationPolicy};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, DelegateExecutionMode, DelegateTargetConfig, RiskProfileConfig,
    RuntimeProfileConfig,
};
use zeroclaw_runtime::agent::loop_::AgentRunOverrides;
use zeroclaw_runtime::cron::{self, CronJob};

/// The command the caller's policy denies in the negatives and admits in the
/// controls. The target admits it in both.
const DENIED_COMMAND: &str = "touch marker.txt";

/// A command every policy in the file admits. Used for jobs that must exist
/// before the bounded turn and must not themselves be the thing under test.
const HARMLESS_COMMAND: &str = "echo original";

const CALLER_DENIES: &[&str] = &["echo"];
const CALLER_ALLOWS: &[&str] = &["echo", "touch"];
const ANYTHING: &[&str] = &["*"];

#[derive(Clone)]
struct Script {
    calls: Arc<AtomicUsize>,
    replies: Arc<Vec<String>>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-cmdpolicy",
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
        "id": "chatcmpl-cmdpolicy",
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

async fn handle_chat(State(script): State<Script>, _body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script
        .replies
        .get(call)
        .cloned()
        .unwrap_or_else(|| plain_content("done"))
}

async fn spawn_stub_provider(replies: Vec<String>) -> SocketAddr {
    let script = Script {
        calls: Arc::new(AtomicUsize::new(0)),
        replies: Arc::new(replies),
    };
    let app = Router::new()
        .route("/chat/completions", post(handle_chat))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    zeroclaw_spawn::spawn!(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    addr
}

/// Which chain the deferral is made from.
#[derive(Clone, Copy)]
enum Chain {
    /// `caller -> target`.
    OneHop,
    /// `caller -> target -> sub`, both bounded.
    TwoHops,
    /// `caller -> target -> spawn_subagent child`.
    Spawned,
}

/// The command allowlist of each agent, root first, and whether the caller's
/// own ceiling contains `shell`.
#[derive(Clone)]
struct Scenario {
    caller: &'static [&'static str],
    target: &'static [&'static str],
    sub: &'static [&'static str],
    caller_holds_shell: bool,
    chain: Chain,
}

impl Scenario {
    fn one_hop(caller: &'static [&'static str]) -> Self {
        Self {
            caller,
            target: ANYTHING,
            sub: ANYTHING,
            caller_holds_shell: true,
            chain: Chain::OneHop,
        }
    }
}

fn command_policy_config(provider_uri: &str, root: &std::path::Path, s: &Scenario) -> Config {
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

    // `Full` autonomy: no medium-risk approval gate, so the allowlist is the
    // only command-policy difference between caller and target.
    let profile = |tools: Vec<&str>, commands: &[&str]| RiskProfileConfig {
        allowed_tools: tools.into_iter().map(str::to_string).collect(),
        level: AutonomyLevel::Full,
        allowed_commands: commands.iter().map(|c| (*c).to_string()).collect(),
        delegation_policy: DelegationPolicy {
            mode: DelegationMode::Allow,
        },
        ..RiskProfileConfig::default()
    };

    let mut caller_tools = vec![
        "delegate",
        "cron_add",
        "cron_update",
        "schedule",
        "spawn_subagent",
    ];
    if s.caller_holds_shell {
        caller_tools.push("shell");
    }

    let mut risk_profiles = HashMap::new();
    risk_profiles.insert("caller_profile".into(), profile(caller_tools, s.caller));
    risk_profiles.insert(
        "target_profile".into(),
        profile(
            vec![
                "delegate",
                "cron_add",
                "cron_update",
                "schedule",
                "spawn_subagent",
                "shell",
            ],
            s.target,
        ),
    );
    risk_profiles.insert(
        "sub_profile".into(),
        profile(
            vec![
                "cron_add",
                "cron_update",
                "schedule",
                "spawn_subagent",
                "shell",
            ],
            s.sub,
        ),
    );

    let mut runtime_profiles = HashMap::new();
    runtime_profiles.insert(
        "agentic".to_string(),
        RuntimeProfileConfig {
            agentic: true,
            max_tool_iterations: 4,
            ..RuntimeProfileConfig::default()
        },
    );

    let agent = |risk: &str, delegates: Vec<DelegateTargetConfig>| AliasedAgentConfig {
        enabled: true,
        model_provider: "custom.default".into(),
        risk_profile: risk.into(),
        runtime_profile: "agentic".into(),
        delegates,
        ..AliasedAgentConfig::default()
    };
    let bounded = |name: &str| DelegateTargetConfig {
        agent: name.to_string(),
        mode: DelegateExecutionMode::Bounded,
    };

    let mut agents = HashMap::new();
    agents.insert(
        "caller".to_string(),
        agent("caller_profile", vec![bounded("target")]),
    );
    agents.insert(
        "target".to_string(),
        agent("target_profile", vec![bounded("sub")]),
    );
    agents.insert("sub".to_string(), agent("sub_profile", vec![]));

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

/// A job that exists before the bounded turn, written by the owning agent
/// (`target`) under its own policy — the state a bounded caller later reuses.
struct Seed {
    command: &'static str,
    paused: bool,
}

/// The replies that walk the chain down to the loop that makes the deferral.
fn chain_replies(chain: Chain, deferral: String) -> Vec<String> {
    let delegate_target = native_tool_call(
        "delegate",
        r#"{"action":"delegate","agent":"target","prompt":"defer the work"}"#,
    );
    match chain {
        Chain::OneHop => vec![delegate_target, deferral],
        Chain::TwoHops => vec![
            delegate_target,
            native_tool_call(
                "delegate",
                r#"{"action":"delegate","agent":"sub","prompt":"defer the work"}"#,
            ),
            deferral,
        ],
        Chain::Spawned => vec![
            delegate_target,
            native_tool_call("spawn_subagent", r#"{"prompt":"defer the work"}"#),
            deferral,
        ],
    }
}

/// Drives `caller -> ... -> <scheduler tool>` and returns the jobs left in the
/// store, so every assertion is about what survived rather than what a tool
/// answered.
async fn drive(
    scenario: Scenario,
    seeds: Vec<Seed>,
    deferral: impl FnOnce(&[String]) -> String,
) -> (Vec<CronJob>, String) {
    let tmp = TempDir::new().expect("temp root");

    // The seed jobs are written through a config whose provider URI is a
    // placeholder: the store lives under `data_dir`, which both configs share,
    // and the deferral needs the seeded ids before the stub can be started.
    let seed_config = command_policy_config("http://127.0.0.1:1", tmp.path(), &scenario);
    let mut ids = Vec::new();
    for seed in &seeds {
        let job = cron::add_job(&seed_config, "target", "*/5 * * * *", seed.command)
            .expect("the owning agent may store its own command");
        if seed.paused {
            cron::pause_job(&seed_config, &job.id).expect("pause seed job");
        }
        ids.push(job.id);
    }

    let addr = spawn_stub_provider(chain_replies(scenario.chain, deferral(&ids))).await;
    let config = command_policy_config(&format!("http://{addr}"), tmp.path(), &scenario);
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

    let jobs = cron::list_jobs(&reader).unwrap_or_default();
    let report = format!(
        "outcome {outcome:?}; jobs {:?}",
        jobs.iter()
            .map(|j| (
                j.id.clone(),
                j.name.clone(),
                j.agent_alias.clone(),
                j.command.clone(),
                j.enabled,
                j.allowed_tools.clone()
            ))
            .collect::<Vec<_>>()
    );
    (jobs, report)
}

/// Nested delegation stacks futures past the harness default per-thread stack.
fn drive_blocking(
    scenario: Scenario,
    seeds: Vec<Seed>,
    deferral: impl FnOnce(&[String]) -> String + Send + 'static,
) -> (Vec<CronJob>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive(scenario, seeds, deferral))
            .await
            .expect("chain task joins")
    })
}

fn stores(jobs: &[CronJob], command: &str) -> bool {
    jobs.iter().any(|job| job.command == command)
}

// ── deferrals ───────────────────────────────────────────────────────────────

fn cron_add_shell(_: &[String]) -> String {
    native_tool_call(
        "cron_add",
        &serde_json::json!({
            "name": "deferred-shell-work",
            "job_type": "shell",
            "schedule": {"kind": "cron", "expr": "*/5 * * * *"},
            "command": DENIED_COMMAND,
        })
        .to_string(),
    )
}

fn schedule_shell(args: serde_json::Value) -> impl FnOnce(&[String]) -> String {
    move |_| native_tool_call("schedule", &args.to_string())
}

// ── cron_add ────────────────────────────────────────────────────────────────

#[test]
fn cron_add_refuses_a_shell_job_the_caller_command_policy_denies() {
    let (jobs, report) = drive_blocking(Scenario::one_hop(CALLER_DENIES), vec![], cron_add_shell);
    assert!(
        !stores(&jobs, DENIED_COMMAND),
        "a bounded target stored `{DENIED_COMMAND}` although its caller's command policy \
         denies it; the scheduler would run it under the target's policy; {report}"
    );
}

#[test]
fn cron_add_stores_the_same_job_when_the_caller_command_policy_allows_it() {
    // Control for the test above: the same chain and config but for one extra
    // entry in the caller's allowlist. Without it "nothing stored" could come
    // from any other gate.
    let (jobs, report) = drive_blocking(Scenario::one_hop(CALLER_ALLOWS), vec![], cron_add_shell);
    assert!(
        stores(&jobs, DENIED_COMMAND),
        "control: with both policies admitting the command the job must be stored, or \
         the refusal above cannot be attributed to the caller's command policy; {report}"
    );
}

// ── schedule: three creation routes ─────────────────────────────────────────

fn schedule_expression() -> impl FnOnce(&[String]) -> String {
    schedule_shell(serde_json::json!({
        "action": "add", "expression": "*/5 * * * *", "command": DENIED_COMMAND,
    }))
}
fn schedule_delay() -> impl FnOnce(&[String]) -> String {
    schedule_shell(serde_json::json!({
        "action": "once", "delay": "30m", "command": DENIED_COMMAND,
    }))
}
fn schedule_run_at() -> impl FnOnce(&[String]) -> String {
    schedule_shell(serde_json::json!({
        "action": "once", "run_at": "2099-01-01T00:00:00Z", "command": DENIED_COMMAND,
    }))
}

#[test]
fn schedule_expression_refuses_a_command_the_caller_denies() {
    let (jobs, report) = drive_blocking(
        Scenario::one_hop(CALLER_DENIES),
        vec![],
        schedule_expression(),
    );
    assert!(!stores(&jobs, DENIED_COMMAND), "{report}");
}

#[test]
fn schedule_expression_stores_it_when_the_caller_allows() {
    let (jobs, report) = drive_blocking(
        Scenario::one_hop(CALLER_ALLOWS),
        vec![],
        schedule_expression(),
    );
    assert!(stores(&jobs, DENIED_COMMAND), "control: {report}");
}

#[test]
fn schedule_delay_refuses_a_command_the_caller_denies() {
    let (jobs, report) = drive_blocking(Scenario::one_hop(CALLER_DENIES), vec![], schedule_delay());
    assert!(!stores(&jobs, DENIED_COMMAND), "{report}");
}

#[test]
fn schedule_delay_stores_it_when_the_caller_allows() {
    let (jobs, report) = drive_blocking(Scenario::one_hop(CALLER_ALLOWS), vec![], schedule_delay());
    assert!(stores(&jobs, DENIED_COMMAND), "control: {report}");
}

#[test]
fn schedule_run_at_refuses_a_command_the_caller_denies() {
    let (jobs, report) =
        drive_blocking(Scenario::one_hop(CALLER_DENIES), vec![], schedule_run_at());
    assert!(!stores(&jobs, DENIED_COMMAND), "{report}");
}

#[test]
fn schedule_run_at_stores_it_when_the_caller_allows() {
    let (jobs, report) =
        drive_blocking(Scenario::one_hop(CALLER_ALLOWS), vec![], schedule_run_at());
    assert!(stores(&jobs, DENIED_COMMAND), "control: {report}");
}

// ── cron_update ─────────────────────────────────────────────────────────────

fn cron_update_command(ids: &[String]) -> String {
    native_tool_call(
        "cron_update",
        &serde_json::json!({
            "job_id": ids[0],
            "patch": {"command": DENIED_COMMAND},
        })
        .to_string(),
    )
}

#[test]
fn cron_update_refuses_a_command_patch_the_caller_denies() {
    let seeds = vec![Seed {
        command: HARMLESS_COMMAND,
        paused: false,
    }];
    let (jobs, report) =
        drive_blocking(Scenario::one_hop(CALLER_DENIES), seeds, cron_update_command);
    assert!(
        !stores(&jobs, DENIED_COMMAND) && stores(&jobs, HARMLESS_COMMAND),
        "{report}"
    );
}

#[test]
fn cron_update_applies_the_same_command_patch_when_the_caller_allows() {
    let seeds = vec![Seed {
        command: HARMLESS_COMMAND,
        paused: false,
    }];
    let (jobs, report) =
        drive_blocking(Scenario::one_hop(CALLER_ALLOWS), seeds, cron_update_command);
    assert!(stores(&jobs, DENIED_COMMAND), "control: {report}");
}

/// The case the review names: an existing job whose STORED command the caller
/// denies, touched by a patch that does not mention `command` at all. The
/// patch itself is harmless (a rename); what it re-points is a job whose
/// command nobody re-judged.
fn cron_update_rename(ids: &[String]) -> String {
    native_tool_call(
        "cron_update",
        &serde_json::json!({
            "job_id": ids[0],
            "patch": {"name": "renamed-by-bounded-caller"},
        })
        .to_string(),
    )
}

fn renamed(jobs: &[CronJob]) -> bool {
    jobs.iter()
        .any(|job| job.name.as_deref() == Some("renamed-by-bounded-caller"))
}

#[test]
fn cron_update_without_a_command_patch_refuses_a_stored_command_the_caller_denies() {
    let seeds = vec![Seed {
        command: DENIED_COMMAND,
        paused: false,
    }];
    let (jobs, report) =
        drive_blocking(Scenario::one_hop(CALLER_DENIES), seeds, cron_update_rename);
    assert!(
        !renamed(&jobs),
        "a bounded target re-pointed an existing job whose stored command its caller's \
         command policy denies; {report}"
    );
}

#[test]
fn cron_update_without_a_command_patch_applies_when_the_caller_allows() {
    let seeds = vec![Seed {
        command: DENIED_COMMAND,
        paused: false,
    }];
    let (jobs, report) =
        drive_blocking(Scenario::one_hop(CALLER_ALLOWS), seeds, cron_update_rename);
    assert!(renamed(&jobs), "control: {report}");
}

/// Pausing only removes capability and stays allowed even for a job the caller
/// could not have created — otherwise a bounded target could not switch off
/// what it may not switch on.
#[test]
fn cron_update_may_still_disable_a_job_whose_command_the_caller_denies() {
    let seeds = vec![Seed {
        command: DENIED_COMMAND,
        paused: false,
    }];
    let (jobs, report) = drive_blocking(Scenario::one_hop(CALLER_DENIES), seeds, |ids| {
        native_tool_call(
            "cron_update",
            &serde_json::json!({"job_id": ids[0], "patch": {"enabled": false}}).to_string(),
        )
    });
    assert!(
        jobs.iter().all(|job| !job.enabled),
        "disabling must stay possible; {report}"
    );
}

// ── schedule resume ─────────────────────────────────────────────────────────

fn schedule_resume(ids: &[String]) -> String {
    native_tool_call(
        "schedule",
        &serde_json::json!({"action": "resume", "id": ids[0]}).to_string(),
    )
}

#[test]
fn schedule_resume_refuses_a_stored_command_the_caller_denies() {
    let seeds = vec![Seed {
        command: DENIED_COMMAND,
        paused: true,
    }];
    let (jobs, report) = drive_blocking(Scenario::one_hop(CALLER_DENIES), seeds, schedule_resume);
    assert!(
        jobs.iter().all(|job| !job.enabled),
        "a bounded target re-armed a paused job whose command its caller denies; {report}"
    );
}

#[test]
fn schedule_resume_rearms_the_job_when_the_caller_allows() {
    let seeds = vec![Seed {
        command: DENIED_COMMAND,
        paused: true,
    }];
    let (jobs, report) = drive_blocking(Scenario::one_hop(CALLER_ALLOWS), seeds, schedule_resume);
    assert!(jobs.iter().any(|job| job.enabled), "control: {report}");
}

// ── deferred AGENT jobs that keep `shell` ───────────────────────────────────

fn cron_add_agent(allowed_tools: Option<Vec<&'static str>>) -> impl FnOnce(&[String]) -> String {
    move |_| {
        let mut args = serde_json::json!({
            "name": "deferred-agent-work",
            "schedule": {"kind": "cron", "expr": "*/5 * * * *"},
            "prompt": "do the deferred work",
        });
        if let Some(list) = allowed_tools {
            args["allowed_tools"] = serde_json::json!(list);
        }
        native_tool_call("cron_add", &args.to_string())
    }
}

fn agent_job_stored(jobs: &[CronJob]) -> Option<&CronJob> {
    jobs.iter()
        .find(|job| job.name.as_deref() == Some("deferred-agent-work"))
}

#[test]
fn a_bounded_agent_job_naming_shell_is_refused() {
    let (jobs, report) = drive_blocking(
        Scenario::one_hop(CALLER_DENIES),
        vec![],
        cron_add_agent(Some(vec!["shell", "cron_add"])),
    );
    assert!(
        agent_job_stored(&jobs).is_none(),
        "a deferred agent job kept `shell`; its later run carries only the target's \
         command policy; {report}"
    );
}

#[test]
fn a_bounded_agent_job_inheriting_a_ceiling_with_shell_is_refused() {
    // No `allowed_tools` means "inherit the caller's ceiling" — which contains
    // `shell` here, so the stored list would too.
    let (jobs, report) = drive_blocking(
        Scenario::one_hop(CALLER_DENIES),
        vec![],
        cron_add_agent(None),
    );
    assert!(agent_job_stored(&jobs).is_none(), "{report}");
}

#[test]
fn a_bounded_agent_job_without_shell_is_still_stored() {
    // Control: the refusal must be about `shell`, not about agent jobs.
    let (jobs, report) = drive_blocking(
        Scenario::one_hop(CALLER_DENIES),
        vec![],
        cron_add_agent(Some(vec!["cron_add"])),
    );
    let job = agent_job_stored(&jobs)
        .unwrap_or_else(|| panic!("control: no agent job was stored; {report}"));
    assert_eq!(
        job.allowed_tools.as_deref(),
        Some(&["cron_add".to_string()][..])
    );
}

#[test]
fn a_bounded_agent_job_inheriting_a_ceiling_without_shell_is_still_stored() {
    // Control for the inherit route: the caller holds no `shell`, so the
    // ceiling it stores has none to keep.
    let mut scenario = Scenario::one_hop(CALLER_DENIES);
    scenario.caller_holds_shell = false;
    let (jobs, report) = drive_blocking(scenario, vec![], cron_add_agent(None));
    let job = agent_job_stored(&jobs)
        .unwrap_or_else(|| panic!("control: no agent job was stored; {report}"));
    assert!(
        job.allowed_tools
            .as_ref()
            .is_some_and(|tools| !tools.iter().any(|t| t == "shell")),
        "{report}"
    );
}

// ── the caller's policy survives another hop ────────────────────────────────

#[test]
fn cron_add_two_hops_down_still_honours_the_root_caller_command_policy() {
    // The root caller denies `touch`; both agents below it permit it. Before
    // the fix the second hop's "caller" is the FIRST target's raw policy, which
    // permits it, so the root caller's restriction is gone.
    let scenario = Scenario {
        chain: Chain::TwoHops,
        ..Scenario::one_hop(CALLER_DENIES)
    };
    let (jobs, report) = drive_blocking(scenario, vec![], cron_add_shell);
    assert!(!stores(&jobs, DENIED_COMMAND), "{report}");
}

#[test]
fn cron_add_two_hops_down_stores_it_when_every_policy_allows() {
    let scenario = Scenario {
        chain: Chain::TwoHops,
        ..Scenario::one_hop(CALLER_ALLOWS)
    };
    let (jobs, report) = drive_blocking(scenario, vec![], cron_add_shell);
    assert!(stores(&jobs, DENIED_COMMAND), "control: {report}");
}

#[test]
fn cron_add_two_hops_down_honours_a_restriction_set_by_the_middle_agent() {
    // The restriction is the MIDDLE hop's, not the root's: the composition must
    // hold for every ancestor, not just the first.
    let scenario = Scenario {
        chain: Chain::TwoHops,
        caller: ANYTHING,
        target: CALLER_DENIES,
        sub: ANYTHING,
        caller_holds_shell: true,
    };
    let (jobs, report) = drive_blocking(scenario, vec![], cron_add_shell);
    assert!(!stores(&jobs, DENIED_COMMAND), "{report}");
}

// ── a spawn_subagent child of a bounded target ──────────────────────────────

#[test]
fn a_spawned_child_of_a_bounded_target_honours_the_caller_command_policy() {
    let scenario = Scenario {
        chain: Chain::Spawned,
        ..Scenario::one_hop(CALLER_DENIES)
    };
    let (jobs, report) = drive_blocking(scenario, vec![], cron_add_shell);
    assert!(!stores(&jobs, DENIED_COMMAND), "{report}");
}

#[test]
fn a_spawned_child_stores_the_job_when_the_caller_command_policy_allows() {
    let scenario = Scenario {
        chain: Chain::Spawned,
        ..Scenario::one_hop(CALLER_ALLOWS)
    };
    let (jobs, report) = drive_blocking(scenario, vec![], cron_add_shell);
    assert!(stores(&jobs, DENIED_COMMAND), "control: {report}");
}
