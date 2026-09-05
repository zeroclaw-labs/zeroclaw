//! The runner: builds an isolated agent per case, drives it, and grades it.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_config::schema::MemoryConfig;
use zeroclaw_memory::{Memory, create_memory};
use zeroclaw_runtime::agent::agent::Agent;
use zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher;
use zeroclaw_runtime::i18n::get_required_cli_string_with_args;

use crate::Mode;
use crate::case::{LlmTrace, load_suite};
use crate::grader::{GradeResult, Grader, default_graders, grade_with};
use crate::observer::RecordingObserver;
use crate::record::{
    CaseProvenance, RunCompletion, RunRecord, ToolSurface, duration_millis_saturating,
};
use crate::report::{CaseReport, SuiteReport};
use crate::tools::default_tools;

/// Callback the runner invokes after each conversation turn completes, in replay
/// mode only, to assert the turn's scripted steps were fully consumed (over-spec
/// guard) and advance the replay cursor. `None` for live and other non-replay
/// providers, which have no per-turn script to exhaust.
pub type FinishTurnFn = Box<dyn Fn(usize) -> anyhow::Result<()> + Send + Sync>;

/// A model provider for one case run, plus the metadata the runner needs to wire
/// it into the agent correctly. Bundling this (rather than a bare boxed provider)
/// lets the factory carry configured-model metadata (item 2) and the replay
/// per-turn boundary handle (item 3) across the `RunDeps` closure boundary.
pub struct CaseProvider {
    pub provider: Box<dyn ModelProvider>,
    /// Sets `Agent::builder().model_provider_name(..)` when present.
    pub provider_name: Option<String>,
    /// Sets `Agent::builder().model_name(..)` when present; this is the value
    /// passed to every `provider.chat` call for the built agent.
    pub model_name: Option<String>,
    /// Replay-only per-turn exhaustion boundary; `None` for live.
    pub finish_turn: Option<FinishTurnFn>,
}

impl CaseProvider {
    /// Wrap a bare provider with no metadata and no replay boundary.
    pub fn from_provider(provider: Box<dyn ModelProvider>) -> Self {
        Self {
            provider,
            provider_name: None,
            model_name: None,
            finish_turn: None,
        }
    }
}

/// Factory that builds a fresh model provider (plus metadata) for one case run.
/// Injected so replay, live, and deterministic tests share one runner code path.
pub type ProviderFactory = Box<dyn Fn(&LlmTrace) -> anyhow::Result<CaseProvider> + Send + Sync>;

