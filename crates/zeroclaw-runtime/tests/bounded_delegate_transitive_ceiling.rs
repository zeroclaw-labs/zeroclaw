//! The bounded-delegation tool ceiling must survive a CHANGE OF MECHANISM,
//! not just depth.
//!
//! `docs/book/src/agents/delegation.md` states the contract: "the caller's
//! registry is the ceiling: a bounded cross-profile target whose risk profile
//! names a tool the caller was never granted does not receive it."
//!
//! A bounded target does not receive `delegate` (both delegation paths strip
//! it from the child registry), but it DOES receive a rebuilt
//! `spawn_subagent`, built with `is_subagent_caller = false` so the depth-1
//! refusal does not fire. That tool re-enters `agent::run`, which assembles a
//! fresh registry for the target's own alias. The chain exercised here is:
//!
//! ```text
//! caller --delegate(bounded)--> target --spawn_subagent--> nested child
//! ```
//!
//! THIS TEST MUST FAIL while `spawn_subagent` calls `agent::run` with its
//! per-run `allowed_tools` argument set to `None`: the nested child is then
//! assembled with `caller_allowed: None`, so the target's own risk profile
//! restores `file_write` - a tool the caller was never granted. Neutralize any
//! future fix by putting that argument back to `None` and this test must go
//! red again.
//!
//! The assertion is deliberately index-free: `file_write` is absent from the
//! CALLER's profile, so no level of the chain may ever be offered it. Reading
//! the tool set off each captured provider request body is what makes the
//! nested level observable at all - `agent::run` builds its own model provider
//! with no injection seam, so a local stub server stands in for the model, the
//! same way the crate's own end-to-end `run()` tests do.

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

/// A tool the CALLER's risk profile never grants and the TARGET's does. Its
/// appearance anywhere in the chain is the leak.
const CEILING_BREACH_TOOL: &str = "file_write";

/// Granted to both profiles, so the positive half of the pair has something to
/// assert: a ceiling that simply denied everything would also hide the breach.
const SHARED_TOOL: &str = "calculator";

/// Substring shared by every tool a TARGET-side skill contributes. Skills are
/// registered AFTER the built-in policy filter runs, so they are the concrete
/// case of "added after assembly" the ceiling has to cover too.
const SKILL_MARKER: &str = "ceilingprobe";

/// Skill name the manifest declares. Registration namespaces every tool a
/// skill contributes as `<skill>__<tool>`, so the names that actually reach a
/// registry are the two below - NOT the bare names in the manifest.
const SKILL_NAME: &str = "ceilingprobe";

/// Registered name of the plain skill-contributed tool.
const SKILL_TOOL: &str = "ceilingprobe__ceilingprobe_plain";

/// Registered name of a skill tool whose manifest name is itself shaped like an
/// MCP `<server>__<tool>` identifier, so the registered name carries TWO `__`
/// separators. Every skill tool already looks like an MCP name after
/// namespacing; a ceiling that admitted on string shape rather than identity
/// would therefore auto-admit the entire skill surface, not just MCP tools.
const SKILL_TOOL_DOUBLE_UNDERSCORE: &str = "ceilingprobe__srv__ceilingprobe_shaped";

