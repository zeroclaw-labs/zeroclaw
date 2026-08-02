//! Append-only, transcript-free eval run-history receipts.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Mode;
use crate::baseline::{BaselineComparison, CaseComparison, SuiteKind, Verdict};
use crate::report::{CaseReport, RepeatCi, SuiteReport};
use crate::stats::RepeatStats;

/// The schema tag stamped on every run-history receipt.
pub const HISTORY_SCHEMA: &str = "zeroclaw-eval/history/v1";

/// One `eval run` invocation, recorded for longitudinal trend analysis.
/// Deliberately transcript-free: receipts may be retained, committed to
/// private repos, or uploaded as CI artifacts. Transcripts stay in
/// `--dump-records` dumps, which are debugging-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryReceipt {
    pub schema: String,
    pub recorded_at: DateTime<Utc>,
    pub git_sha: Option<String>,
    pub git_dirty: Option<bool>,
    pub zeroclaw_version: String,
    pub suite: String,
    pub suite_dir: String,
    pub suite_kind: SuiteKind,
    pub mode: Mode,
    pub provider_ref: String,
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    /// Exact numeric suite-level repeated-run summary from the report.
    pub repeat_ci: Option<HistoryRepeatCi>,
    pub cases: Vec<HistoryCase>,
}

/// One case's transcript-free longitudinal result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryCase {
    pub case_id: String,
    pub case_hash: String,
    pub provider_ref: String,
    pub tool_surface: Vec<String>,
    pub judge_ref: Option<String>,
    pub verdict: Verdict,
    pub error: bool,
    pub score: f64,
    /// Per-check results keyed by privacy-safe approved check-kind/ordinal identifiers.
    /// Raw grader labels are deliberately excluded because expectation labels
    /// can contain response needles, regexes, or workspace-relative paths.
    pub checks: BTreeMap<String, bool>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    pub llm_calls: u32,
    pub repeat: Option<RepeatStats>,
    pub baseline_comparison: Option<String>,
    pub regression_categories: Vec<String>,
}

/// Suite-level repeated-run confidence interval, copied from the report's
/// canonical numeric `repeat_ci` value without recomputing it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HistoryRepeatCi {
    pub pass_rate: f64,
    pub ci_half_width: Option<f64>,
    pub independent_units: usize,
}

impl From<RepeatCi> for HistoryRepeatCi {
    fn from(value: RepeatCi) -> Self {
        Self {
            pass_rate: value.pass_rate,
            ci_half_width: value.ci_half_width,
            independent_units: value.independent_units,
        }
    }
}

/// Run-level inputs that are not already owned by [`SuiteReport`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRun {
    pub recorded_at: DateTime<Utc>,
    pub suite_dir: String,
    pub suite_kind: SuiteKind,
    pub mode: Mode,
    pub provider_ref: String,
}

impl HistoryReceipt {
    /// Build a transcript-free receipt from a completed report. Git identity is
    /// sampled at this boundary because it describes the invocation, not a case.
    pub fn from_report(
        report: &SuiteReport,
        run: HistoryRun,
        comparison: Option<&BaselineComparison>,
    ) -> Result<Self> {
        let suite_path = Path::new(&run.suite_dir);
        let suite = suite_path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .context("eval suite path has no UTF-8 final component")?
            .to_string();
        let (git_sha, git_dirty) = git_stamp();
        let cases = report
            .cases
            .iter()
            .map(|case| history_case(case, &run.provider_ref, comparison))
            .collect();

        Ok(Self {
            schema: HISTORY_SCHEMA.to_string(),
            recorded_at: run.recorded_at,
            git_sha,
            git_dirty,
            zeroclaw_version: env!("CARGO_PKG_VERSION").to_string(),
            suite,
            suite_dir: run.suite_dir,
            suite_kind: run.suite_kind,
            mode: run.mode,
            provider_ref: run.provider_ref,
            passed: report.passed_count(),
            failed: report.failed_count(),
            total: report.cases.len(),
            repeat_ci: report.repeat_ci().map(HistoryRepeatCi::from),
            cases,
        })
    }
}

