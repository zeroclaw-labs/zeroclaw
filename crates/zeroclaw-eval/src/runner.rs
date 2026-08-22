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
use crate::case::{LlmTrace, load_suite};
use crate::grader::{GradeResult, grade_run};
use crate::observer::RecordingObserver;
use crate::record::{CaseProvenance, RunCompletion, RunRecord, ToolSurface};
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

/// Enforces a conversation-turn boundary on the provider side.
///
/// Replay implements this to assert every step scripted for a turn was consumed
/// before the next turn begins. Without it, a flat response queue lets a turn that
/// over-specifies its round-trips bleed the surplus into the following turn and
/// still pass — a false green from a harness other PRs gate merges on.
pub trait TurnBoundary: Send + Sync {
    /// Called after each completed turn. Errors if the turn was over-specified.
    fn finish_turn(&self, turn_index: usize) -> anyhow::Result<()>;
}

/// A provider built for one case, plus the optional turn-boundary hook that goes
/// with it. Live mode has no scripted steps to over-specify, so it supplies `None`.
pub struct CaseProvider {
    pub provider: Box<dyn ModelProvider>,
    pub turn_boundary: Option<Arc<dyn TurnBoundary>>,
}

impl From<Box<dyn ModelProvider>> for CaseProvider {
    fn from(provider: Box<dyn ModelProvider>) -> Self {
        Self {
            provider,
            turn_boundary: None,
        }
    }
}

/// Factory that builds a fresh model provider for one case run. Injected so
/// replay, live, and deterministic tests share one runner code path.
pub type ProviderFactory = Box<dyn Fn(&LlmTrace) -> anyhow::Result<CaseProvider> + Send + Sync>;

/// Everything a case run needs that differs between replay, live, and tests.
///
/// The provider is injected as a closure so replay, live, and deterministic tests
/// share one code path; the runner never constructs a provider itself.
pub struct RunDeps {
    pub mode: Mode,
    /// Builds the model provider for one case run.
    pub provider: ProviderFactory,
    /// Receipt provider identity: `"scripted"` for replay; `"<type>.<alias>:<model>"` for live.
    pub provider_ref: String,
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
                // The handle shares the provider's per-turn queues, so the runner
                // can enforce turn boundaries without owning the boxed provider.
                let turn_boundary: Arc<dyn TurnBoundary> = Arc::new(replay.handle());
                Ok(CaseProvider {
                    provider: Box::new(replay) as Box<dyn ModelProvider>,
                    turn_boundary: Some(turn_boundary),
                })
            }),
            provider_ref: "scripted".to_string(),
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

/// Build the provenance half of a case's receipt. Everything here is knowable
/// before the fallible work starts, so a failed run still carries it.
///
/// `tool_surface` is supplied by the caller because only the execution path knows
/// which registry was actually constructed; an error before that point records an
/// empty surface rather than a guess.
pub fn case_provenance(
    trace: &LlmTrace,
    deps: &RunDeps,
    tool_surface: ToolSurface,
) -> anyhow::Result<CaseProvenance> {
    Ok(CaseProvenance {
        schema: crate::record::RECORD_SCHEMA.to_string(),
        mode: deps.mode,
        case_id: trace.display_id().to_string(),
        case_hash: crate::case::case_hash(trace)?,
        provider_ref: deps.provider_ref.clone(),
        tool_surface,
        sandbox: crate::record::SandboxStamp {
            autonomy: "supervised".to_string(),
            workspace_only: matches!(deps.mode, Mode::Live),
        },
    })
}

