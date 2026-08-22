//! `zeroclaw eval` — run the agent evaluation harness.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroclaw_config::schema::Config;
use zeroclaw_eval::baseline::{self, Baseline, CaseComparison, SuiteKind};
use zeroclaw_eval::{CaseProvider, CaseReport, LlmTrace, Mode, RunDeps, SuiteReport};
use zeroclaw_runtime::agent::agent::build_session_model_provider;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

/// Where failed-case records are auto-dumped on every run.
pub const AUTO_DUMP_DIR: &str = "target/eval-last-run";

/// Post-run options gathered from the `eval run` flags.
pub struct FinalizeOpts {
    pub format: OutputFormat,
    pub dump_records: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub write_baseline: Option<PathBuf>,
    pub suite_kind: Option<SuiteKind>,
}

/// Handle the post-run flow (dumps, baselines, comparison, printing) and return
/// the process exit code. Kept together so `main` only wires flags.
pub async fn finalize(
    config: &Config,
    mode: Mode,
    suite_path: &Path,
    report: SuiteReport,
    opts: FinalizeOpts,
) -> Result<i32> {
    let kind = SuiteKind::resolve(suite_path, opts.suite_kind);
    print_report(&report, opts.format);

    let wrote_auto = write_dumps(
        &report,
        opts.dump_records.as_deref(),
        Path::new(AUTO_DUMP_DIR),
    )?;
    if wrote_auto && opts.format == OutputFormat::Table {
        println!(
            "{}",
            get_required_cli_string_with_args(
                "cli-eval-failed-case-records",
                &[("dir", AUTO_DUMP_DIR)],
            )
        );
    }

    // --write-baseline: persist the run and exit with its normal code.
    if let Some(path) = &opts.write_baseline {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, Baseline::from_report(&report).to_json())?;
        return Ok(report.exit_code(kind, None));
    }

    // --baseline: compare, apply the live flakiness rule, and report.
    let comparison = match &opts.baseline {
        Some(path) => {
            let baseline = Baseline::from_json(&std::fs::read_to_string(path)?)?;
            let mut cmp = baseline::compare(&report, &baseline);
            if mode == Mode::Live {
                let rerun_passed =
                    Box::pin(rerun_live_regressions(config, suite_path, &cmp)).await?;
                let flaky = baseline::downgrade_flaky_regressions(&mut cmp, mode, &rerun_passed);
                if opts.format == OutputFormat::Table {
                    for id in &flaky {
                        println!(
                            "{}",
                            get_required_cli_string_with_args(
                                "cli-eval-flaky-unconfirmed-regression",
                                &[("id", id)],
                            )
                        );
                    }
                }
            }
            if opts.format == OutputFormat::Table {
                print_comparison(&cmp, kind, &report, &baseline);
            }
            Some(cmp)
        }
        None => {
            if kind == SuiteKind::Capability && opts.format == OutputFormat::Table {
                println!("  {}", report.capability_summary(None));
            }
            None
        }
    };

    Ok(report.exit_code(kind, comparison.as_ref()))
}

/// Re-run each regressed case against the same config, returning whether the
/// re-run confirmed a pass, keyed by case id. Used only for live suites.
///
/// The re-run honors the case's own effective repeat policy via
/// [`zeroclaw_eval::run_case_repeated`], so a `repeat = k` case must clear
/// `pass^k` again to be excused as flaky. Re-running once would let a single
/// lucky attempt downgrade a real regression on a case whose documented
/// contract is "passes iff all k runs pass".
async fn rerun_live_regressions(
    config: &Config,
    suite_path: &Path,
    comparison: &baseline::BaselineComparison,
) -> Result<BTreeMap<String, bool>> {
    let deps = build_run_deps(config, Mode::Live)?;
    rerun_live_regressions_with_deps(suite_path, comparison, &deps).await
}