#[derive(Clone)]
struct Script {
    captured: Arc<AsyncMutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

fn native_tool_call(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-ceiling",
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
        "id": "chatcmpl-ceiling",
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

/// Drives the chain: first turn delegates, second spawns a subagent, every
/// later turn answers plainly so the nested run and both unwinds terminate.
async fn handle_chat(State(script): State<Script>, body: String) -> String {
    let call = script.calls.fetch_add(1, Ordering::SeqCst);
    script.captured.lock().await.push(body);
    match call {
        0 => native_tool_call(
            "delegate",
            r#"{"action":"delegate","agent":"target","prompt":"work the subtask"}"#,
        ),
        1 => native_tool_call("spawn_subagent", r#"{"prompt":"nested subtask"}"#),
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

/// Tool names a captured request offered the model, read off the OpenAI-style
/// `tools[].function.name` array the compatible wire sends.
fn offered_tool_names(body: &str) -> BTreeSet<String> {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return BTreeSet::new(),
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

/// Writes a skill the TARGET agent owns and the caller does not, contributing
/// two tools whose names differ only in shape.
fn plant_target_skill(target_workspace: &std::path::Path) {
    let skill_dir = target_workspace.join("skills").join(SKILL_NAME);
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    let manifest = format!(
        r#"
[skill]
name = "{SKILL_NAME}"
description = "target-owned skill used to probe the delegation ceiling"

[[tools]]
name = "ceilingprobe_plain"
description = "probe tool contributed by a target-owned skill"
kind = "shell"
command = "echo probe"

[[tools]]
name = "srv__ceilingprobe_shaped"
description = "probe tool whose name is shaped like an MCP identifier"
kind = "shell"
command = "echo probe"
"#
    );
    std::fs::write(skill_dir.join("SKILL.toml"), manifest).expect("skill manifest");
}

fn ceiling_config(provider_uri: &str, root: &std::path::Path, caller_extra: &[&str]) -> Config {
    let mut providers = zeroclaw_config::providers::Providers::default();
    {
        let base = providers
            .models
            .ensure("custom", "default")
            .expect("`custom` slot must exist on ModelProviders");
        base.api_key = Some("test-key".to_string());
        base.model = Some("test-model".to_string());
        base.uri = Some(provider_uri.to_string());
        // Native tool calling puts the offered tool set in the request's
        // `tools` array, which is what makes each level's registry observable.
        base.native_tools = Some(true);
    }

    let mut caller_allowed_tools = vec![
        "delegate".to_string(),
        "spawn_subagent".to_string(),
        SHARED_TOOL.to_string(),
    ];
    caller_allowed_tools.extend(caller_extra.iter().map(|name| (*name).to_string()));

    let mut risk_profiles = HashMap::new();
    risk_profiles.insert(
        "caller_profile".to_string(),
        RiskProfileConfig {
            // No `file_write`: this list IS the ceiling under test.
            allowed_tools: caller_allowed_tools,
            delegation_policy: DelegationPolicy {
                mode: DelegationMode::Allow,
            },
            ..RiskProfileConfig::default()
        },
    );
    risk_profiles.insert(
        "target_profile".to_string(),
        RiskProfileConfig {
            // Strictly broader than the caller's on purpose.
            allowed_tools: vec![
                CEILING_BREACH_TOOL.to_string(),
                "spawn_subagent".to_string(),
                SHARED_TOOL.to_string(),
                SKILL_TOOL.to_string(),
                SKILL_TOOL_DOUBLE_UNDERSCORE.to_string(),
            ],
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
    let target_workspace = root.join("target-workspace");
    plant_target_skill(&target_workspace);
    agents.insert(
        "target".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "custom.default".into(),
            risk_profile: "target_profile".into(),
            runtime_profile: "agentic".into(),
            workspace: zeroclaw_config::multi_agent::AgentWorkspaceConfig {
                path: Some(target_workspace.clone()),
                ..Default::default()
            },
            ..AliasedAgentConfig::default()
        },
    );

    let mut config = Config {
        data_dir: root.to_path_buf(),
        config_path: root.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        runtime_profiles,
        ..Config::default()
    };
    config.reliability.scheduler_retries = 0;
    config.reliability.provider_retries = 0;
    config.skills.allow_scripts = true;
    config
}

/// Every request the chain sent the model, as offered-tool-name sets.
async fn drive_chain(caller_extra: &[&str]) -> Vec<BTreeSet<String>> {
    let tmp = TempDir::new().expect("temp root");
    let (addr, captured) = spawn_stub_provider().await;
    let config = ceiling_config(&format!("http://{addr}"), tmp.path(), caller_extra);

    let _ = zeroclaw_runtime::agent::run(
        config,
        "caller",
        Some("hand the subtask to the target agent".to_string()),
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

    let bodies = captured.lock().await;
    bodies.iter().map(|b| offered_tool_names(b)).collect()
}

/// Runs the chain on a worker thread with a raised stack. Nested delegation
/// stacks `agent::run` -> tool loop -> `delegate` -> agentic sub-loop ->
/// `spawn_subagent` -> `agent::run` -> tool loop, and those futures are large
/// enough that the test harness's default per-test thread stack overflows
/// before any assertion runs.
fn drive_chain_blocking(caller_extra: &[&str]) -> Vec<BTreeSet<String>> {
    let owned: Vec<String> = caller_extra.iter().map(|n| (*n).to_string()).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(64 * 1024 * 1024)
        .build()
        .expect("test runtime builds");
    runtime.block_on(async move {
        zeroclaw_spawn::spawn!(async move {
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            drive_chain(&refs).await
        })
        .await
        .expect("chain task joins")
    })
}

/// Tool names the NESTED CHILD - the deepest level, reached by crossing from
/// `delegate` to `spawn_subagent` - was offered.
fn nested_child_tools(levels: &[BTreeSet<String>]) -> &BTreeSet<String> {
    assert!(
        levels.len() >= DESCENT_LEVELS,
        "chain did not reach the nested level: only {} model requests, sets {levels:?}",
        levels.len()
    );
    &levels[DESCENT_LEVELS - 1]
}

#[test]
fn nested_spawn_subagent_cannot_restore_a_tool_the_caller_never_had() {
    let levels = drive_chain_blocking(&[]);

    assert!(
        levels.len() >= 3,
        "chain did not reach the nested level: only {} model requests were made, sets {levels:?}",
        levels.len()
    );

    // Positive half. Without it a ceiling that denied EVERYTHING would satisfy
    // the negative assertion below while proving nothing about reconstruction.
    assert!(
        levels[0].contains(SHARED_TOOL),
        "caller was not offered its own granted tool; sets {levels:?}"
    );
    assert!(
        levels.iter().any(|names| names.contains(SHARED_TOOL)),
        "no level kept the shared granted tool; sets {levels:?}"
    );

    // Negative half. `file_write` is outside the caller's profile, so the
    // ceiling forbids it at EVERY level - including the one `agent::run`
    // assembles for the nested child.
    let breached: Vec<usize> = levels
        .iter()
        .enumerate()
        .filter(|(_, names)| names.contains(CEILING_BREACH_TOOL))
        .map(|(index, _)| index)
        .collect();
    assert!(
        breached.is_empty(),
        "levels {breached:?} were offered `{CEILING_BREACH_TOOL}`, which the caller never had; \
         sets {levels:?}"
    );
}

/// The descent is the first `DESCENT_LEVELS` requests: the script answers with
/// a tool call exactly twice, so request 0 is the caller, 1 the bounded target
/// and 2 the nested child. Later requests are the chain unwinding back up,
/// where the set legitimately grows again.
const DESCENT_LEVELS: usize = 3;

#[test]
fn the_offered_tool_set_never_grows_while_descending() {
    // Monotonicity rather than a fixed-depth assertion: `max_delegation_depth`
    // is configurable with no upper clamp, so "the child of the child does not
    // have X" would only ever pin one point of an unbounded chain. Proved for
    // the base case and the step, this covers N levels.
    //
    // MUST FAIL while a nested assembly rebuilds from the target's own profile
    // without carrying the caller's ceiling: the set then grows at the step
    // where the mechanism changes from `delegate` to `spawn_subagent`.
    let levels = drive_chain_blocking(&[]);

    assert!(
        levels.len() >= DESCENT_LEVELS,
        "chain did not reach the nested level: only {} model requests, sets {levels:?}",
        levels.len()
    );

    for step in 1..DESCENT_LEVELS {
        let parent = &levels[step - 1];
        let child = &levels[step];
        let grown: Vec<&String> = child.difference(parent).collect();
        assert!(
            grown.is_empty(),
            "descending from level {} to {step} ADDED {grown:?}; a ceiling may only narrow. \
             sets {levels:?}",
            step - 1
        );
    }
}

#[test]
fn nested_child_cannot_gain_a_skill_tool_outside_the_caller_ceiling() {
    // Skill tools are registered AFTER the built-in policy filter, and the only
    // subtraction at that final boundary is the target's own `excluded_tools`.
    //
    // MUST FAIL while nothing re-applies the caller ceiling to tools added
    // after assembly: the nested child then loads the target's own skills and
    // is offered names the caller never had.
    //
    // The second name is deliberately shaped like an MCP `<server>__<tool>`
    // identifier: a ceiling implemented on the risk-profile branch of the tool
    // access policy would auto-admit any name containing `__`, so this half is
    // what distinguishes an identity check from a shape check.
    let levels = drive_chain_blocking(&[]);
    let child = nested_child_tools(&levels);

    let leaked: Vec<&String> = child
        .iter()
        .filter(|name| name.contains(SKILL_MARKER))
        .collect();
    assert!(
        leaked.is_empty(),
        "the nested child was offered target-only skill tools {leaked:?}; sets {levels:?}"
    );
    // Named explicitly, not just matched by marker: the second name is shaped
    // like an MCP `<server>__<tool>` identifier, and a ceiling that admitted on
    // string shape rather than on identity would let precisely that one back in
    // while the plain sibling stayed out.
    assert!(
        !child.contains(SKILL_TOOL) && !child.contains(SKILL_TOOL_DOUBLE_UNDERSCORE),
        "a target-owned skill tool reached the nested child; sets {levels:?}"
    );
}

#[test]
fn the_nested_child_still_receives_a_tool_the_ceiling_admits() {
    // The other half of the pair above, and the guard against a vacuous pass:
    // a ceiling that emptied the nested registry outright would satisfy every
    // absence assertion in this file while destroying the feature.
    //
    // This asserts on the CHILD specifically. The base case covers the first
    // hop; nothing else covers the level reached by crossing from `delegate`
    // to `spawn_subagent`, which is the one the ceiling had to be carried to.
    //
    // MUST FAIL if the ceiling is implemented as a denial of the nested
    // assembly rather than as a narrowing of it.
    //
    // It replaces an earlier case that asserted a target-owned SKILL tool
    // named by the ceiling would reach the child. That case was unsatisfiable
    // by design, not by defect: the bounded assembly is built with no skills
    // at all, and skill tools are not reusable across the boundary either, so
    // no skill tool can reach a bounded target or anything below it. The
    // absence half of that pair survives, above, because it is what the
    // contract actually requires.
    let levels = drive_chain_blocking(&[]);
    let child = nested_child_tools(&levels);

    assert!(
        child.contains(SHARED_TOOL),
        "the nested child lost `{SHARED_TOOL}`, which the caller grants and the ceiling admits;          sets {levels:?}"
    );
}

#[test]
fn the_bounded_target_itself_is_already_bounded_by_the_caller() {
    // Base case of the induction, and a guard on behaviour that already holds:
    // the FIRST hop is bounded correctly today. It is separated from the nested
    // assertions so that a regression here is distinguishable from a regression
    // in the transitive step - the two have different causes and different fixes.
    //
    // MUST FAIL if the immediate rebuild is neutralized and the target simply
    // inherits the caller's registry, or if the target's own broader profile is
    // allowed to widen the first hop.
    let levels = drive_chain_blocking(&[]);
    assert!(
        levels.len() >= 2,
        "chain never reached the bounded target: sets {levels:?}"
    );
    let target = &levels[1];

    assert!(
        !target.contains(CEILING_BREACH_TOOL),
        "the bounded target was offered `{CEILING_BREACH_TOOL}`, outside the caller ceiling; \
         sets {levels:?}"
    );
    // Positive half: a ceiling that denied everything would satisfy the line
    // above while proving nothing about reconstruction.
    assert!(
        target.contains(SHARED_TOOL),
        "the bounded target lost `{SHARED_TOOL}`, which BOTH profiles grant; sets {levels:?}"
    );
}
