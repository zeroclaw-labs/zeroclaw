//! `zeroclaw eval` — run the agent evaluation harness.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroclaw_config::schema::Config;
use zeroclaw_eval::baseline::{self, Baseline, CaseComparison, SuiteKind};
use zeroclaw_eval::{CaseReport, LlmTrace, Mode, RunDeps, SuiteReport};
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

/// Re-run each regressed case once against the same config, returning whether the
/// single re-run passed, keyed by case id. Used only for live suites.
async fn rerun_live_regressions(
    config: &Config,
    suite_path: &Path,
    comparison: &baseline::BaselineComparison,
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
    let deps = build_run_deps(config, Mode::Live)?;
    for (_, trace) in &traces {
        let id = trace.display_id();
        if regressed.contains(&id) {
            let passed = matches!(
                Box::pin(zeroclaw_eval::run_case(trace, &deps)).await,
                Ok(outcome) if outcome.grades.iter().all(|g| g.passed)
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
    let judge = build_judge_deps(config)?;
    let mut deps = match mode {
        // Replay's provider wiring is owned by `RunDeps::replay()`; delegate so the
        // trace-replay factory has a single definition. Replay ignores the live-only
        // tool allowlist and timeout.
        Mode::Replay => RunDeps::replay(),
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
            // Self-judge bias warning: judge and agent share the same provider ref.
            if judge
                .as_ref()
                .is_some_and(|j| j.judge_ref.split(':').next() == Some(provider_ref.as_str()))
            {
                println!(
                    "  warning: judge and live provider are the same provider reference (self-judging bias)"
                );
            }
            let cfg = config.clone();
            RunDeps {
                mode,
                provider: Box::new(move |_trace: &LlmTrace| {
                    let (provider, _provider_type, _resolved_model) =
                        build_session_model_provider(&cfg, &provider_ref, None)?;
                    Ok(provider)
                }),
                provider_ref: receipt_ref,
                live_tools: config.eval.live_allowed_tools.clone(),
                case_timeout: Duration::from_secs(config.eval.case_timeout_secs),
                judge: None,
            }
        }
    };
    deps.judge = judge;
    Ok(deps)
}

/// Sanitize a judge ref into a calibration filename stem: `/`, `.`, and `:` all
/// become `_` so the model-inclusive `judge_ref` is a safe single-segment name.
fn calibration_stem(judge_ref: &str) -> String {
    judge_ref
        .chars()
        .map(|c| {
            if c == '/' || c == '.' || c == ':' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Build judge deps from config, or `None` when `[eval].judge_provider` is empty.
/// Judge grades gate only when `judge_gate` is set AND a calibration file exists
/// for the judge (keyed by the model-inclusive `judge_ref`, matching the
/// comparability key); otherwise they stay diagnostic (a missing file warns).
fn build_judge_deps(config: &Config) -> Result<Option<zeroclaw_eval::grader::JudgeDeps>> {
    let judge_provider = config.eval.judge_provider.as_str().trim().to_string();
    if judge_provider.is_empty() {
        return Ok(None);
    }
    let (provider, _provider_type, model) =
        build_session_model_provider(config, &judge_provider, None)?;
    let judge_ref = format!("{judge_provider}:{model}");
    let calibration = std::path::Path::new(&format!(
        "evals/calibration/{}.json",
        calibration_stem(&judge_ref)
    ))
    .exists();
    let gates = config.eval.judge_gate && calibration;
    if config.eval.judge_gate && !calibration {
        println!(
            "  warning: [eval].judge_gate is set but no calibration file for {judge_ref}; judge grades stay diagnostic"
        );
    }
    Ok(Some(zeroclaw_eval::grader::JudgeDeps {
        provider: std::sync::Arc::from(provider),
        model,
        judge_ref,
        gates,
    }))
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
    use zeroclaw_eval::RunRecord;
    use zeroclaw_eval::record::SandboxStamp;

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
            judge_ref: None,
            judge_usage: None,
        }
    }

    fn case_report(name: &str, passed: bool) -> CaseReport {
        CaseReport {
            name: name.to_string(),
            source: "f.json".to_string(),
            record: Some(record(name)),
            grades: Vec::new(),
            error: if passed {
                None
            } else {
                Some("boom".to_string())
            },
        }
    }

    #[test]
    fn calibration_stem_keys_on_model_inclusive_judge_ref() {
        // The stem is derived from judge_ref (provider:model), not the bare
        // provider, so calibration is model-specific and matches the docs.
        assert_eq!(
            calibration_stem("anthropic.sonnet:claude-x"),
            "anthropic_sonnet_claude-x"
        );
        // A model swap under the same provider produces a different stem.
        assert_ne!(
            calibration_stem("anthropic.sonnet:model-a"),
            calibration_stem("anthropic.sonnet:model-b")
        );
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