/// A completed case run plus its grades, produced while the case's temp
/// workspace is still alive. The workspace itself is intentionally not carried
/// here (it is dropped once grading finishes).
#[derive(Debug)]
pub struct CaseOutcome {
    pub record: RunRecord,
    pub grades: Vec<GradeResult>,
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
    /// Config tool allowlist for live runs; intersected per case with
    /// `case.tools` by `live::effective_live_tools`, which then drops any
    /// tool in `live::LIVE_TOOL_DENYLIST` (e.g. `shell`) regardless of
    /// whether it appears here.
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
                let provider = crate::replay::TraceLlmProvider::try_from_trace(trace)?;
                let handle = provider.handle();
                Ok(CaseProvider {
                    provider: Box::new(provider),
                    provider_name: None,
                    model_name: None,
                    finish_turn: Some(Box::new(move |turn_index| handle.finish_turn(turn_index))),
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
        let mut provenance = Some(case_provenance(&trace, deps, ToolSurface::default())?);
        let report = match run_case_repeated_recording_provenance(&trace, deps, &mut provenance)
            .await
        {
            Ok((outcome, repeat)) => {
                // A truncated repetition set must not pass: the representative's
                // grades only describe the repetitions that completed, so a
                // case whose completed runs all passed would otherwise clear
                // pass^k on fewer than k runs. Surface the truncation as the
                // case error (which counts as a failure) while keeping the
                // record, grades, and partial statistics as evidence.
                let error = repeat.as_ref().and_then(|r| {
                    r.truncated().then(|| {
                        let detail = r.error.as_deref().unwrap_or("repetition did not complete");
                        format!(
                            "repeat {}/{} runs completed (pass^k not established): {detail}",
                            r.completed, r.k
                        )
                    })
                });
                CaseReport {
                    name,
                    source,
                    record: Some(outcome.record),
                    grades: outcome.grades,
                    error,
                    repeat,
                    cluster: trace.cluster.clone(),
                }
            }
            Err(e) => {
                let (k, _) = crate::stats::effective_repeat(deps.mode, trace.repeat);
                // The receipt exists for exactly this path. A provider, setup,
                // timeout, or agent error must still produce a record carrying the
                // case hash, mode, provider, tool surface, and sandbox stamp, or a
                // baseline cannot classify the failure against the attempted run.
                let Some(provenance) = provenance else {
                    anyhow::bail!("case provenance disappeared after initialization");
                };
                CaseReport {
                    name,
                    source,
                    record: Some(RunRecord::from_provenance(provenance)),
                    grades: vec![],
                    error: Some(e.to_string()),
                    repeat: (k > 1).then(|| {
                        crate::stats::RepeatStats::from_partial_runs(k, &[], e.to_string())
                    }),
                    cluster: trace.cluster.clone(),
                }
            }
        };
        cases.push(report);
    }

    Ok(SuiteReport { cases })
}

/// Run a case, repeating it for live suites with `repeat > 1`. Each repeat is a
/// fully isolated run (fresh temp workspace, agent, and provider). Returns a
/// representative outcome (a failing run when any exists, so its grades explain
/// the failure; otherwise the first run) plus aggregated [`RepeatStats`]. The
/// representative's grades make the case pass iff every run passed (pass^k).
///
/// If a repetition errors, the repetitions already completed are still
/// aggregated and returned rather than discarded: live runs are paid, and the
/// evidence from a partial set is what makes the aggregate disputable. The
/// error is retained on the stats, and the case still fails `pass^k` because
/// the missing repetitions never count as passes. The error only propagates
/// when it leaves no completed repetition to report.
pub async fn run_case_repeated(
    trace: &LlmTrace,
    deps: &RunDeps,
) -> anyhow::Result<(CaseOutcome, Option<crate::stats::RepeatStats>)> {
    run_case_repeated_recording_provenance(trace, deps, &mut None).await
}

async fn run_case_repeated_recording_provenance(
    trace: &LlmTrace,
    deps: &RunDeps,
    provenance_out: &mut Option<CaseProvenance>,
) -> anyhow::Result<(CaseOutcome, Option<crate::stats::RepeatStats>)> {
    let (k, warnings) = crate::stats::effective_repeat(deps.mode, trace.repeat);
    for w in &warnings {
        let case = trace.display_id();
        let warning = match w {
            crate::stats::RepeatWarning::ClampedLow => {
                zeroclaw_runtime::i18n::get_required_cli_string(
                    "cli-eval-repeat-warning-clamped-low",
                )
            }
            crate::stats::RepeatWarning::ClampedHigh { requested } => {
                let requested = requested.to_string();
                get_required_cli_string_with_args(
                    "cli-eval-repeat-warning-clamped-high",
                    &[("requested", requested.as_str())],
                )
            }
            crate::stats::RepeatWarning::ReplayIgnored => {
                zeroclaw_runtime::i18n::get_required_cli_string(
                    "cli-eval-repeat-warning-replay-ignored",
                )
            }
        };
        eprintln!(
            "{}",
            get_required_cli_string_with_args(
                "cli-eval-repeat-warning",
                &[("case", case), ("warning", warning.as_str())],
            )
        );
    }
    if k <= 1 {
        return Ok((
            run_case_recording_provenance(trace, deps, provenance_out).await?,
            None,
        ));
    }
    let mut outcomes = Vec::with_capacity(k as usize);
    let mut run_error: Option<String> = None;
    for i in 0..k {
        let mut attempt_provenance = None;
        match run_case_recording_provenance(trace, deps, &mut attempt_provenance).await {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => {
                // Keep the evidence from repetitions 0..i and stop; with no
                // completed repetition there is nothing to report, so the error
                // propagates as an errored case instead.
                if outcomes.is_empty() {
                    *provenance_out = attempt_provenance;
                    return Err(e);
                }
                let case = trace.display_id();
                let attempt = (i + 1).to_string();
                let total = k.to_string();
                let completed = outcomes.len().to_string();
                let error = e.to_string();
                eprintln!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-eval-repeat-error",
                        &[
                            ("case", case),
                            ("attempt", attempt.as_str()),
                            ("total", total.as_str()),
                            ("completed", completed.as_str()),
                            ("error", error.as_str()),
                        ],
                    )
                );
                run_error = Some(error);
                break;
            }
        }
    }
    let all_pass = |o: &CaseOutcome| o.grades.iter().all(|g| g.passed);
    let samples: Vec<crate::stats::RunSample> = outcomes
        .iter()
        .map(|o| {
            let completion = o.record.completion_or_default();
            crate::stats::RunSample {
                passed: all_pass(o),
                input_tokens: completion.input_tokens,
                output_tokens: completion.output_tokens,
                duration_ms: completion.duration_ms,
                llm_calls: completion.llm_calls,
                checks: o
                    .grades
                    .iter()
                    .map(|g| (g.check.clone(), g.passed))
                    .collect(),
            }
        })
        .collect();
    let stats = match run_error {
        Some(e) => crate::stats::RepeatStats::from_partial_runs(k, &samples, e),
        None => crate::stats::RepeatStats::from_runs(k, &samples),
    };
    let rep_idx = outcomes.iter().position(|o| !all_pass(o)).unwrap_or(0);
    let representative = outcomes.swap_remove(rep_idx);
    Ok((representative, Some(stats)))
}

