//! The runner: builds an isolated agent per case, drives it, and grades it.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_config::schema::MemoryConfig;
use zeroclaw_memory::{Memory, create_memory};
use zeroclaw_runtime::agent::agent::Agent;
use zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher;

use crate::Mode;
use crate::case::{LlmTrace, load_suite_entries};
use crate::grader::{GradeResult, grade_run};
use crate::observer::RecordingObserver;
use crate::record::RunRecord;
use crate::report::{CaseReport, SuiteReport};
use crate::tools::default_tools;

/// A completed case run plus its grades, produced while the case's temp
/// workspace is still alive. The workspace itself is intentionally not carried
/// here (it is dropped once grading finishes).
#[derive(Debug)]
pub struct CaseOutcome {
    pub record: RunRecord,
    pub grades: Vec<GradeResult>,
}

/// A per-turn boundary hook invoked by the runner after each `Agent::turn`,
/// receiving the just-finished turn index. Replay uses it to assert the turn
/// consumed all of its scripted steps; live mode has no such contract and passes
/// `None`.
pub type TurnBoundary = Box<dyn Fn(usize) -> anyhow::Result<()> + Send + Sync>;

/// Everything one case run needs from its provider: the boxed provider itself
/// plus the optional per-turn boundary hook that must share state with it.
///
/// The two travel together because the hook is only meaningful for the specific
/// provider instance built for this case — the runner cannot reach inside a boxed
/// `dyn ModelProvider` to recover it.
pub struct ProviderSetup {
    pub provider: Box<dyn ModelProvider>,
    pub on_turn_end: Option<TurnBoundary>,
}

impl ProviderSetup {
    /// A provider with no per-turn boundary contract (live mode, tests).
    pub fn new(provider: Box<dyn ModelProvider>) -> Self {
        Self {
            provider,
            on_turn_end: None,
        }
    }

    /// A provider that enforces a per-turn boundary after every `Agent::turn`.
    pub fn with_turn_boundary(provider: Box<dyn ModelProvider>, on_turn_end: TurnBoundary) -> Self {
        Self {
            provider,
            on_turn_end: Some(on_turn_end),
        }
    }
}

/// Factory that builds a fresh model provider for one case run. Injected so
/// replay, live, and deterministic tests share one runner code path.
pub type ProviderFactory = Box<dyn Fn(&LlmTrace) -> anyhow::Result<ProviderSetup> + Send + Sync>;

/// Everything a case run needs that differs between replay, live, and tests.
///
/// The provider is injected as a closure so replay, live, and deterministic tests
/// share one code path; the runner never constructs a provider itself.
pub struct RunDeps {
    pub mode: Mode,
    /// Builds the model provider for one case run.
    pub provider: ProviderFactory,
    /// Config tool allowlist for live runs; intersected per case with `case.tools`.
    pub live_tools: Vec<String>,
    /// Wall-clock timeout applied per conversation turn in live mode.
    pub case_timeout: Duration,
}

impl RunDeps {
    /// A replay-mode `RunDeps`: the provider replays each trace's scripted steps.
    /// Live-only fields take inert defaults.
    pub fn replay() -> Self {
        Self {
            mode: Mode::Replay,
            provider: Box::new(|trace| {
                let replay = crate::replay::TraceLlmProvider::try_from_trace(trace)?;
                // The handle shares the provider's per-turn queues, so the runner can
                // assert consumption at each turn boundary without owning the box.
                let handle = replay.handle();
                Ok(ProviderSetup::with_turn_boundary(
                    Box::new(replay) as Box<dyn ModelProvider>,
                    Box::new(move |turn_index| handle.finish_turn(turn_index)),
                ))
            }),
            live_tools: Vec::new(),
            case_timeout: Duration::from_secs(120),
        }
    }
}

/// Guard: live mode needs a configured provider reference. An empty ref yields a
/// clear error naming `[eval].live_provider`, raised before any case runs.
pub fn ensure_live_provider(provider_ref: &str) -> anyhow::Result<()> {
    if provider_ref.trim().is_empty() {
        anyhow::bail!(
            "live mode requires [eval].live_provider (dotted providers.models reference, e.g. \"anthropic.sonnet\")"
        );
    }
    Ok(())
}

