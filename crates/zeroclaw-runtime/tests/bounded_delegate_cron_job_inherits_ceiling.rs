//! A cron job scheduled from inside a bounded delegate must not outlive the
//! caller's tool ceiling.
//!
//! Bounded delegation caps the target's registry by the caller's own. That cap
//! lives in the turn. A cron job does not: when it fires, the scheduler rebuilds
//! the owning agent's policy from config and hands `agent::run` the job's STORED
//! `allowed_tools`, so a job saved without one runs with the owning agent's full
//! registry. A bounded target can therefore schedule work that executes tools
//! its caller was never granted — the deferred half of the same escape the
//! in-turn ceiling closes.
//!
//! The scenario: `caller` may `delegate` and `cron_add` but has no
//! `file_write`. `target` permits both `cron_add` and `file_write`. The caller
//! delegates bounded work; inside that bounded sub-loop the model asks
//! `cron_add` for a job whose `allowed_tools` names `file_write`.
//!
//! THIS TEST MUST FAIL if the stored tool set stops being capped. Neutralize it
//! by making `CronAddTool::cap_allowed_tools` return its argument unchanged, and
//! the stored job regains `file_write`.
//!
//! Both halves are asserted, because under a cap that stored nothing at all the
//! negative half would hold for the wrong reason:
//!   - negative: the stored list must NOT contain `file_write`;
//!   - positive: it MUST contain `cron_add`, which proves the job was really
//!     created through the bounded target's rebuilt tool rather than the whole
//!     path failing somewhere earlier.
//!
//! The third test closes the loop the first two leave open: it REPLAYS the
//! stored job through the scheduler and follows the escape one hop further. A
//! capped stored list only helps if the turn the scheduler starts from it is
//! itself bounded — including the `spawn_subagent` in that turn's registry,
//! which is what a replayed job would otherwise use to assemble a child from
//! the owning agent's full policy.
//!
//! What none of these assert: `cron_run`'s refusal, a different property
//! covered by the unit tests of `tools::caller_ceiling`.

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

/// The tool the caller was never granted and the target would otherwise
/// smuggle into a scheduled job.
const BEYOND_CEILING: &str = "file_write";

/// A tool both sides hold, so the job is created with a non-empty stored list
/// and the positive half of the assertion has something to observe.
const WITHIN_CEILING: &str = "cron_add";

const JOB_NAME: &str = "bounded-scheduled-work";

/// Planted in the `spawn_subagent` prompt the REPLAYED job issues, so the
/// child's own model request can be located by content instead of by turn
/// ordering — the chain crosses a persistence boundary, so counting turns is
/// especially fragile here.
const CHILD_MARKER: &str = "ZC-CRON-CHILD-MARKER";

/// The stored job's prompt. Used as the needle that recognises the REPLAYED
/// turn, so the reply that makes it spawn is addressed by content instead of by
/// position.
const JOB_PROMPT: &str = "ZC-CRON-JOB-PROMPT carry out the scheduled work";

#[derive(Clone)]
struct Script {
    calls: Arc<AtomicUsize>,
    captured: Arc<AsyncMutex<Vec<String>>>,
    /// Canned replies, consumed in order. Once exhausted every further turn
    /// answers plainly so each loop in the chain unwinds instead of hanging.
    replies: Arc<Vec<String>>,
    /// Content-addressed replies, checked BEFORE the positional ones:
    /// `(must_contain, must_not_contain, reply)`.
    ///
    /// Positional replies cannot reach a turn that happens after a persistence
    /// boundary: the first run consumes them by index, so a reply meant for a
    /// later scheduler replay is served to whatever request happens to land
    /// third. The negative needle is what stops a rule from firing again on the
    /// same loop's follow-up request, which carries the previous tool call —
    /// arguments included — in its history.
    content_rules: Arc<Vec<(String, String, String)>>,
    /// Gate for `content_rules`: they apply only once the test has entered the
    /// scheduler-replay phase.
    ///
    /// Content alone cannot separate the phases. The job's prompt travels inside
    /// the `cron_add` arguments, so it reappears in the FIRST run's tool-call
    /// history and a content rule keyed on it fires there too — spawning a child
    /// that is correctly bounded and shadows the real one. The phase boundary is
    /// structural, so the gate is structural.
    replaying: Arc<std::sync::atomic::AtomicBool>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-cron",
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
        "id": "chatcmpl-cron",
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
    if script.replaying.load(Ordering::SeqCst) {
        for (needle, anti_needle, reply) in script.content_rules.iter() {
            if body.contains(needle.as_str()) && !body.contains(anti_needle.as_str()) {
                script.captured.lock().await.push(body);
                return reply.clone();
            }
        }
    }
    script.captured.lock().await.push(body);
    script
        .replies
        .get(call)
        .cloned()
        .unwrap_or_else(|| plain_content("done"))
}