/// Dependency-injected core of [`rerun_live_regressions`]. Keeping the retry
/// decision here lets the regression exercise the same suite loading, repeat
/// runner, and baseline verdict used by production without calling a provider.
async fn rerun_live_regressions_with_deps(
    suite_path: &Path,
    comparison: &baseline::BaselineComparison,
    deps: &RunDeps,
) -> Result<BTreeMap<String, bool>> {
    let regressed: Vec<&str> = comparison
        .per_case
        .iter()
        .filter(|(_, c)| matches!(c, CaseComparison::Regression { .. }))
        .map(|(id, _)| id.as_str())
        .collect();
    let mut out = BTreeMap::new();
    if regressed.is_empty() {
        return Ok(out);
    }
    let traces = zeroclaw_eval::case::load_suite(suite_path)?;
    for (_, trace) in &traces {
        let id = trace.display_id();
        if regressed.contains(&id) {
            let passed = matches!(
                Box::pin(zeroclaw_eval::run_case_repeated(trace, deps)).await,
                Ok((outcome, repeat))
                    if outcome.grades.iter().all(|g| g.passed)
                        && repeat.as_ref().is_none_or(|stats| stats.establishes_pass_hat_k())
            );
            out.insert(id.to_string(), passed);
        }
    }
    Ok(out)
}

/// Print a compact per-case comparison summary.
fn print_comparison(
    comparison: &baseline::BaselineComparison,
    kind: SuiteKind,
    report: &SuiteReport,
    baseline: &Baseline,
) {
    println!();
    println!(
        "{}",
        get_required_cli_string("cli-eval-baseline-comparison")
    );
    for (id, c) in &comparison.per_case {
        let line = match c {
            CaseComparison::New => get_required_cli_string("cli-eval-comparison-new"),
            CaseComparison::Removed => get_required_cli_string("cli-eval-comparison-removed"),
            CaseComparison::Unverifiable => {
                get_required_cli_string("cli-eval-comparison-unverifiable")
            }
            CaseComparison::Improvement => {
                get_required_cli_string("cli-eval-comparison-improvement")
            }
            CaseComparison::FlakyUnconfirmed => {
                get_required_cli_string("cli-eval-comparison-flaky-unconfirmed")
            }
            CaseComparison::Regression { categories } => {
                let cats: Vec<&str> = categories.iter().map(|c| c.as_str()).collect();
                let categories = cats.join(", ");
                get_required_cli_string_with_args(
                    "cli-eval-comparison-regression",
                    &[("categories", categories.as_str())],
                )
            }
            CaseComparison::Unchanged { token_delta_pct } => match token_delta_pct {
                Some(pct) => {
                    let pct = format!("{pct:+.0}");
                    get_required_cli_string_with_args(
                        "cli-eval-comparison-unchanged-tokens",
                        &[("pct", pct.as_str())],
                    )
                }
                None => get_required_cli_string("cli-eval-comparison-unchanged"),
            },
        };
        println!("    {id}: {line}");
    }
    if kind == SuiteKind::Capability {
        println!("  {}", report.capability_summary(Some(baseline)));
    }
}

/// Build the per-run dependencies for the requested mode, threading the loaded
/// config so live mode can resolve its provider. Replay injects the deterministic
/// trace-replay provider; live resolves `[eval].live_provider` per case.
fn build_run_deps(config: &Config, mode: Mode) -> Result<RunDeps> {
    match mode {
        // Replay's provider wiring is owned by `RunDeps::replay()`; delegate so the
        // trace-replay factory has a single definition. Replay ignores the live-only
        // tool allowlist and timeout.
        Mode::Replay => Ok(RunDeps::replay()),
        Mode::Live => {
            // Trim so validation (which trims) and runtime resolution agree: a
            // whitespace-padded ref must not pass `Config::validate` then miss here.
            let provider_ref = config.eval.live_provider.as_str().trim().to_string();
            zeroclaw_eval::ensure_live_provider(&provider_ref)?;
            // Resolve the model once for the receipt label; the closure builds a
            // fresh provider per case (isolation) and must be `'static`, so it owns
            // a config clone.
            let (_, _provider_type, resolved_model) =
                build_session_model_provider(config, &provider_ref, None)?;
            let receipt_ref = format!("{provider_ref}:{resolved_model}");
            let cfg = config.clone();
            Ok(RunDeps {
                mode,
                provider: Box::new(move |_trace: &LlmTrace| {
                    let (provider, provider_type, resolved_model) =
                        build_session_model_provider(&cfg, &provider_ref, None)?;
                    Ok(CaseProvider {
                        provider,
                        provider_name: Some(provider_type),
                        model_name: Some(resolved_model),
                        finish_turn: None,
                    })
                }),
                provider_ref: receipt_ref,
                live_tools: config.eval.live_allowed_tools.clone(),
                case_timeout: Duration::from_secs(config.eval.case_timeout_secs),
            })
        }
    }
}