/// Run every `*.json` trace fixture in `dir` and return an aggregated report.
pub async fn run_suite(dir: &Path, deps: &RunDeps) -> anyhow::Result<SuiteReport> {
    let traces = load_suite_entries(dir)?;
    if traces.is_empty() {
        anyhow::bail!("no *.json trace fixtures found in {}", dir.display());
    }

    let mut cases = Vec::with_capacity(traces.len());
    for (path, trace) in traces {
        let source = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        // A fixture that fails to parse or validate becomes a named FAILED case with
        // zero grades. `CaseReport::passed()` is false whenever `error` is set, so a
        // malformed fixture can never render green.
        let trace = match trace {
            Ok(trace) => trace,
            Err(e) => {
                cases.push(CaseReport {
                    name: source.clone(),
                    source,
                    grades: vec![],
                    error: Some(format!("{e:#}")),
                });
                continue;
            }
        };

        let name = trace.display_id().to_string();

        let report = match run_case(&trace, deps).await {
            Ok(outcome) => CaseReport {
                name,
                source,
                grades: outcome.grades,
                error: None,
            },
            Err(e) => CaseReport {
                name,
                source,
                grades: vec![],
                error: Some(e.to_string()),
            },
        };
        cases.push(report);
    }

    Ok(SuiteReport { cases })
}

/// Run a single trace through a freshly built, isolated agent, grade it while its
/// workspace is still alive, and return the outcome. Dispatches on `deps.mode`.
pub async fn run_case(trace: &LlmTrace, deps: &RunDeps) -> anyhow::Result<CaseOutcome> {
    match deps.mode {
        Mode::Replay => run_replay_case(trace, deps).await,
        Mode::Live => crate::live::run_live_case(trace, deps).await,
    }
}