/// The `cron_add` call the deepest loop of each chain makes. It asks for a tool
/// its caller never held, which is the whole point of the assertion.
fn schedule_out_of_ceiling_job() -> String {
    native_tool_call(
        "cron_add",
        &serde_json::json!({
            "name": JOB_NAME,
            "schedule": {"kind": "cron", "expr": "*/5 * * * *"},
            "prompt": JOB_PROMPT,
            // `spawn_subagent` is requested and IS within the ceiling, so it
            // survives the cap and reaches the stored list. That is deliberate:
            // the replay test needs the job to be able to spawn, which is the
            // hop where the ceiling was being dropped.
            "allowed_tools": [BEYOND_CEILING, WITHIN_CEILING, "spawn_subagent"],
        })
        .to_string(),
    )
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

async fn spawn_stub_provider(replies: Vec<String>) -> (SocketAddr, Arc<AsyncMutex<Vec<String>>>) {
    let (addr, captured, _) = spawn_stub_provider_with_rules(replies, Vec::new()).await;
    (addr, captured)
}

async fn spawn_stub_provider_with_rules(
    replies: Vec<String>,
    content_rules: Vec<(String, String, String)>,
) -> (
    SocketAddr,
    Arc<AsyncMutex<Vec<String>>>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let script = Script {
        calls: Arc::new(AtomicUsize::new(0)),
        captured: Arc::new(AsyncMutex::new(Vec::new())),
        replies: Arc::new(replies),
        content_rules: Arc::new(content_rules),
        replaying: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let captured = Arc::clone(&script.captured);
    let replaying = Arc::clone(&script.replaying);
    let app = Router::new()
        .route("/chat/completions", post(handle_chat))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    zeroclaw_spawn::spawn!(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    (addr, captured, replaying)
}

fn bounded_cron_config(provider_uri: &str, root: &std::path::Path) -> Config {
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
    // The caller can delegate and schedule, but cannot write files. This is the
    // ceiling the scheduled job must not exceed.
    risk_profiles.insert(
        "caller_profile".to_string(),
        permissive(vec![
            "delegate".to_string(),
            "spawn_subagent".to_string(),
            WITHIN_CEILING.to_string(),
        ]),
    );
    // The target's own profile is wider. Without the cap, the job it schedules
    // inherits THIS, which is the defect.
    risk_profiles.insert(
        "target_profile".to_string(),
        permissive(vec![
            "spawn_subagent".to_string(),
            "delegate".to_string(),
            WITHIN_CEILING.to_string(),
            BEYOND_CEILING.to_string(),
        ]),
    );
    // The agent a REPLAYED job delegates to. Its own profile grants the
    // out-of-ceiling tool, so a bounded delegation whose ceiling is computed from
    // the wrong set hands it over.
    risk_profiles.insert(
        "sub_profile".to_string(),
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
            delegates: vec![DelegateTargetConfig {
                agent: "sub".to_string(),
                mode: DelegateExecutionMode::Bounded,
            }],
            ..AliasedAgentConfig::default()
        },
    );
    agents.insert(
        "sub".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "sub_profile".into(),
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

/// Returns the stored `allowed_tools` of the job the bounded target scheduled,
/// plus a report string for assertion failures.
async fn drive_bounded_cron_add(replies: Vec<String>) -> (Option<Vec<String>>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, _captured) = spawn_stub_provider(replies).await;
    let config = bounded_cron_config(&format!("http://{addr}"), tmp.path());
    // `agent::run` consumes the config; the store is read back through this copy.
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

/// Nested delegation stacks futures past the harness default per-thread stack.
fn drive_blocking(replies: Vec<String>) -> (Option<Vec<String>>, String) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        zeroclaw_spawn::spawn!(drive_bounded_cron_add(replies))
            .await
            .expect("chain task joins")
    })
}

/// Shared by both chains: the stored list must have been capped, and must not
/// be empty or absent — either of which the scheduler reads as unrestricted,
/// so the negative half alone would hold for exactly the wrong reason.
fn assert_stored_within_ceiling(stored: Option<Vec<String>>, report: &str, chain: &str) {
    let stored = stored.unwrap_or_else(|| {
        panic!(
            "[{chain}] no job with an allowed_tools list was stored; the negative \
             assertion would hold vacuously. {report}"
        )
    });
    assert!(
        stored.iter().any(|t| t == WITHIN_CEILING),
        "[{chain}] the admitted tool did not survive the cap, so the job was not \
         really scheduled through the bounded path; stored={stored:?}; {report}"
    );
    assert!(
        !stored.iter().any(|t| t == BEYOND_CEILING),
        "[{chain}] a job scheduled from inside a bounded delegate stored \
         `{BEYOND_CEILING}`, which the caller was never granted: the ceiling did not \
         survive to the persisted job; stored={stored:?}; {report}"
    );
}

#[test]
fn a_job_scheduled_from_a_bounded_delegate_stores_only_tools_within_the_caller_ceiling() {
    let (stored, report) = drive_blocking(vec![
        native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"schedule the follow-up"}"#,
        ),
        schedule_out_of_ceiling_job(),
    ]);
    assert_stored_within_ceiling(stored, &report, "caller -> bounded target -> cron_add");
}

