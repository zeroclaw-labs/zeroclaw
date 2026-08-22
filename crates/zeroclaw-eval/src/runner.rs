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
use crate::record::RunRecord;
use crate::replay::ReplayHandle;
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

/// Factory that builds a fresh model provider for one case run. Injected so
/// replay, live, and deterministic tests share one runner code path.
pub type ProviderFactory = Box<dyn Fn(&LlmTrace) -> anyhow::Result<CaseProvider> + Send + Sync>;

/// The provider for one case run, plus the optional replay handle the runner needs
/// to police turn boundaries.
///
/// The provider itself is opaque to the runner (it is injected as a boxed
/// `ModelProvider`), so the per-turn consumption contract cannot be enforced through
/// the trait. Replay therefore hands the runner a [`ReplayHandle`] alongside the
/// provider; live mode has no scripted steps and supplies `None`.
pub struct CaseProvider {
    pub provider: Box<dyn ModelProvider>,
    pub replay: Option<ReplayHandle>,
}

impl CaseProvider {
    /// A provider with no replay turn-boundary contract (live mode, test doubles).
    pub fn plain(provider: Box<dyn ModelProvider>) -> Self {
        Self {
            provider,
            replay: None,
        }
    }
}

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
                let handle = replay.handle();
                Ok(CaseProvider {
                    provider: Box::new(replay) as Box<dyn ModelProvider>,
                    replay: Some(handle),
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

        let report = match run_case(&trace, deps).await {
            Ok(outcome) => CaseReport {
                name,
                source,
                record: Some(outcome.record),
                grades: outcome.grades,
                error: None,
            },
            Err(e) => CaseReport {
                name,
                source,
                record: None,
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
    // Each case gets an isolated temp workspace and an ephemeral "none" memory
    // backend so cases cannot observe one another.
    let tmp = tempfile::tempdir()?;

    let mem_cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    let memory: Arc<dyn Memory> = Arc::from(create_memory(&mem_cfg, tmp.path(), None)?);

    let observer = Arc::new(RecordingObserver::new());
    let CaseProvider {
        provider,
        replay: replay_handle,
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
        if let Some(handle) = &replay_handle {
            handle.finish_turn(turn_index)?;
        }
    }
    let duration_ms = start.elapsed().as_millis() as u64;

    let (input_tokens, output_tokens) = observer.tokens();
    let record = RunRecord {
        schema: crate::record::RECORD_SCHEMA.to_string(),
        mode: Mode::Replay,
        case_id: trace.display_id().to_string(),
        case_hash: crate::case::case_hash(trace)?,
        provider_ref: deps.provider_ref.clone(),
        tool_surface: Vec::new(),
        sandbox: crate::record::SandboxStamp {
            autonomy: "supervised".to_string(),
            workspace_only: false,
        },
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
    async fn over_specified_turn_is_an_error() {
        // The turn declares two steps but the agent makes a single chat() call, so the
        // extra step is left unconsumed. Under a flat cross-turn queue this passed
        // silently; it must now surface as a turn-scoped case error rather than bleed
        // into the next turn (and, via --write-baseline, freeze a wrong transcript).
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
    async fn turn_two_never_replays_turn_ones_leftover_response() {
        // End-to-end turn-boundary check through the real runner: turn 0 scripts an
        // extra step that the agent never requests, and turn 1 scripts its own reply.
        // A flattened queue would hand turn 0's leftover to turn 1, produce
        // "LEAKED-FROM-TURN-ONE" as the final response, and still report Ok.
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "test-cross-turn-bleed",
                "turns": [
                    { "user_input": "first", "steps": [
                        { "response": { "type": "text", "content": "first reply" } },
                        { "response": { "type": "text", "content": "LEAKED-FROM-TURN-ONE" } }
                    ] },
                    { "user_input": "second", "steps": [
                        { "response": { "type": "text", "content": "second reply" } }
                    ] }
                ],
                "expects": {}
            }"#,
        )
        .unwrap();
        match run_case(&trace, &RunDeps::replay()).await {
            Ok(outcome) => panic!(
                "cross-turn bleed went undetected; final response was {:?}",
                outcome.record.final_response
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("over-specifies") || msg.contains("never requested"),
                    "the run must fail at the turn boundary, not later: {msg}"
                );
                assert!(
                    msg.contains("turn 0"),
                    "the error must name the offending turn: {msg}"
                );
            }
        }
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
}