/// Run a single trace through a freshly built, isolated agent, grade it while its
/// workspace is still alive, and return the outcome. Dispatches on `deps.mode`.
pub async fn run_case(trace: &LlmTrace, deps: &RunDeps) -> anyhow::Result<CaseOutcome> {
    run_case_with_graders(trace, deps, default_graders(trace)).await
}

/// Injection seam: the caller supplies the grader catalog instead of the runner
/// building it internally.
///
/// [`run_case`] is the thin wrapper that passes [`default_graders`], so both go
/// through this one body. The point is testability of the *ordering contract*:
/// the whole reason [`Grader::grade`] is async is that a grader may inspect the
/// case's temp workspace, which is only valid because the runner awaits grading
/// before dropping the workspace. A test can inject a workspace-probing grader
/// here and observe that ordering from inside a real run — proving the runner
/// honours it, which a test that calls `Grader::grade` directly cannot do.
///
/// [`Grader::grade`]: crate::grader::Grader::grade
pub async fn run_case_with_graders(
    trace: &LlmTrace,
    deps: &RunDeps,
    graders: Vec<Box<dyn Grader>>,
) -> anyhow::Result<CaseOutcome> {
    run_case_with_graders_recording_provenance(trace, deps, graders, &mut None).await
}

/// [`run_case`], but publish immutable provenance before fallible execution.
pub async fn run_case_recording_provenance(
    trace: &LlmTrace,
    deps: &RunDeps,
    provenance_out: &mut Option<CaseProvenance>,
) -> anyhow::Result<CaseOutcome> {
    run_case_with_graders_recording_provenance(trace, deps, default_graders(trace), provenance_out)
        .await
}

async fn run_case_with_graders_recording_provenance(
    trace: &LlmTrace,
    deps: &RunDeps,
    graders: Vec<Box<dyn Grader>>,
    provenance_out: &mut Option<CaseProvenance>,
) -> anyhow::Result<CaseOutcome> {
    match deps.mode {
        Mode::Replay => run_replay_case(trace, deps, graders, provenance_out).await,
        Mode::Live => {
            crate::live::run_live_case_with_graders_recording_provenance(
                trace,
                deps,
                graders,
                provenance_out,
            )
            .await
        }
    }
}