fn history_case(
    case: &CaseReport,
    run_provider_ref: &str,
    comparison: Option<&BaselineComparison>,
) -> HistoryCase {
    let classification = case.record.as_ref().and_then(|record| {
        comparison.and_then(|value| value.per_case.get(record.case_id.as_str()))
    });
    let (baseline_comparison, regression_categories) = comparison_fields(classification);

    let (checks, repeat) = history_check_fields(case);

    match &case.record {
        Some(record) => HistoryCase {
            case_id: record.case_id.clone(),
            case_hash: record.case_hash.clone(),
            provider_ref: record.provider_ref.clone(),
            tool_surface: record.tool_surface.clone(),
            judge_ref: record.judge_ref.clone(),
            verdict: if case.passed() {
                Verdict::Pass
            } else {
                Verdict::Fail
            },
            error: false,
            score: case.score(),
            checks,
            input_tokens: record.input_tokens,
            output_tokens: record.output_tokens,
            duration_ms: record.duration_ms,
            llm_calls: record.llm_calls,
            repeat,
            baseline_comparison,
            regression_categories,
        },
        None => HistoryCase {
            case_id: case.name.clone(),
            case_hash: String::new(),
            provider_ref: run_provider_ref.to_string(),
            tool_surface: Vec::new(),
            judge_ref: None,
            verdict: Verdict::Fail,
            error: true,
            score: 0.0,
            checks: BTreeMap::new(),
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: 0,
            llm_calls: 0,
            repeat: None,
            baseline_comparison,
            regression_categories,
        },
    }
}

/// Convert grader labels to category/ordinal identifiers before retention.
///
/// `GradeResult::check` is intended for ephemeral diagnostic output and may
/// embed fixture arguments such as response needles or expected file content.
/// The approved kind and deterministic position carry the longitudinal identity
/// we need within an unchanged case hash without retaining those arguments.
fn history_check_fields(case: &CaseReport) -> (BTreeMap<String, bool>, Option<RepeatStats>) {
    let mut kind_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut identities: BTreeMap<String, String> = BTreeMap::new();
    let mut checks = BTreeMap::new();

    for grade in &case.grades {
        let kind = safe_check_kind(&grade.check, grade.category.as_str());
        let ordinal = kind_counts.entry(kind).or_default();
        *ordinal += 1;
        let identity = format!("{kind}:{ordinal}");
        identities
            .entry(grade.check.clone())
            .or_insert_with(|| identity.clone());
        checks.insert(identity, grade.passed);
    }

    let repeat = case.repeat.as_ref().map(|value| {
        let mut safe = value.clone();
        let mut fallback_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        safe.check_flips = value
            .check_flips
            .iter()
            .map(|(raw, flips)| {
                let identity = identities.get(raw).cloned().unwrap_or_else(|| {
                    let kind = safe_check_kind(raw, "check");
                    let ordinal = fallback_counts.entry(kind).or_default();
                    *ordinal += 1;
                    format!("{kind}:{ordinal}")
                });
                (identity, *flips)
            })
            .collect();
        safe
    });

    (checks, repeat)
}