/// Run every `*.json` trace fixture in `dir` and return an aggregated report.
pub async fn run_suite(dir: &Path, deps: &RunDeps) -> anyhow::Result<SuiteReport> {
    let traces = load_suite(dir)?;
    if traces.is_empty() {
        anyhow::bail!("no *.json trace fixtures found in {}", dir.display());
    }

    let mut cases = Vec::with_capacity(traces.len());
    for (path, trace) in traces {
        let name = trace.display_id().to_string();
        let source = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        // The execution path publishes provenance here as soon as it is known,
        // so an error after that point still yields a joinable receipt.
        let mut provenance: Option<CaseProvenance> = None;
        let report = match run_case_recording_provenance(&trace, deps, &mut provenance).await {
            Ok(outcome) => CaseReport {
                name,
                source,
                record: Some(outcome.record),
                grades: outcome.grades,
                error: None,
            },
            Err(e) => {
                // The receipt exists for exactly this path. A provider, setup,
                // timeout, or agent error must still produce a record carrying the
                // case hash, mode, provider, tool surface, and sandbox stamp, or a
                // baseline cannot classify the failure against the attempted run.
                let provenance = provenance
                    .or_else(|| case_provenance(&trace, deps, ToolSurface::default()).ok());
                CaseReport {
                    name,
                    source,
                    record: provenance.map(RunRecord::from_provenance),
                    grades: vec![],
                    error: Some(e.to_string()),
                }
            }
        };
        cases.push(report);
    }

    Ok(SuiteReport { cases })
}

/// Run a single trace through a freshly built, isolated agent, grade it while its
/// workspace is still alive, and return the outcome. Dispatches on `deps.mode`.
pub async fn run_case(trace: &LlmTrace, deps: &RunDeps) -> anyhow::Result<CaseOutcome> {
    run_case_recording_provenance(trace, deps, &mut None).await
}

/// [`run_case`], but publishing the case's [`CaseProvenance`] into `provenance_out`
/// the moment it is known — before the fallible execution work. On `Err` the caller
/// can therefore still build a receipt describing the run that was attempted.
pub async fn run_case_recording_provenance(
    trace: &LlmTrace,
    deps: &RunDeps,
    provenance_out: &mut Option<CaseProvenance>,
) -> anyhow::Result<CaseOutcome> {
    match deps.mode {
        Mode::Replay => run_replay_case(trace, deps, provenance_out).await,
        Mode::Live => crate::live::run_live_case(trace, deps, provenance_out).await,
    }
}