/// The composition the two review-requested regressions do not reach.
///
/// `spawn_subagent` now carries the sealed set, so the nested child's own
/// registry is correctly bounded — that is this PR's other repair. But the
/// child can still SCHEDULE, and a stored job is read back by the scheduler
/// long after every one of those in-turn bounds is gone. Capping the child's
/// registry therefore does not cap what the child persists: without the stored
/// cap, this chain defeats the very fix that bounds the hop before it.
///
/// THIS TEST MUST FAIL if `cron_add` stops capping the stored list, even while
/// the direct `caller -> target -> cron_add` chain above is still repaired.
#[test]
fn a_job_scheduled_from_a_spawned_child_of_a_bounded_delegate_inherits_the_same_ceiling() {
    let (stored, report) = drive_blocking(vec![
        native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"hand this to a subagent"}"#,
        ),
        native_tool_call("spawn_subagent", r#"{"prompt":"schedule the follow-up"}"#),
        schedule_out_of_ceiling_job(),
    ]);
    assert_stored_within_ceiling(
        stored,
        &report,
        "caller -> bounded target -> spawn_subagent -> cron_add",
    );
}

/// Runs the bounded chain to create the job, then REPLAYS that job through the
/// scheduler and returns the tool set offered to the child the replayed turn
/// spawns.
async fn drive_bounded_cron_then_replay() -> (Option<BTreeSet<String>>, String) {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured, replaying) = spawn_stub_provider_with_rules(
        vec![
            native_tool_call(
                "delegate",
                r#"{"action":"delegate","agent":"target","prompt":"schedule the follow-up"}"#,
            ),
            schedule_out_of_ceiling_job(),
        ],
        // Addressed by content, not position: this reply belongs to a turn that
        // happens AFTER the first run has ended, so no index can reach it. It
        // fires on the replayed job's own turn (which carries the stored prompt)
        // and not on that turn's follow-up, which also carries the prompt but
        // has the child marker in its tool-call history by then.
        vec![(
            JOB_PROMPT.to_string(),
            CHILD_MARKER.to_string(),
            native_tool_call(
                "spawn_subagent",
                &serde_json::json!({ "prompt": format!("{CHILD_MARKER} do the nested work") })
                    .to_string(),
            ),
        )],
    )
    .await;
    let config = bounded_cron_config(&format!("http://{addr}"), tmp.path());
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

    // The scheduler replay. This is the hop the other two tests stop before:
    // the turn below is started by the scheduler from the STORED list, long
    // after every in-turn bound of the chain above is gone.
    // Everything captured so far belongs to the first run. Only turns after this
    // index can be the replay's, and content alone cannot tell them apart: the
    // job prompt and the child prompt both reappear in the first run's tool-call
    // history. The boundary is what makes the observation unambiguous.
    let first_run_turns = captured.lock().await.len();
    replaying.store(true, Ordering::SeqCst);

    let replay = match job.as_ref() {
        Some(job) => Some(zeroclaw_runtime::cron::scheduler::execute_job_now(&reader, job).await),
        None => None,
    };

    let bodies = captured.lock().await.clone();
    let child_turn = bodies
        .iter()
        .skip(first_run_turns)
        .find(|body| body.contains(CHILD_MARKER))
        .map(|body| offered_tools(body));
    let report = format!(
        "first_run {first:?}; stored {stored:?}; replay {replay:?}; turns={}; per-turn tools {:?}",
        bodies.len(),
        bodies.iter().map(|b| offered_tools(b)).collect::<Vec<_>>()
    );
    (child_turn, report)
}