/// Run a suite of eval cases and return the aggregated report. The failed-case
/// auto-dump directory is cleared at run start.
pub async fn run(config: &Config, suite: PathBuf, mode: Mode) -> Result<SuiteReport> {
    let _ = std::fs::remove_dir_all(AUTO_DUMP_DIR);
    let deps = build_run_deps(config, mode)?;
    Box::pin(zeroclaw_eval::run_suite(&suite, &deps)).await
}

/// Choose a collision-free path `dir/<stem>.json`, appending `_N` when a file
/// already exists there so distinct cases with the same (sanitized) id or a
/// shared `model_name` never silently overwrite each other's dump.
fn unique_dump_path(dir: &Path, case_id: &str) -> std::path::PathBuf {
    let stem: String = case_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut path = dir.join(format!("{stem}.json"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem}_{n}.json"));
        n += 1;
    }
    path
}

/// Write one case's dump into `dir`. Includes the record when present (`null` for
/// an errored case) plus grades and the error string, so an errored case still
/// yields an inspectable artifact.
fn write_case_dump(dir: &Path, case: &CaseReport) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let dump = serde_json::json!({
        "case_id": case.name,
        "record": case.record,
        "grades": case.grades,
        "error": case.error,
        "repeat": case.repeat,
    });
    std::fs::write(
        unique_dump_path(dir, &case.name),
        serde_json::to_string_pretty(&dump)?,
    )?;
    Ok(())
}

/// Write case dumps: `explicit_dir` (from `--dump-records`) receives every case;
/// `auto_dir` receives only failed/errored cases. Returns `true` if any auto-dump
/// was written, so the caller can print the footer.
pub fn write_dumps(
    report: &SuiteReport,
    explicit_dir: Option<&Path>,
    auto_dir: &Path,
) -> Result<bool> {
    if let Some(dir) = explicit_dir {
        for case in &report.cases {
            write_case_dump(dir, case)?;
        }
    }
    let mut any_auto = false;
    for case in &report.cases {
        if !case.passed() {
            write_case_dump(auto_dir, case)?;
            any_auto = true;
        }
    }
    Ok(any_auto)
}

/// Output format for the eval report.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table.
    Table,
    /// Machine-readable JSON, for CI artifacts.
    Json,
}