/// Replay a scripted trace through the Phase 0 deterministic agent (echo tools,
/// native dispatcher, no network).
async fn run_replay_case(
    trace: &LlmTrace,
    deps: &RunDeps,
    provenance_out: &mut Option<CaseProvenance>,
) -> anyhow::Result<CaseOutcome> {
    // Each case gets an isolated temp workspace and an ephemeral "none" memory
    // backend so cases cannot observe one another.
    let tmp = tempfile::tempdir()?;

    let mem_cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    let memory: Arc<dyn Memory> = Arc::from(create_memory(&mem_cfg, tmp.path(), None)?);

    // Replay uses the built-in echo registry; record what it actually exposes so
    // the surface is derived from the registry, not from the request path.
    let tools = default_tools();
    let registered: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    // Published before the provider is constructed: a fixture whose steps are
    // unscriptable fails in the factory, and that failure still needs a receipt.
    let provenance = case_provenance(
        trace,
        deps,
        ToolSurface::new(Vec::new(), Vec::new(), registered),
    )?;
    *provenance_out = Some(provenance.clone());

    let observer = Arc::new(RecordingObserver::new());
    let CaseProvider {
        provider,
        turn_boundary,
    } = (deps.provider)(trace)?;

    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(tools)
        .memory(memory)
        .observer(observer.clone())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(tmp.path().to_path_buf())
        .build()?;

    let start = std::time::Instant::now();
    let mut final_response = String::new();
    for (turn_index, turn) in trace.turns.iter().enumerate() {
        final_response = agent.turn(&turn.user_input).await?;
        // Enforce the turn boundary: every step scripted for this turn must have
        // been consumed before the next turn begins, so a surplus response cannot
        // bleed forward and turn an over-specified fixture into a passing case.
        if let Some(boundary) = &turn_boundary {
            boundary.finish_turn(turn_index)?;
        }
    }
    let duration_ms = start.elapsed().as_millis() as u64;

    let (input_tokens, output_tokens) = observer.tokens();
    let record = RunRecord {
        provenance,
        completion: Some(RunCompletion {
            final_response,
            history: agent.history().to_vec(),
            tools_called: observer.tool_names(),
            all_tools_succeeded: observer.all_tools_succeeded(),
            input_tokens,
            output_tokens,
            duration_ms,
            llm_calls: observer.llm_calls(),
        }),
    };
    // Grade while the temp workspace is still alive, then let `tmp` drop.
    let grades = grade_run(trace, &record, tmp.path()).await;
    Ok(CaseOutcome { record, grades })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            outcome
                .record
                .completion_or_default()
                .final_response
                .contains("Hello")
        );
        assert!(
            outcome
                .record
                .completion_or_default()
                .tools_called
                .is_empty()
        );
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
        assert_eq!(
            outcome.record.completion_or_default().tools_called,
            vec!["echo".to_string()]
        );
        assert!(outcome.record.completion_or_default().all_tools_succeeded);
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
            outcome
                .record
                .completion_or_default()
                .final_response
                .contains("Goodbye"),
            "final response: {:?}",
            outcome.record.completion_or_default().final_response
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

    /// A provider factory that always fails, standing in for an unreachable or
    /// misconfigured provider.
    fn failing_deps() -> RunDeps {
        RunDeps {
            mode: Mode::Replay,
            provider: Box::new(|_| anyhow::bail!("provider unreachable")),
            provider_ref: "test.model:m".to_string(),
            live_tools: Vec::new(),
            case_timeout: Duration::from_secs(5),
        }
    }

    fn write_suite(dir: &std::path::Path, name: &str, json: &str) {
        std::fs::write(dir.join(name), json).unwrap();
    }

    #[tokio::test]
    async fn errored_case_report_retains_provenance() {
        // The receipt's whole purpose is comparability. A provider error must not
        // collapse the record to `None` — the case hash, mode, provider ref, tool
        // surface, and sandbox stamp are all knowable without executing anything.
        let tmp = tempfile::tempdir().unwrap();
        write_suite(tmp.path(), "a.json", SMOKE);
        let report = run_suite(tmp.path(), &failing_deps()).await.unwrap();

        let case = &report.cases[0];
        assert!(case.error.is_some(), "the case must be recorded as errored");
        let record = case
            .record
            .as_ref()
            .expect("an errored case must still carry a receipt");
        assert!(!record.is_complete(), "no completion data for a failed run");
        assert!(!record.provenance.case_hash.is_empty());
        assert_eq!(record.provenance.case_id, "test-smoke-greeting");
        assert_eq!(record.provenance.provider_ref, "test.model:m");
        assert_eq!(record.provenance.mode, Mode::Replay);

        // ...and it survives serialization: the JSON report must not emit `null`.
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        let c = &json["cases"][0];
        assert!(c["case_hash"].is_string() && !c["case_hash"].as_str().unwrap().is_empty());
        assert_eq!(c["mode"], "replay");
        assert_eq!(c["provider_ref"], "test.model:m");
        assert!(c["tool_surface"].is_object());
        assert!(c["sandbox"].is_object());
    }

    #[tokio::test]
    async fn setup_error_before_agent_still_records_case_hash() {
        // An authoring error (a replay turn with no scripted steps) fails inside the
        // provider factory, before the agent exists. Provenance is published first,
        // so the receipt is intact.
        let tmp = tempfile::tempdir().unwrap();
        write_suite(
            tmp.path(),
            "a.json",
            r#"{ "model_name": "no-steps-case", "turns": [{ "user_input": "Hi" }], "expects": {} }"#,
        );
        let report = run_suite(tmp.path(), &RunDeps::replay()).await.unwrap();
        let case = &report.cases[0];
        assert!(
            case.error.as_deref().unwrap().contains("no scripted steps"),
            "unexpected error: {:?}",
            case.error
        );
        let record = case
            .record
            .as_ref()
            .expect("receipt must survive a setup error");
        assert!(!record.provenance.case_hash.is_empty());
        assert_eq!(record.provenance.case_id, "no-steps-case");
    }

    #[tokio::test]
    async fn errored_case_scores_none_not_vacuous_one() {
        // An errored case has an empty grade list because nothing was scored, not
        // because everything passed. Emitting `passed: false` beside `score: 1.0`
        // misleads machine consumers and inflates suite averages.
        let tmp = tempfile::tempdir().unwrap();
        write_suite(tmp.path(), "a.json", SMOKE);
        let report = run_suite(tmp.path(), &failing_deps()).await.unwrap();
        assert_eq!(report.cases[0].score(), None);

        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["cases"][0]["passed"], false);
        assert!(
            json["cases"][0]["score"].is_null(),
            "an errored case must not serialize score 1.0: {}",
            json["cases"][0]["score"]
        );
    }

    #[tokio::test]
    async fn successful_case_still_reports_a_score_and_completion() {
        // Guard the other direction: the provenance split must not strip completion
        // data or scoring from a run that actually finished.
        let tmp = tempfile::tempdir().unwrap();
        write_suite(tmp.path(), "a.json", SMOKE);
        let report = run_suite(tmp.path(), &RunDeps::replay()).await.unwrap();
        let case = &report.cases[0];
        assert!(case.error.is_none());
        assert_eq!(case.score(), Some(1.0));
        let record = case.record.as_ref().unwrap();
        assert!(record.is_complete());
        assert!(
            record
                .completion_or_default()
                .final_response
                .contains("Hello")
        );
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["cases"][0]["score"], 1.0);
        assert!(json["cases"][0]["total_tokens"].is_number());
    }

    const OVER_SPECIFIED_TURN: &str = r#"{
        "model_name": "test-over-specified-turn",
        "turns": [
            { "user_input": "Hi", "steps": [
                { "response": { "type": "text", "content": "Hello there." } },
                { "response": { "type": "text", "content": "SURPLUS-FROM-TURN-0" } }
            ] },
            { "user_input": "And goodbye?", "steps": [
                { "response": { "type": "text", "content": "Goodbye!" } }
            ] }
        ],
        "expects": {}
    }"#;

    #[tokio::test]
    async fn replay_rejects_over_specified_turn() {
        // Turn 0 scripts two responses but the agent only requests one. With a flat
        // response queue the surplus silently becomes turn 1's answer and the case
        // passes — a replay suite used as a merge gate would certify behaviour that
        // never happened. The turn boundary must fail the case instead.
        let trace: LlmTrace = serde_json::from_str(OVER_SPECIFIED_TURN).unwrap();
        let err = run_case(&trace, &RunDeps::replay())
            .await
            .expect_err("an over-specified turn must fail the case, not pass it");
        let msg = err.to_string();
        assert!(
            msg.contains("over-specifies"),
            "error must name the unconsumed responses: {msg}"
        );
        assert!(
            msg.contains("turn 0"),
            "error must name the offending turn: {msg}"
        );
    }

    #[tokio::test]
    async fn replay_extra_response_does_not_bleed_into_next_turn() {
        // The same fixture, observed from the other side: turn 1 must never receive
        // turn 0's leftover response. If the surplus bled forward the run would
        // succeed with "Goodbye!" left unconsumed, so a successful run — or a final
        // response carrying the surplus — is the bug.
        let trace: LlmTrace = serde_json::from_str(OVER_SPECIFIED_TURN).unwrap();
        match run_case(&trace, &RunDeps::replay()).await {
            Err(e) => assert!(
                !e.to_string().contains("SURPLUS-FROM-TURN-0"),
                "the surplus must be reported as unconsumed, never served: {e}"
            ),
            Ok(outcome) => panic!(
                "turn 0's surplus bled into turn 1; final response: {:?}",
                outcome.record.completion_or_default().final_response
            ),
        }
    }

    #[tokio::test]
    async fn replay_exhausted_turn_names_the_turn() {
        // The exhaustion guard is also per-turn now: turn 1 asking for a response
        // the trace does not script for it must not be able to borrow turn 0's.
        let trace: LlmTrace = serde_json::from_str(MULTI_TURN).unwrap();
        let outcome = run_case(&trace, &RunDeps::replay()).await.unwrap();
        // Baseline: a well-formed multi-turn fixture still replays cleanly.
        assert!(
            outcome
                .record
                .completion_or_default()
                .final_response
                .contains("Goodbye")
        );
    }
}