/// A job scheduled from a bounded target cannot store `delegate`, and that is
/// what keeps the delegate hop out of the replay entirely.
///
/// `delegate` is stripped from a bounded target's registry, so it never reaches
/// the sealed set the cap intersects against — even when the model asks for it
/// and the target's own profile and roster would allow it. This test sets up
/// exactly that: `target_profile` grants `delegate`, `target` has a `delegates`
/// roster, and the request names it. It still must not be stored.
///
/// The property matters beyond bookkeeping. A bounded delegation computes its
/// target's ceiling from the delegate's `parent_tools` filtered by the caller's
/// POLICY, which is wider than a per-run allowlist. A replayed job that could
/// hold `delegate` would therefore delegate from the wide set. The assembly now
/// narrows that parent set by the ceiling as well, but the reason no reachable
/// escape exists today is this stripping — so this is the test that must go red
/// if the stripping ever changes.
///
/// Honest limit, and it is different from the other three in this file: **the
/// red was not demonstrated.** Neutralizing the bounded path's `tool.name() !=
/// Self::NAME` filter — the only name-based strip on that path — did NOT make
/// `delegate` reach the stored list, so something further along keeps it out
/// too and this test does not isolate which. It pins the observable property,
/// not a named mechanism, and the assertions below are worth exactly that.
///
/// So treat a failure here as a signal to go find that mechanism, not as proof
/// that one specific line regressed. The in-flight work to honour
/// `delegation_policy` for bounded targets is the change most likely to trip
/// it; if it does, re-check the delegate parent-set narrowing in
/// `ScopedToolRegistry::assemble` and model the replay chain this file's other
/// tests use for `spawn_subagent`.
#[test]
fn a_job_scheduled_from_a_bounded_delegate_cannot_store_delegate() {
    let (stored, report) = drive_blocking(vec![
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
                "allowed_tools": [WITHIN_CEILING, "delegate"],
            })
            .to_string(),
        ),
    ]);

    // Positive half: the job exists and kept the admitted tool, so the absence
    // below is a real exclusion rather than a job that was never created.
    let stored =
        stored.unwrap_or_else(|| panic!("no job with an allowed_tools list was stored; {report}"));
    assert!(
        stored.iter().any(|t| t == WITHIN_CEILING),
        "the admitted tool did not survive, so the job was not scheduled through the \
         bounded path; stored={stored:?}; {report}"
    );

    assert!(
        !stored.iter().any(|t| t == "delegate"),
        "a job scheduled from a bounded target stored `delegate`, so a scheduler \
         replay could delegate from the caller's policy-filtered parent set instead \
         of the job's own allowlist; stored={stored:?}; {report}"
    );
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

/// The escape that survives the turn.
///
/// Capping the stored list bounds the FIRST turn the scheduler starts from it.
/// It does not, on its own, bound what that turn can spawn: the eager registry
/// builds `spawn_subagent`, and if that construction drops the ceiling the
/// replayed turn assembles a child from the owning agent's full policy — so the
/// bound is restored for one turn and lost on the next.
///
/// THIS TEST MUST FAIL if `all_tools_with_runtime` stops forwarding its
/// `caller_ceiling` to `spawn_subagent_tool`. Neutralize by passing `None`
/// there and the child regains `BEYOND_CEILING`, while both tests above stay
/// green — which is exactly why neither of them catches this.
#[test]
fn a_child_spawned_by_a_replayed_bounded_job_stays_within_the_original_ceiling() {
    let (child_turn, report) = drive_replay_blocking();

    // Positive half first: the replay must have reached a child turn at all.
    let offered = child_turn.unwrap_or_else(|| {
        panic!(
            "no model request carried the child marker, so the scheduler replay never \
             spawned a child and the ceiling assertion below would hold vacuously. \
             {report}"
        )
    });
    assert!(
        offered.contains(WITHIN_CEILING),
        "the child of the replayed job was offered nothing from within the ceiling, so \
         the absence of `{BEYOND_CEILING}` proves nothing; offered={offered:?}; {report}"
    );

    assert!(
        !offered.contains(BEYOND_CEILING),
        "a child spawned by a REPLAYED bounded job was offered `{BEYOND_CEILING}`, which \
         the original caller never held: the ceiling survived into the stored job but not \
         into the turn the scheduler started from it; offered={offered:?}; {report}"
    );
}
