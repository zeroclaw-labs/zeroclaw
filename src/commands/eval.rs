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
///
/// Every accepted path converges on the single `emit_report` call at the end,
/// so no flag combination can drop the requested output format.
pub async fn finalize(
    config: &Config,
    mode: Mode,
    suite_path: &Path,
    report: SuiteReport,
    opts: FinalizeOpts,
) -> Result<i32> {
    let kind = SuiteKind::resolve(suite_path, opts.suite_kind);

    let wrote_auto = write_dumps(
        &report,
        opts.dump_records.as_deref(),
        Path::new(AUTO_DUMP_DIR),
    )?;

    // --write-baseline: persist the run, then fall through to the same renderer
    // with an explicit no-comparison policy. A baseline refresh has no prior
    // baseline to diff against, so nothing is <skipped/> — but the requested
    // format is still emitted exactly once. Returning early here is what made
    // `--format junit --write-baseline` produce no document at all.
    let mut flaky_ids: Vec<String> = Vec::new();
    let compared = if let Some(path) = &opts.write_baseline {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, Baseline::from_report(&report).to_json())?;
        None
    } else {
        // --baseline: compare and apply the live flakiness rule.
        match &opts.baseline {
            Some(path) => {
                let baseline = Baseline::from_json(&std::fs::read_to_string(path)?)?;
                let mut cmp = baseline::compare(&report, &baseline);
                if mode == Mode::Live {
                    let rerun_passed =
                        Box::pin(rerun_live_regressions(config, suite_path, &cmp)).await?;
                    flaky_ids =
                        baseline::downgrade_flaky_regressions(&mut cmp, mode, &rerun_passed);
                }
                Some((cmp, baseline))
            }
            None => None,
        }
    };
    let (comparison, baseline) = match compared {
        Some((cmp, baseline)) => (Some(cmp), Some(baseline)),
        None => (None, None),
    };

    emit_report(EmitReport {
        report: &report,
        comparison: comparison.as_ref(),
        baseline: baseline.as_ref(),
        format: opts.format,
        kind,
        wrote_auto,
        flaky_ids: &flaky_ids,
    });

    Ok(report.exit_code(kind, comparison.as_ref()))
}

/// Everything the single render site needs. Grouped in one struct so adding a
/// new output format cannot silently miss a call site.
struct EmitReport<'a> {
    report: &'a SuiteReport,
    comparison: Option<&'a baseline::BaselineComparison>,
    baseline: Option<&'a Baseline>,
    format: OutputFormat,
    kind: SuiteKind,
    wrote_auto: bool,
    /// Live cases downgraded from regression to flaky-unconfirmed, announced in
    /// the table output only.
    flaky_ids: &'a [String],
}

/// The one place a run's output is written. Every accepted flag combination
/// reaches this exactly once, so a requested format is never dropped.
fn emit_report(args: EmitReport<'_>) {
    let EmitReport {
        report,
        comparison,
        baseline,
        format,
        kind,
        wrote_auto,
        flaky_ids,
    } = args;

    match format {
        OutputFormat::Json => println!("{}", report.to_json()),
        OutputFormat::Table => {
            println!("{}", report.render_table());
            if wrote_auto {
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-eval-failed-case-records",
                        &[("dir", AUTO_DUMP_DIR)],
                    )
                );
            }
            for id in flaky_ids {
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-eval-flaky-unconfirmed-regression",
                        &[("id", id.as_str())],
                    )
                );
            }
            match (comparison, baseline) {
                (Some(cmp), Some(baseline)) => print_comparison(cmp, kind, report, baseline),
                _ => {
                    if kind == SuiteKind::Capability {
                        println!("  {}", report.capability_summary(None));
                    }
                }
            }
        }
        OutputFormat::Junit => {
            // Cases unverifiable against the baseline render as <skipped/>. With
            // no comparison (no --baseline, or --write-baseline) nothing is
            // skipped and the raw suite report is rendered as-is.
            let skipped: Vec<&str> = comparison
                .map(|cmp| {
                    cmp.per_case
                        .iter()
                        .filter(|(_, c)| matches!(c, CaseComparison::Unverifiable))
                        .map(|(id, _)| id.as_str())
                        .collect()
                })
                .unwrap_or_default();
            // Flaky-unconfirmed live cases are "reported, never gated" and exit
            // 0, so they must not render as <failure>. Read them off the
            // comparison rather than the local list, so the XML classification
            // and the exit code are driven by the same source of truth.
            let flaky: Vec<&str> = comparison
                .map(|cmp| {
                    cmp.per_case
                        .iter()
                        .filter(|(_, c)| matches!(c, CaseComparison::FlakyUnconfirmed))
                        .map(|(id, _)| id.as_str())
                        .collect()
                })
                .unwrap_or_default();
            print!(
                "{}",
                zeroclaw_eval::junit::render_junit(report, &skipped, &flaky)
            );
        }
    }
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
                    let (provider, _provider_type, _resolved_model) =
                        build_session_model_provider(&cfg, &provider_ref, None)?;
                    Ok(provider)
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
    /// JUnit XML, for CI test reporters.
    Junit,
}

/// Render a suite report in the requested format, without any comparison
/// context. Kept for callers that only have a `SuiteReport`; the `eval run`
/// flow goes through `emit_report` instead so no format can be dropped by an
/// early return.
pub fn print_report(report: &SuiteReport, format: OutputFormat) {
    emit_report(EmitReport {
        report,
        comparison: None,
        baseline: None,
        format,
        kind: SuiteKind::Regression,
        wrote_auto: false,
        flaky_ids: &[],
    });
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