/// Replay a scripted trace through the Phase 0 deterministic agent (echo tools,
/// native dispatcher, no network).
async fn run_replay_case(
    trace: &LlmTrace,
    deps: &RunDeps,
    graders: Vec<Box<dyn Grader>>,
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

    // The engine's tool registry is sealed (`ScopedToolRegistry`), mintable only
    // through the one assembly seam. Route the eval harness's fixed tool set
    // through it with a permissive default policy so `assemble` is an identity
    // over `default_tools()` (nothing added, nothing dropped): the eval agent
    // sees exactly the same tools as before the seal. Every assembly divergence
    // is off (no peripherals / MCP / skills / memory-strip), so the config,
    // alias, and runtime are never read beyond satisfying the signature.
    let eval_config = zeroclaw_config::schema::Config::default();
    let eval_security = Arc::new(zeroclaw_config::policy::SecurityPolicy::default());
    let eval_registry = zeroclaw_runtime::tools::scoped::ScopedToolRegistry::assemble(
        zeroclaw_runtime::tools::scoped::ScopedAssembly {
            config: &eval_config,
            agent_alias: "eval",
            security: &eval_security,
            built: zeroclaw_runtime::tools::AllToolsResult::from_prebuilt_tools(default_tools()),
            skills: &[],
            runtime: Arc::new(zeroclaw_runtime::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: false,
            acp_delivery: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        },
    )
    .await
    .registry;

    let registered = eval_registry
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    let provenance = case_provenance(
        trace,
        deps,
        ToolSurface::new(Vec::new(), Vec::new(), registered),
    )?;
    *provenance_out = Some(provenance.clone());

    let observer = Arc::new(RecordingObserver::new());
    let CaseProvider {
        provider,
        finish_turn,
        ..
    } = (deps.provider)(trace)?;

    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(eval_registry)
        .memory(memory)
        .observer(observer.clone())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(tmp.path().to_path_buf())
        .build()?;

    let start = std::time::Instant::now();
    let mut final_response = String::new();
    for (turn_index, turn) in trace.turns.iter().enumerate() {
        final_response = agent.turn(&turn.user_input).await?;
        if let Some(finish) = &finish_turn {
            finish(turn_index)?;
        }
    }
    let duration_ms = duration_millis_saturating(start.elapsed());

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
    let grades = grade_with(&graders, &record, tmp.path()).await;
    Ok(CaseOutcome { record, grades })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::grader::{GradeCategory, GradeContext, GradeResult};
    use crate::record::RunRecord;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A grader that records, from *inside* a real run, whether the case
    /// workspace still existed when the runner called it.
    ///
    /// The assertion happens after the runner returns, so what it observes is
    /// the runner's grade-then-drop ordering rather than the test's own setup.
    /// A test that constructs a `GradeContext` by hand and calls `Grader::grade`
    /// directly cannot observe that — it would stay green if the production
    /// await moved after the workspace drop.
    pub(crate) struct WorkspaceProbe {
        pub(crate) seen_alive: Arc<AtomicBool>,
        pub(crate) calls: Arc<AtomicUsize>,
    }

    impl WorkspaceProbe {
        pub(crate) fn new() -> (Self, Arc<AtomicBool>, Arc<AtomicUsize>) {
            let seen_alive = Arc::new(AtomicBool::new(false));
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    seen_alive: Arc::clone(&seen_alive),
                    calls: Arc::clone(&calls),
                },
                seen_alive,
                calls,
            )
        }
    }

    #[async_trait::async_trait]
    impl Grader for WorkspaceProbe {
        fn name(&self) -> &str {
            "workspace_probe"
        }

        async fn grade(&self, _run: &RunRecord, ctx: &GradeContext<'_>) -> Vec<GradeResult> {
            let alive = ctx.workspace.exists();
            self.seen_alive.store(alive, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            vec![GradeResult::new(
                "workspace_live".to_string(),
                alive,
                if alive {
                    "workspace present at grade time"
                } else {
                    "workspace already torn down at grade time"
                },
                GradeCategory::SideEffect,
            )]
        }
    }

    #[tokio::test]
    async fn runner_grades_before_dropping_the_case_workspace() {
        // The contract the async `Grader` trait exists to provide: a grader can
        // inspect the case's temp workspace, because the runner awaits grading
        // before that workspace is dropped. Driven through `run_case_with_graders`
        // so the ordering under test is the runner's, not the test's.
        let trace: LlmTrace = serde_json::from_str(SMOKE).unwrap();
        let (probe, seen_alive, calls) = WorkspaceProbe::new();
        let outcome = run_case_with_graders(&trace, &RunDeps::replay(), vec![Box::new(probe)])
            .await
            .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the injected grader must actually run on the real runner path"
        );
        assert!(
            seen_alive.load(Ordering::SeqCst),
            "case workspace was torn down before the runner awaited grading"
        );
        assert!(
            outcome.grades.iter().all(|g| g.passed),
            "grades: {:?}",
            outcome.grades
        );
    }

    #[tokio::test]
    async fn run_case_uses_the_default_grader_catalog() {
        // `run_case` must remain a thin wrapper over the seam: if it drifted to a
        // different catalog, the seam-based regression above would be testing a
        // path production does not use.
        let trace: LlmTrace = serde_json::from_str(SMOKE).unwrap();
        let via_wrapper = run_case(&trace, &RunDeps::replay()).await.unwrap();
        let via_seam = run_case_with_graders(
            &trace,
            &RunDeps::replay(),
            crate::grader::default_graders(&trace),
        )
        .await
        .unwrap();

        let checks =
            |o: &CaseOutcome| -> Vec<String> { o.grades.iter().map(|g| g.check.clone()).collect() };
        assert_eq!(checks(&via_wrapper), checks(&via_seam));
        assert!(
            checks(&via_wrapper).contains(&r#"response_contains("Hello")"#.to_string()),
            "default catalog must include the fixture's expectation grades: {:?}",
            checks(&via_wrapper)
        );
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
    async fn replay_rejects_overspecified_turn_at_boundary() {
        // Turn 0 scripts two text steps, but the agent's turn loop stops after
        // the first text response (no tool call to keep the round-trip going),
        // so "Leftover never requested." is never popped. The per-turn boundary
        // (`finish_turn`) must catch this leftover step at turn-end, rather than
        // silently letting it bleed into turn 1's replay (the pre-fix behavior:
        // the flattened single-queue provider would hand "Leftover never
        // requested." to turn 1 instead of "Second.", with no error at all).
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "test-overspecified-turn",
                "turns": [
                    {
                        "user_input": "Hi",
                        "steps": [
                            { "response": { "type": "text", "content": "First." } },
                            { "response": { "type": "text", "content": "Leftover never requested." } }
                        ]
                    },
                    { "user_input": "Again", "steps": [{ "response": { "type": "text", "content": "Second." } }] }
                ],
                "expects": {}
            }"#,
        )
        .unwrap();
        let err = run_case(&trace, &RunDeps::replay()).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("turn 0"), "error must name turn 0: {msg}");
        assert!(
            msg.contains("over-specifies"),
            "error must call out the over-specification: {msg}"
        );
    }

    #[tokio::test]
    async fn replay_turn_cannot_borrow_next_turns_steps() {
        // Turn 0 scripts only a tool call, so after the agent executes the tool
        // it needs a follow-up LLM response within turn 0 that was never
        // scripted. Turn 1 has its own steps, kept in a separate queue, and
        // must not be reachable from turn 0's replay: the error must be the
        // turn-scoped exhaustion guard naming turn 0, not a borrowed "Borrowed."
        // response from turn 1.
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "test-turn-boundary",
                "turns": [
                    {
                        "user_input": "Echo hello for me",
                        "steps": [
                            { "response": { "type": "tool_calls", "tool_calls": [{ "id": "call_1", "name": "echo", "arguments": {"message": "hello"} }] } }
                        ]
                    },
                    { "user_input": "Again", "steps": [{ "response": { "type": "text", "content": "Borrowed." } }] }
                ],
                "expects": {}
            }"#,
        )
        .unwrap();
        let err = run_case(&trace, &RunDeps::replay()).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("turn 0"), "error must name turn 0: {msg}");
        assert!(
            !msg.contains("Borrowed"),
            "error must not have consumed turn 1's steps: {msg}"
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
        let json: serde_json::Value =
            serde_json::from_str(&report.to_json(crate::baseline::SuiteKind::Regression, None))
                .unwrap();
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
            r#"{ "model_name": "no-steps-case", "turns": [{ "user_input": "Hi" }], "expects": { "max_tool_calls": 0 } }"#,
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

        let json: serde_json::Value =
            serde_json::from_str(&report.to_json(crate::baseline::SuiteKind::Regression, None))
                .unwrap();
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
        let json: serde_json::Value =
            serde_json::from_str(&report.to_json(crate::baseline::SuiteKind::Regression, None))
                .unwrap();
        assert_eq!(json["cases"][0]["score"], 1.0);
        assert!(json["cases"][0]["total_tokens"].is_number());
    }
}