/// Render a suite report in the requested format.
pub fn print_report(report: &SuiteReport, format: OutputFormat) {
    match format {
        OutputFormat::Json => println!("{}", report.to_json()),
        OutputFormat::Table => println!("{}", report.render_table()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{
        ChatRequest, ChatResponse, ModelProvider, ProviderCapabilities,
    };
    use zeroclaw_eval::RunRecord;
    use zeroclaw_eval::record::SandboxStamp;

    struct StaticTextProvider;

    impl Attributable for StaticTextProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "eval-retry-test"
        }
    }

    #[async_trait]
    impl ModelProvider for StaticTextProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        async fn chat_with_system(
            &self,
            _system: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    fn record(case_id: &str) -> RunRecord {
        RunRecord {
            schema: zeroclaw_eval::record::RECORD_SCHEMA.to_string(),
            mode: Mode::Replay,
            case_id: case_id.to_string(),
            case_hash: "deadbeef".to_string(),
            provider_ref: "scripted".to_string(),
            tool_surface: Vec::new(),
            sandbox: SandboxStamp {
                autonomy: "supervised".to_string(),
                workspace_only: false,
            },
            final_response: "x".to_string(),
            history: Vec::new(),
            tools_called: Vec::new(),
            all_tools_succeeded: true,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: 0,
            llm_calls: 0,
        }
    }

    fn case_report(name: &str, passed: bool) -> CaseReport {
        CaseReport {
            name: name.to_string(),
            source: "f.json".to_string(),
            record: Some(record(name)),
            // Every case carries at least one grade: `CaseReport::passed()`
            // fails closed on an empty grade list, so a grade-less "passing"
            // fixture would not model a real passing case.
            grades: vec![zeroclaw_eval::grader::GradeResult {
                check: "response_contains(\"x\")".to_string(),
                passed: true,
                detail: "found".to_string(),
                category: zeroclaw_eval::grader::GradeCategory::Response,
            }],
            error: if passed {
                None
            } else {
                Some("boom".to_string())
            },
            repeat: None,
            cluster: None,
        }
    }

    #[test]
    fn dump_records_writes_all_cases() {
        let report = SuiteReport {
            cases: vec![case_report("pass", true), case_report("fail", false)],
        };
        let explicit = tempfile::tempdir().unwrap();
        let auto = tempfile::tempdir().unwrap();
        write_dumps(&report, Some(explicit.path()), auto.path()).unwrap();
        assert!(explicit.path().join("pass.json").exists());
        assert!(explicit.path().join("fail.json").exists());
    }

    #[test]
    fn failed_case_autodumps_record() {
        let report = SuiteReport {
            cases: vec![case_report("fail", false)],
        };
        let auto = tempfile::tempdir().unwrap();
        let any = write_dumps(&report, None, auto.path()).unwrap();
        assert!(any, "a failed case must report an auto-dump");
        assert!(auto.path().join("fail.json").exists());
    }

    #[test]
    fn passing_case_does_not_autodump() {
        let report = SuiteReport {
            cases: vec![case_report("pass", true)],
        };
        let auto = tempfile::tempdir().unwrap();
        let any = write_dumps(&report, None, auto.path()).unwrap();
        assert!(!any, "a passing case must not auto-dump");
        assert!(!auto.path().join("pass.json").exists());
    }

    #[test]
    fn colliding_case_ids_do_not_overwrite() {
        // "a/b" and "a_b" both sanitize to "a_b"; both must still be written.
        let report = SuiteReport {
            cases: vec![case_report("a/b", false), case_report("a_b", false)],
        };
        let explicit = tempfile::tempdir().unwrap();
        let auto = tempfile::tempdir().unwrap();
        write_dumps(&report, Some(explicit.path()), auto.path()).unwrap();
        let count = std::fs::read_dir(explicit.path()).unwrap().count();
        assert_eq!(count, 2, "colliding ids must produce two files, not one");
    }

    /// Drive the production baseline retry composition: two passing attempts
    /// followed by a provider-construction error must remain a regression, even
    /// though the representative completed run passed every grade.
    #[tokio::test]
    async fn truncated_baseline_retry_remains_gating() {
        use zeroclaw_eval::baseline::{BaselineComparison, CaseComparison, SuiteKind};

        let suite = tempfile::tempdir().unwrap();
        std::fs::write(
            suite.path().join("partial.json"),
            r#"{
                "id": "partial-retry",
                "model_name": "partial-retry",
                "repeat": 3,
                "turns": [{ "user_input": "run" }],
                "expects": { "response_contains": ["ok"] }
            }"#,
        )
        .unwrap();

        let builds = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = Arc::clone(&builds);
        let deps = RunDeps {
            mode: Mode::Live,
            provider: Box::new(move |_trace| {
                let attempt = builds_for_factory.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt == 3 {
                    anyhow::bail!("provider failed on attempt 3");
                }
                Ok(zeroclaw_eval::CaseProvider::from_provider(Box::new(
                    StaticTextProvider,
                )))
            }),
            provider_ref: "test.retry:model".to_string(),
            live_tools: Vec::new(),
            case_timeout: Duration::from_secs(5),
        };

        let mut cmp = BaselineComparison {
            per_case: [(
                "partial-retry".to_string(),
                CaseComparison::Regression {
                    categories: Vec::new(),
                },
            )]
            .into_iter()
            .collect(),
        };
        let rerun = rerun_live_regressions_with_deps(suite.path(), &cmp, &deps)
            .await
            .unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 3);
        assert_eq!(rerun.get("partial-retry"), Some(&false));

        let flaky =
            zeroclaw_eval::baseline::downgrade_flaky_regressions(&mut cmp, Mode::Live, &rerun);
        assert!(
            flaky.is_empty(),
            "a truncated retry must not be excused as flaky"
        );

        let report = SuiteReport {
            cases: vec![case_report("partial-retry", false)],
        };
        assert_eq!(
            report.exit_code(SuiteKind::Regression, Some(&cmp)),
            1,
            "the incomplete retry leaves the original regression gating"
        );
    }

    /// The bot's exact gate-bypass scenario at the command boundary: a baseline
    /// regression for `"duplicate"`, plus two current fixtures sharing that id —
    /// one failing, one passing, the passing one visited last (path order
    /// `a_failing.json` < `b_passing.json`). Before identity validation the
    /// retry's `rerun_passed["duplicate"]` was overwritten last-writer-wins by
    /// the passing sibling and the real regression was downgraded to flaky.
    /// Now the suite refuses to load at all, so the regression stays gating.
    #[tokio::test]
    async fn duplicate_case_ids_cannot_downgrade_a_regression() {
        use zeroclaw_eval::baseline::{BaselineComparison, CaseComparison, SuiteKind};

        let suite = tempfile::tempdir().unwrap();
        // Fails: demands a substring the static provider never emits.
        std::fs::write(
            suite.path().join("a_failing.json"),
            r#"{
                "id": "duplicate",
                "model_name": "duplicate",
                "turns": [{ "user_input": "run" }],
                "expects": { "response_contains": ["never-emitted"] }
            }"#,
        )
        .unwrap();
        // Passes, and sorts last so it would be the final writer.
        std::fs::write(
            suite.path().join("b_passing.json"),
            r#"{
                "id": "duplicate",
                "model_name": "duplicate",
                "turns": [{ "user_input": "run" }],
                "expects": { "response_contains": ["ok"] }
            }"#,
        )
        .unwrap();

        let deps = RunDeps {
            mode: Mode::Live,
            provider: Box::new(|_trace| {
                Ok(zeroclaw_eval::CaseProvider::from_provider(Box::new(
                    StaticTextProvider,
                )))
            }),
            provider_ref: "test.retry:model".to_string(),
            live_tools: Vec::new(),
            case_timeout: Duration::from_secs(5),
        };

        let mut cmp = BaselineComparison {
            per_case: [(
                "duplicate".to_string(),
                CaseComparison::Regression {
                    categories: Vec::new(),
                },
            )]
            .into_iter()
            .collect(),
        };

        let err = rerun_live_regressions_with_deps(suite.path(), &cmp, &deps)
            .await
            .expect_err("a suite with colliding case ids must not run the retry");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("duplicate")
                && rendered.contains("a_failing.json")
                && rendered.contains("b_passing.json"),
            "error must name the id and both fixtures, got: {rendered}"
        );

        // No retry result exists, so nothing can be downgraded and the
        // regression still forces a non-zero exit.
        let flaky = zeroclaw_eval::baseline::downgrade_flaky_regressions(
            &mut cmp,
            Mode::Live,
            &BTreeMap::new(),
        );
        assert!(flaky.is_empty(), "no case may be excused as flaky");
        let report = SuiteReport {
            cases: vec![case_report("duplicate", false)],
        };
        assert_eq!(
            report.exit_code(SuiteKind::Regression, Some(&cmp)),
            1,
            "the regression must stay gating"
        );
    }

    #[test]
    fn record_dump_contains_indexed_repeat_attempts() {
        let sample = zeroclaw_eval::stats::RunSample {
            passed: true,
            input_tokens: 3,
            output_tokens: 2,
            duration_ms: 17,
            llm_calls: 1,
            checks: vec![("response_contains".to_string(), true)],
        };
        let mut case = case_report("partial", false);
        case.repeat = Some(zeroclaw_eval::stats::RepeatStats::from_partial_runs(
            3,
            &[sample],
            "provider timeout".to_string(),
        ));

        let dir = tempfile::tempdir().unwrap();
        write_case_dump(dir.path(), &case).unwrap();
        let dump: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("partial.json")).unwrap(),
        )
        .unwrap();
        let attempts = dump["repeat"]["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["attempt"].as_u64(), Some(1));
        assert_eq!(attempts[0]["outcome"].as_str(), Some("passed"));
        assert_eq!(attempts[1]["attempt"].as_u64(), Some(2));
        assert_eq!(attempts[1]["outcome"].as_str(), Some("error"));
        assert_eq!(attempts[1]["error"].as_str(), Some("provider timeout"));
        assert!(
            attempts[0].get("history").is_none(),
            "minimal attempt receipts must not duplicate transcripts"
        );
    }

    #[test]
    fn errored_case_is_dumped_with_error() {
        let mut errored = case_report("err", false);
        errored.record = None; // an errored case has no record, only an error string
        let report = SuiteReport {
            cases: vec![errored],
        };
        let auto = tempfile::tempdir().unwrap();
        let any = write_dumps(&report, None, auto.path()).unwrap();
        assert!(any, "an errored case must auto-dump");
        let content = std::fs::read_to_string(auto.path().join("err.json")).unwrap();
        assert!(
            content.contains("boom"),
            "the error string must be captured"
        );
    }
}