/// Replay a scripted trace through the Phase 0 deterministic agent (echo tools,
/// native dispatcher, no network).
async fn run_replay_case(trace: &LlmTrace, deps: &RunDeps) -> anyhow::Result<CaseOutcome> {
    if trace.declares_memory() {
        anyhow::bail!("replay cases cannot seed or grade memory; run this case with --mode live");
    }

    // Each case gets an isolated temp workspace and an ephemeral "none" memory
    // backend so cases cannot observe one another.
    let tmp = tempfile::tempdir()?;

    let mem_cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    let memory: Arc<dyn Memory> = Arc::from(create_memory(&mem_cfg, tmp.path(), None)?);

    let observer = Arc::new(RecordingObserver::new());
    let ProviderSetup {
        provider,
        on_turn_end,
    } = (deps.provider)(trace)?;

    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(default_tools())
        .memory(memory)
        .observer(observer.clone())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(tmp.path().to_path_buf())
        .build()?;

    let start = std::time::Instant::now();
    let mut final_response = String::new();
    for (turn_index, turn) in trace.turns.iter().enumerate() {
        final_response = agent.turn(&turn.user_input).await?;
        // Enforce the turn boundary: every step scripted for this turn must have been
        // consumed before the next turn begins, so responses cannot bleed across turns.
        if let Some(on_turn_end) = &on_turn_end {
            on_turn_end(turn_index)?;
        }
    }
    let duration_ms = start.elapsed().as_millis() as u64;

    let (input_tokens, output_tokens) = observer.tokens();
    let record = RunRecord {
        final_response,
        history: agent.history().to_vec(),
        tools_called: observer.tool_names(),
        all_tools_succeeded: observer.all_tools_succeeded(),
        input_tokens,
        output_tokens,
        duration_ms,
        llm_calls: observer.llm_calls(),
    };
    // Grade while the temp workspace is still alive, then let `tmp` drop.
    let grades = grade_run(trace, &record, tmp.path(), None).await;
    Ok(CaseOutcome { record, grades })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting_replay_deps(provider_calls: Arc<AtomicUsize>) -> RunDeps {
        RunDeps {
            mode: Mode::Replay,
            provider: Box::new(move |_| {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("provider must not be invoked for rejected replay case")
            }),
            live_tools: Vec::new(),
            case_timeout: Duration::from_secs(5),
        }
    }

    const SMOKE: &str = r#"{
        "model_name": "test-smoke-greeting",
        "turns": [{
            "user_input": "Hello, how are you?",
            "steps": [{ "response": { "type": "text", "content": "Hello! I'm doing well.", "input_tokens": 20, "output_tokens": 15 } }]
        }],
        "expects": { "response_contains": ["Hello"], "response_not_contains": ["error"], "max_tool_calls": 0 }
    }"#;

    const ECHO: &str = r#"{
        "model_name": "test-single-tool-echo",
        "turns": [{
            "user_input": "Echo hello for me",
            "steps": [
                { "response": { "type": "tool_calls", "tool_calls": [{ "id": "call_1", "name": "echo", "arguments": {"message": "hello"} }], "input_tokens": 30, "output_tokens": 15 } },
                { "response": { "type": "text", "content": "The echo tool said: hello", "input_tokens": 50, "output_tokens": 10 } }
            ]
        }],
        "expects": { "response_contains": ["hello"], "tools_used": ["echo"], "max_tool_calls": 1, "all_tools_succeeded": true }
    }"#;

    #[tokio::test]
    async fn replays_text_only_trace() {
        let trace: LlmTrace = serde_json::from_str(SMOKE).unwrap();
        let outcome = run_case(&trace, &RunDeps::replay()).await.unwrap();
        assert!(outcome.record.final_response.contains("Hello"));
        assert!(outcome.record.tools_called.is_empty());
        assert!(
            outcome.grades.iter().all(|g| g.passed),
            "grades: {:?}",
            outcome.grades
        );
    }

    #[tokio::test]
    async fn replays_tool_call_trace() {
        let trace: LlmTrace = serde_json::from_str(ECHO).unwrap();
        let outcome = run_case(&trace, &RunDeps::replay()).await.unwrap();
        assert_eq!(outcome.record.tools_called, vec!["echo".to_string()]);
        assert!(outcome.record.all_tools_succeeded);
        assert!(
            outcome.grades.iter().all(|g| g.passed),
            "grades: {:?}",
            outcome.grades
        );
    }

    const MULTI_TURN: &str = r#"{
        "model_name": "test-multi-turn",
        "turns": [
            { "user_input": "Hi", "steps": [{ "response": { "type": "text", "content": "Hello there." } }] },
            { "user_input": "And goodbye?", "steps": [{ "response": { "type": "text", "content": "Goodbye!" } }] }
        ],
        "expects": {}
    }"#;

    #[tokio::test]
    async fn replays_multi_turn_trace_in_order() {
        let trace: LlmTrace = serde_json::from_str(MULTI_TURN).unwrap();
        let outcome = run_case(&trace, &RunDeps::replay()).await.unwrap();
        // The final response comes from the *last* turn, proving turns replay in order.
        assert!(
            outcome.record.final_response.contains("Goodbye"),
            "final response: {:?}",
            outcome.record.final_response
        );
    }

    #[tokio::test]
    async fn replay_turn_without_steps_errors() {
        // A replay turn with no scripted steps is an authoring error surfaced by
        // the fallible constructor before the agent runs.
        let trace: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "test-no-steps", "turns": [{ "user_input": "Hi" }], "expects": {} }"#,
        )
        .unwrap();
        let err = run_case(&trace, &RunDeps::replay()).await.unwrap_err();
        assert!(
            err.to_string().contains("no scripted steps"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn replay_rejects_memory_setup_before_provider_invocation() {
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "memory-setup",
                "turns": [],
                "setup": { "memory": { "project/role": "zeroclaw_operator" } }
            }"#,
        )
        .unwrap();
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let deps = counting_replay_deps(provider_calls.clone());

        let error = run_case(&trace, &deps).await.unwrap_err();

        assert!(
            error.to_string().contains("--mode live"),
            "unexpected error: {error}"
        );
        assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn replay_rejects_memory_expectations_before_provider_invocation() {
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "memory-expects",
                "turns": [],
                "expects": { "memory": { "present": ["project/role"] } }
            }"#,
        )
        .unwrap();
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let deps = counting_replay_deps(provider_calls.clone());

        let error = run_case(&trace, &deps).await.unwrap_err();

        assert!(
            error.to_string().contains("--mode live"),
            "unexpected error: {error}"
        );
        assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn live_mode_without_provider_config_errors() {
        // Empty [eval].live_provider is rejected before any case runs, with an
        // error that names the config key the operator must set.
        let err = ensure_live_provider("   ").unwrap_err();
        assert!(
            err.to_string().contains("[eval].live_provider"),
            "error must name the config key: {err}"
        );
        assert!(ensure_live_provider("anthropic.sonnet").is_ok());
    }

    #[tokio::test]
    async fn malformed_memory_fixture_does_not_report_passing() {
        // End-to-end guard for B1: a fixture whose `expects.memory` asserts nothing
        // must not render green. Before validation, the MemoryGrader emitted zero
        // grades and `all_passed()` (which is `all()` over an empty vec) was true.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("vacuous.json"),
            r#"{
                "model_name": "vacuous-memory",
                "turns": [{ "user_input": "Hi", "steps": [{ "response": { "type": "text", "content": "Hello." } }] }],
                "expects": { "memory": {} }
            }"#,
        )
        .unwrap();

        let report = run_suite(dir.path(), &RunDeps::replay()).await.unwrap();

        assert!(
            !report.all_passed(),
            "a fixture that asserts nothing must not pass: {report:?}"
        );
        assert_eq!(report.cases.len(), 1);
        let case = &report.cases[0];
        assert_eq!(case.source, "vacuous.json");
        let error = case.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("expects.memory"),
            "the failure should name the offending block: {error}"
        );
    }

    #[tokio::test]
    async fn malformed_workspace_fixture_does_not_report_passing() {
        // Same guard for the `WorkspaceExpects` / `file_contains` mirror, using the
        // always-true empty-string needle shape.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("vacuous_ws.json"),
            r#"{
                "model_name": "vacuous-workspace",
                "turns": [{ "user_input": "Hi", "steps": [{ "response": { "type": "text", "content": "Hello." } }] }],
                "expects": { "workspace": { "file_contains": { "out.txt": [""] } } }
            }"#,
        )
        .unwrap();

        let report = run_suite(dir.path(), &RunDeps::replay()).await.unwrap();

        assert!(
            !report.all_passed(),
            "an always-true needle must not pass: {report:?}"
        );
        let error = report.cases[0].error.as_deref().unwrap_or_default();
        assert!(
            error.contains("empty-string needle"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn well_formed_fixture_still_passes_through_run_suite() {
        // Control for the two guards above: validation must not turn healthy
        // fixtures red.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("smoke.json"), SMOKE).unwrap();
        let report = run_suite(dir.path(), &RunDeps::replay()).await.unwrap();
        assert!(report.all_passed(), "{report:?}");
    }

    // --- B2: the replay per-turn consumption boundary ---

    #[tokio::test]
    async fn over_specified_turn_is_an_error() {
        // The turn declares two steps but the agent makes a single chat() call, so the
        // extra step is left unconsumed. Under a flat queue this passed silently;
        // it must surface as a turn-scoped error rather than bleed into a next turn.
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "test-over-specified",
                "turns": [{ "user_input": "Hi", "steps": [
                    { "response": { "type": "text", "content": "Hello there." } },
                    { "response": { "type": "text", "content": "unused extra step" } }
                ] }],
                "expects": {}
            }"#,
        )
        .unwrap();
        let err = match run_case(&trace, &RunDeps::replay()).await {
            Ok(_) => panic!("expected an error: an over-specified turn left a step unconsumed"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("over-specifies") || msg.contains("never requested"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn leftover_step_does_not_leak_into_next_turn() {
        // Turn 1 scripts two steps and the agent consumes one. With a single flat
        // queue, turn 2 would silently receive turn 1's leftover ("turn one leftover")
        // as its response, shifting the whole trace one step out of phase while still
        // reporting green. The boundary must fail the run at turn 1 instead.
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "test-cross-turn-leak",
                "turns": [
                    { "user_input": "Hi", "steps": [
                        { "response": { "type": "text", "content": "Hello there." } },
                        { "response": { "type": "text", "content": "turn one leftover" } }
                    ] },
                    { "user_input": "And goodbye?", "steps": [
                        { "response": { "type": "text", "content": "Goodbye!" } }
                    ] }
                ],
                "expects": {}
            }"#,
        )
        .unwrap();

        let err = match run_case(&trace, &RunDeps::replay()).await {
            Ok(outcome) => panic!(
                "expected an error; turn 1's leftover leaked into turn 2: {:?}",
                outcome.record.final_response
            ),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("turn 0")
                && (msg.contains("over-specifies") || msg.contains("never requested")),
            "the error should name the offending turn: {msg}"
        );
    }

    #[tokio::test]
    async fn under_specified_turn_is_scoped_to_its_own_turn() {
        // Turn 1 scripts one step and turn 2 scripts none of its own beyond the first
        // response the agent needs; exhausting the current turn's queue must not fall
        // through to a later turn's steps.
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "test-under-specified",
                "turns": [
                    { "user_input": "Echo hello for me", "steps": [
                        { "response": { "type": "tool_calls", "tool_calls": [{ "id": "call_1", "name": "echo", "arguments": {"message": "hello"} }] } }
                    ] },
                    { "user_input": "And goodbye?", "steps": [
                        { "response": { "type": "text", "content": "Goodbye!" } }
                    ] }
                ],
                "expects": {}
            }"#,
        )
        .unwrap();

        let err = run_case(&trace, &RunDeps::replay())
            .await
            .expect_err("turn 0 needs a follow-up response it does not script");
        let msg = err.to_string();
        assert!(
            msg.contains("turn 0") && msg.contains("more LLM responses"),
            "the exhaustion error should be scoped to turn 0: {msg}"
        );
    }
}