fn safe_check_kind(raw: &str, fallback: &'static str) -> &'static str {
    // Canonical allowlist for check kinds safe to retain in history. Unknown
    // grader labels fall back closed to their non-sensitive category (or
    // `check` for repeat-only data) instead of persisting an arbitrary prefix.
    const KINDS: &[&str] = &[
        "all_tools_succeeded",
        "file_absent",
        "file_contains",
        "file_exists",
        "judge",
        "max_duration_ms",
        "max_input_tokens",
        "max_llm_calls",
        "max_output_tokens",
        "max_tool_calls",
        "max_total_tokens",
        "response_contains",
        "response_json",
        "response_matches",
        "response_not_contains",
        "tools_not_used",
        "tools_used",
    ];
    KINDS
        .iter()
        .copied()
        .find(|kind| {
            raw.strip_prefix(kind).is_some_and(|suffix| {
                suffix.is_empty() || suffix.starts_with('(') || suffix.starts_with(':')
            })
        })
        .unwrap_or(fallback)
}

fn comparison_fields(classification: Option<&CaseComparison>) -> (Option<String>, Vec<String>) {
    match classification {
        Some(CaseComparison::New) => (Some("new".to_string()), Vec::new()),
        Some(CaseComparison::Unverifiable) => (Some("unverifiable".to_string()), Vec::new()),
        Some(CaseComparison::Regression { categories }) => (
            Some("regression".to_string()),
            categories
                .iter()
                .map(|category| category.as_str().to_string())
                .collect(),
        ),
        Some(CaseComparison::FlakyUnconfirmed) => {
            (Some("flaky_unconfirmed".to_string()), Vec::new())
        }
        Some(CaseComparison::Improvement) => (Some("improvement".to_string()), Vec::new()),
        Some(CaseComparison::Unchanged { .. }) => (Some("unchanged".to_string()), Vec::new()),
        // Removed cases do not have a current case receipt.
        Some(CaseComparison::Removed) | None => (None, Vec::new()),
    }
}

/// Write one pretty-printed receipt under `<dir>/<suite>/`, preserving earlier
/// receipts by suffixing collisions with `_N`.
pub fn write_history_receipt(dir: &Path, receipt: &HistoryReceipt) -> Result<PathBuf> {
    write_history_receipt_with(dir, receipt, |file, json| file.write_all(json))
}

fn write_history_receipt_with(
    dir: &Path,
    receipt: &HistoryReceipt,
    write_json: impl FnOnce(&mut File, &[u8]) -> std::io::Result<()>,
) -> Result<PathBuf> {
    let suite_component = Path::new(&receipt.suite);
    let mut components = suite_component.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        anyhow::bail!("history receipt suite must be one normal path component");
    }

    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let canonical_root = std::fs::canonicalize(dir)
        .with_context(|| format!("failed to resolve {}", dir.display()))?;
    let suite_dir = dir.join(suite_component);
    match std::fs::create_dir(&suite_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&suite_dir)
                .with_context(|| format!("failed to inspect {}", suite_dir.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                anyhow::bail!(
                    "history suite path is not a real directory: {}",
                    suite_dir.display()
                );
            }
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to create {}", suite_dir.display()));
        }
    }
    let suite_dir = std::fs::canonicalize(&suite_dir)
        .with_context(|| format!("failed to resolve {}", suite_dir.display()))?;
    if !suite_dir.starts_with(&canonical_root) {
        anyhow::bail!(
            "history suite path escapes configured directory: {}",
            suite_dir.display()
        );
    }

    let git_component = receipt
        .git_sha
        .as_deref()
        .filter(|sha| !sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or("nogit");
    let stem = format!(
        "{}-{git_component}",
        receipt.recorded_at.format("%Y%m%dT%H%M%SZ")
    );
    let mut json = serde_json::to_vec_pretty(receipt)?;
    json.push(b'\n');

    // Write and sync a hidden sibling first. Where the filesystem permits it,
    // a hard link publishes the complete file atomically without overwriting.
    // Other filesystems use the create-new compatibility path below.
    let mut temp_suffix = 0_u32;
    let (temp_path, mut temp_file) = loop {
        let temp_name = if temp_suffix == 0 {
            format!(".{stem}.tmp")
        } else {
            format!(".{stem}.{temp_suffix}.tmp")
        };
        let temp_path = suite_dir.join(temp_name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => break (temp_path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                temp_suffix = temp_suffix
                    .checked_add(1)
                    .context("history receipt temp-file suffix overflowed")?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create {}", temp_path.display()));
            }
        }
    };
    if let Err(error) = write_json(&mut temp_file, &json).and_then(|()| temp_file.sync_all()) {
        drop(temp_file);
        let _ = std::fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("failed to write {}", temp_path.display()));
    }
    if !path_has_parent(&temp_path, &suite_dir)? {
        drop(temp_file);
        anyhow::bail!("history suite directory changed while writing receipt");
    }
    drop(temp_file);

    let mut path = suite_dir.join(format!("{stem}.json"));
    let mut suffix = 1_u32;
    loop {
        match std::fs::hard_link(&temp_path, &path) {
            Ok(()) => {
                let temp_contained = path_has_parent(&temp_path, &suite_dir).unwrap_or(false);
                let final_contained = path_has_parent(&path, &suite_dir).unwrap_or(false);
                if !temp_contained || !final_contained {
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(&temp_path);
                    anyhow::bail!("history suite directory changed while publishing receipt");
                }
                let _ = std::fs::remove_file(&temp_path);
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                path = suite_dir.join(format!("{stem}_{suffix}.json"));
                let Some(next_suffix) = suffix.checked_add(1) else {
                    let _ = std::fs::remove_file(&temp_path);
                    anyhow::bail!("history receipt collision suffix overflowed");
                };
                suffix = next_suffix;
            }
            Err(link_error) => {
                let result = write_history_receipt_without_hard_links(&suite_dir, &stem, &json)
                    .with_context(|| {
                        format!(
                            "hard-link publication failed ({link_error}); create-new fallback also failed"
                        )
                    });
                let _ = std::fs::remove_file(&temp_path);
                return result;
            }
        }
    }
}

fn path_has_parent(path: &Path, expected_parent: &Path) -> Result<bool> {
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("failed to revalidate {}", path.display()))?;
    Ok(canonical.parent() == Some(expected_parent))
}

/// Compatibility path for writable filesystems without hard-link support.
/// `create_new` still prevents overwrite and partial files are removed on
/// returned write errors, though such filesystems cannot provide atomic publish
/// with the Rust standard library alone.
fn write_history_receipt_without_hard_links(
    suite_dir: &Path,
    stem: &str,
    json: &[u8],
) -> Result<PathBuf> {
    let mut path = suite_dir.join(format!("{stem}.json"));
    let mut suffix = 1_u32;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(json).and_then(|()| file.sync_all()) {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(error)
                        .with_context(|| format!("failed to write {}", path.display()));
                }
                drop(file);
                if !path_has_parent(&path, suite_dir)? {
                    let _ = std::fs::remove_file(&path);
                    anyhow::bail!("history suite directory changed while publishing receipt");
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                path = suite_dir.join(format!("{stem}_{suffix}.json"));
                suffix = suffix
                    .checked_add(1)
                    .context("history receipt collision suffix overflowed")?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to create {}", path.display()));
            }
        }
    }
}

fn git_stamp() -> (Option<String>, Option<bool>) {
    git_stamp_at(None)
}

fn git_stamp_at(cwd: Option<&Path>) -> (Option<String>, Option<bool>) {
    let mut revision = Command::new("git");
    revision.args(["rev-parse", "--short=12", "HEAD"]);
    if let Some(path) = cwd {
        revision.current_dir(path);
    }
    let Ok(revision) = revision.output() else {
        return (None, None);
    };
    if !revision.status.success() {
        return (None, None);
    }
    let sha = String::from_utf8_lossy(&revision.stdout).trim().to_string();
    if sha.is_empty() {
        return (None, None);
    }

    let mut status = Command::new("git");
    status.args(["status", "--porcelain"]);
    if let Some(path) = cwd {
        status.current_dir(path);
    }
    let Ok(status) = status.output() else {
        return (None, None);
    };
    if !status.status.success() {
        return (None, None);
    }

    (Some(sha), Some(!status.stdout.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::{Baseline, compare};
    use crate::case::LlmTrace;
    use crate::grader::{GradeCategory, GradeResult};
    use crate::record::{RECORD_SCHEMA, RunRecord, SandboxStamp};
    use crate::report::CaseReport;
    use crate::stats::{RepeatStats, RunSample};

    fn record(case_id: &str) -> RunRecord {
        RunRecord {
            schema: RECORD_SCHEMA.to_string(),
            mode: Mode::Replay,
            case_id: case_id.to_string(),
            case_hash: "case-hash".to_string(),
            provider_ref: "scripted".to_string(),
            tool_surface: vec!["echo".to_string()],
            sandbox: SandboxStamp {
                autonomy: "supervised".to_string(),
                workspace_only: true,
            },
            final_response: "private transcript sentinel".to_string(),
            history: Vec::new(),
            tools_called: vec!["echo".to_string()],
            all_tools_succeeded: true,
            input_tokens: 12,
            output_tokens: 4,
            duration_ms: 8,
            llm_calls: 1,
            judge_ref: None,
            judge_usage: None,
        }
    }

    fn grade(passed: bool) -> GradeResult {
        GradeResult {
            check: "response_contains".to_string(),
            passed,
            detail: "private grade detail sentinel".to_string(),
            category: GradeCategory::Response,
            diagnostic: false,
        }
    }

    fn case(case_id: &str, passed: bool) -> CaseReport {
        CaseReport {
            name: case_id.to_string(),
            source: "case.json".to_string(),
            record: Some(record(case_id)),
            grades: vec![grade(passed)],
            error: None,
            repeat: None,
            cluster: None,
        }
    }

    fn build_receipt(report: &SuiteReport) -> HistoryReceipt {
        HistoryReceipt::from_report(
            report,
            HistoryRun {
                recorded_at: DateTime::parse_from_rfc3339("2026-07-21T12:34:56Z")
                    .expect("test timestamp is valid")
                    .with_timezone(&Utc),
                suite_dir: "evals/regression".to_string(),
                suite_kind: SuiteKind::Regression,
                mode: Mode::Replay,
                provider_ref: "scripted".to_string(),
            },
            None,
        )
        .expect("test suite path is valid")
    }

    fn contains_forbidden_key(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
                matches!(key.as_str(), "final_response" | "history")
                    || contains_forbidden_key(value)
            }),
            serde_json::Value::Array(values) => values.iter().any(contains_forbidden_key),
            _ => false,
        }
    }

    #[tokio::test]
    async fn history_receipt_contains_no_transcripts() {
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "history-privacy",
                "turns": [{"user_input":"hello","steps":[{"response":{
                    "type":"text","content":"private transcript sentinel"
                }}]}],
                "expects":{"max_tool_calls":0}
            }"#,
        )
        .expect("test trace is valid");
        let outcome = crate::run_case(&trace, &crate::RunDeps::replay())
            .await
            .expect("replay run succeeds");
        let mut grades = outcome.grades;
        grades.push(GradeResult {
            check: "response_contains(\"private expectation sentinel\")".to_string(),
            passed: true,
            detail: "private grade detail sentinel".to_string(),
            category: GradeCategory::Response,
            diagnostic: false,
        });
        let repeat = RepeatStats::from_runs(
            2,
            &[
                RunSample {
                    passed: true,
                    total_tokens: 10,
                    duration_ms: 20,
                    checks: vec![(
                        "file_contains(\"private/path\", \"private workspace sentinel\")"
                            .to_string(),
                        true,
                    )],
                },
                RunSample {
                    passed: false,
                    total_tokens: 12,
                    duration_ms: 24,
                    checks: vec![(
                        "file_contains(\"private/path\", \"private workspace sentinel\")"
                            .to_string(),
                        false,
                    )],
                },
            ],
        );
        let report = SuiteReport {
            cases: vec![CaseReport {
                name: trace.display_id().to_string(),
                source: "privacy.json".to_string(),
                record: Some(outcome.record),
                grades,
                error: None,
                repeat: Some(repeat),
                cluster: None,
            }],
        };
        let value = serde_json::to_value(build_receipt(&report)).expect("receipt serializes");
        assert!(!contains_forbidden_key(&value));
        let text = serde_json::to_string(&value).expect("value serializes");
        assert!(!text.contains("private transcript sentinel"));
        assert!(!text.contains("private expectation sentinel"));
        assert!(!text.contains("private workspace sentinel"));
        assert!(!text.contains("private/path"));
        assert!(!text.contains("private grade detail sentinel"));
    }

    #[test]
    fn history_receipt_records_envelope_and_cases() {
        let receipt = build_receipt(&SuiteReport {
            cases: vec![case("case-a", true)],
        });
        let json = serde_json::to_string(&receipt).expect("receipt serializes");
        let parsed: HistoryReceipt = serde_json::from_str(&json).expect("receipt deserializes");
        assert_eq!(parsed.schema, HISTORY_SCHEMA);
        assert_eq!(parsed.recorded_at.to_rfc3339(), "2026-07-21T12:34:56+00:00");
        assert_eq!(parsed.zeroclaw_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed.suite, "regression");
        assert_eq!(parsed.passed, 1);
        assert_eq!(parsed.cases[0].case_id, "case-a");
    }

    #[test]
    fn history_filename_collision_suffixes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut receipt = build_receipt(&SuiteReport {
            cases: vec![case("case-a", true)],
        });
        receipt.git_sha = Some("abcdef123456".to_string());
        let first = write_history_receipt(dir.path(), &receipt).expect("first write succeeds");
        let second = write_history_receipt(dir.path(), &receipt).expect("second write succeeds");
        assert_eq!(
            first.file_name().and_then(|name| name.to_str()),
            Some("20260721T123456Z-abcdef123456.json")
        );
        assert_eq!(
            second.file_name().and_then(|name| name.to_str()),
            Some("20260721T123456Z-abcdef123456_1.json")
        );
        for path in [&first, &second] {
            let json = std::fs::read_to_string(path).expect("published receipt is readable");
            serde_json::from_str::<HistoryReceipt>(&json)
                .expect("published receipt is complete JSON");
        }
        assert!(
            std::fs::read_dir(first.parent().expect("receipt has parent"))
                .expect("suite dir is readable")
                .all(|entry| {
                    entry
                        .expect("directory entry is readable")
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json")
                })
        );
    }

    #[test]
    fn history_write_failure_removes_partial_temp_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let receipt = build_receipt(&SuiteReport {
            cases: vec![case("case-a", true)],
        });
        let result = write_history_receipt_with(dir.path(), &receipt, |file, json| {
            file.write_all(&json[..json.len() / 2])?;
            Err(std::io::Error::other("injected write failure"))
        });
        assert!(result.is_err());
        let suite_dir = dir.path().join("regression");
        assert_eq!(
            std::fs::read_dir(suite_dir)
                .expect("suite dir is readable")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn history_writer_rejects_symlinked_suite_directory() {
        let dir = tempfile::tempdir().expect("history root");
        let outside = tempfile::tempdir().expect("outside dir");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("regression"))
            .expect("suite symlink is created");
        let receipt = build_receipt(&SuiteReport {
            cases: vec![case("case-a", true)],
        });
        let error = write_history_receipt(dir.path(), &receipt)
            .expect_err("suite symlink must be rejected");
        assert!(error.to_string().contains("not a real directory"));
        assert_eq!(
            std::fs::read_dir(outside.path())
                .expect("outside dir is readable")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn history_writer_detects_suite_directory_swap_during_write() {
        let dir = tempfile::tempdir().expect("history root");
        let outside = tempfile::tempdir().expect("outside dir");
        let held = dir.path().join("held-suite");
        let receipt = build_receipt(&SuiteReport {
            cases: vec![case("case-a", true)],
        });
        let result = write_history_receipt_with(dir.path(), &receipt, |file, json| {
            file.write_all(json)?;
            std::fs::rename(dir.path().join("regression"), &held)?;
            std::os::unix::fs::symlink(outside.path(), dir.path().join("regression"))?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_dir(outside.path())
                .expect("outside dir is readable")
                .count(),
            0
        );
    }

    #[test]
    fn history_create_new_fallback_preserves_collisions() {
        let dir = tempfile::tempdir().expect("history root");
        let suite_dir = dir.path().join("regression");
        std::fs::create_dir(&suite_dir).expect("suite dir");
        let suite_dir = std::fs::canonicalize(suite_dir).expect("suite dir resolves");
        let receipt = build_receipt(&SuiteReport {
            cases: vec![case("case-a", true)],
        });
        let mut json = serde_json::to_vec_pretty(&receipt).expect("receipt serializes");
        json.push(b'\n');
        let first = write_history_receipt_without_hard_links(&suite_dir, "fallback", &json)
            .expect("first fallback write");
        let second = write_history_receipt_without_hard_links(&suite_dir, "fallback", &json)
            .expect("second fallback write");
        assert_eq!(
            first.file_name().and_then(|name| name.to_str()),
            Some("fallback.json")
        );
        assert_eq!(
            second.file_name().and_then(|name| name.to_str()),
            Some("fallback_1.json")
        );
        for path in [first, second] {
            let text = std::fs::read_to_string(path).expect("fallback receipt is readable");
            serde_json::from_str::<HistoryReceipt>(&text)
                .expect("fallback receipt is complete JSON");
        }
    }

    #[test]
    fn errored_case_recorded_with_error_flag() {
        let report = SuiteReport {
            cases: vec![CaseReport {
                name: "errored-case".to_string(),
                source: "error.json".to_string(),
                record: None,
                grades: Vec::new(),
                error: Some("private provider payload".to_string()),
                repeat: None,
                cluster: None,
            }],
        };
        let receipt = HistoryReceipt::from_report(
            &report,
            HistoryRun {
                recorded_at: Utc::now(),
                suite_dir: "evals/live".to_string(),
                suite_kind: SuiteKind::Capability,
                mode: Mode::Live,
                provider_ref: "anthropic.eval:model-x".to_string(),
            },
            None,
        )
        .expect("receipt builds");
        let case = &receipt.cases[0];
        assert_eq!(case.verdict, Verdict::Fail);
        assert!(case.error);
        assert_eq!(case.provider_ref, "anthropic.eval:model-x");
        assert_eq!(case.score, 0.0);
        assert!(case.checks.is_empty());
        assert!(
            !serde_json::to_string(&receipt)
                .expect("receipt serializes")
                .contains("private provider payload")
        );
    }

    #[test]
    fn baseline_comparison_class_recorded_when_baseline_given() {
        let baseline_report = SuiteReport {
            cases: vec![case("case-a", true)],
        };
        let baseline = Baseline::from_report(&baseline_report);
        let current = SuiteReport {
            cases: vec![case("case-a", false)],
        };
        let comparison = compare(&current, &baseline);
        let receipt = HistoryReceipt::from_report(
            &current,
            HistoryRun {
                recorded_at: Utc::now(),
                suite_dir: "evals/regression".to_string(),
                suite_kind: SuiteKind::Regression,
                mode: Mode::Replay,
                provider_ref: "scripted".to_string(),
            },
            Some(&comparison),
        )
        .expect("receipt builds");
        assert_eq!(
            receipt.cases[0].baseline_comparison.as_deref(),
            Some("regression")
        );
        assert_eq!(receipt.cases[0].regression_categories, vec!["response"]);
    }

    #[test]
    fn repeat_stats_survive_history_round_trip() {
        let mut repeated = case("case-a", true);
        repeated.repeat = Some(RepeatStats::from_runs(
            2,
            &[
                RunSample {
                    passed: true,
                    total_tokens: 10,
                    duration_ms: 20,
                    checks: vec![("response_contains".to_string(), true)],
                },
                RunSample {
                    passed: false,
                    total_tokens: 14,
                    duration_ms: 24,
                    checks: vec![("response_contains".to_string(), false)],
                },
            ],
        ));
        let receipt = build_receipt(&SuiteReport {
            cases: vec![repeated],
        });
        let json = serde_json::to_string(&receipt).expect("receipt serializes");
        let parsed: HistoryReceipt = serde_json::from_str(&json).expect("receipt deserializes");
        let repeat = parsed.cases[0].repeat.as_ref().expect("repeat is present");
        assert_eq!(repeat.k, 2);
        assert_eq!(repeat.passes, 1);
        assert_eq!(repeat.check_flips["response_contains:1"], 1);
    }

    #[test]
    fn git_stamp_absent_outside_repo() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(git_stamp_at(Some(dir.path())), (None, None));
    }
}
