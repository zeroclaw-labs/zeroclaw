//! Git-versioned baseline files and per-case regression diffing.
//!
//! A baseline captures each case's verdict and comparability key from a prior
//! run. A later run compares against it per case id: gating is strictly on
//! per-case confirmed Pass -> Fail flips, never on aggregate score deltas.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Mode;
use crate::grader::GradeCategory;
use crate::record::SandboxStamp;
use crate::report::{CaseReport, SuiteReport};

/// The schema tag stamped on every baseline file.
pub const BASELINE_SCHEMA: &str = "zeroclaw-eval/baseline/v1";

/// Whether a suite gates CI. A suite directory named `capability` (or the
/// `--suite-kind capability` override) is tracked but never gating; everything
/// else has regression semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteKind {
    Regression,
    Capability,
}

impl SuiteKind {
    /// The snake_case label used in machine-readable output.
    pub fn as_str(self) -> &'static str {
        match self {
            SuiteKind::Regression => "regression",
            SuiteKind::Capability => "capability",
        }
    }

    /// Resolve the suite kind from the suite directory's final component,
    /// unless an explicit override is given.
    pub fn resolve(suite_dir: &std::path::Path, override_kind: Option<SuiteKind>) -> SuiteKind {
        if let Some(kind) = override_kind {
            return kind;
        }
        match suite_dir.file_name().and_then(|n| n.to_str()) {
            Some("capability") => SuiteKind::Capability,
            _ => SuiteKind::Regression,
        }
    }
}

/// A case's pass/fail verdict as recorded in a baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
}

/// One case's entry in a baseline file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub case_id: String,
    pub case_hash: String,
    pub mode: Mode,
    pub provider_ref: String,
    pub tool_surface: Vec<String>,
    /// The sandbox posture the baseline run executed under. Part of the
    /// comparability key: runs under different sandbox policies are not
    /// comparable.
    pub sandbox: SandboxStamp,
    pub verdict: Verdict,
    /// Per-check pass/fail, keyed by check name.
    pub checks: BTreeMap<String, bool>,
    pub total_tokens: u64,
    pub score: f64,
}

impl BaselineEntry {
    /// The comparability key: two entries are comparable only when these agree.
    fn key(&self) -> (&str, Mode, &str, &[String], &SandboxStamp) {
        (
            self.case_hash.as_str(),
            self.mode,
            self.provider_ref.as_str(),
            self.tool_surface.as_slice(),
            &self.sandbox,
        )
    }
}

/// A baseline file: every case's entry from a prior run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub schema: String,
    pub entries: Vec<BaselineEntry>,
}

impl Baseline {
    /// Build a baseline from a completed suite report.
    ///
    /// Fails closed: a baseline must describe **every** case in the suite. A case
    /// that errored before producing a `RunRecord` cannot be represented, and
    /// silently omitting it would make the case merely `New` on a later run —
    /// and a failing `New` case is explicitly non-gating, so an incomplete
    /// reference permanently excuses a regression.
    pub fn from_report(report: &SuiteReport) -> anyhow::Result<Baseline> {
        let mut missing: Vec<&str> = report
            .cases
            .iter()
            .filter(|c| c.record.is_none())
            .map(|c| c.name.as_str())
            .collect();
        if !missing.is_empty() {
            missing.sort_unstable();
            anyhow::bail!(
                "refusing to write a baseline: {} case(s) produced no run record ({}); \
                 fix the run errors and regenerate",
                missing.len(),
                missing.join(", ")
            );
        }
        let entries = report.cases.iter().filter_map(entry_from_case).collect();
        Ok(Baseline {
            schema: BASELINE_SCHEMA.to_string(),
            entries,
        })
    }

    /// Serialize as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Parse and validate a baseline from JSON text. Fails closed: an
    /// unrecognized schema tag, an empty case id, or a duplicate case id is an
    /// error, never a silently accepted or collapsed input.
    pub fn from_json(text: &str) -> anyhow::Result<Baseline> {
        let baseline: Baseline = serde_json::from_str(text)?;
        if baseline.schema != BASELINE_SCHEMA {
            anyhow::bail!(
                "unsupported baseline schema {:?} (expected {BASELINE_SCHEMA:?}); \
                 regenerate the baseline once with --write-baseline on a known-green run",
                baseline.schema
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &baseline.entries {
            if entry.case_id.is_empty() {
                anyhow::bail!("baseline entry with empty case_id");
            }
            if !seen.insert(entry.case_id.as_str()) {
                anyhow::bail!(
                    "duplicate case_id {:?} in baseline: one entry could silently mask another",
                    entry.case_id
                );
            }
        }
        Ok(baseline)
    }
}

fn entry_from_case(case: &CaseReport) -> Option<BaselineEntry> {
    let rec = case.record.as_ref()?;
    Some(BaselineEntry {
        case_id: rec.case_id.clone(),
        case_hash: rec.case_hash.clone(),
        mode: rec.mode,
        provider_ref: rec.provider_ref.clone(),
        tool_surface: rec.tool_surface.clone(),
        sandbox: rec.sandbox.clone(),
        verdict: if case.passed() {
            Verdict::Pass
        } else {
            Verdict::Fail
        },
        checks: case
            .grades
            .iter()
            .map(|g| (g.check.clone(), g.passed))
            .collect(),
        total_tokens: rec.input_tokens + rec.output_tokens,
        score: case.score(),
    })
}

/// The per-case classification of a comparison against a baseline.
#[derive(Debug, Clone, PartialEq)]
pub enum CaseComparison {
    /// Present now, absent from the baseline.
    New,
    /// In the baseline, absent now (warned, never gated).
    Removed,
    /// The current run errored before producing a record; no trustworthy
    /// comparison exists. Always gates (a run error, not a check failure).
    CurrentError,
    /// Comparability key changed; cannot be compared or gated.
    Unverifiable,
    /// Baseline passed, current failed: a confirmed regression, with the
    /// categories whose checks flipped.
    Regression { categories: Vec<GradeCategory> },
    /// A live regression that passed on a single re-run: reported, never gated.
    FlakyUnconfirmed,
    /// Current passed, baseline failed (reported, never gated).
    Improvement,
    /// No verdict flip. `token_delta_pct` is informational when comparable.
    Unchanged { token_delta_pct: Option<f64> },
}

/// The full comparison of a suite report against a baseline, keyed by case id.
#[derive(Debug, Clone)]
pub struct BaselineComparison {
    pub per_case: BTreeMap<String, CaseComparison>,
}

impl BaselineComparison {
    /// Count of confirmed regressions (excludes flaky-unconfirmed).
    pub fn confirmed_regressions(&self) -> usize {
        self.per_case
            .values()
            .filter(|c| matches!(c, CaseComparison::Regression { .. }))
            .count()
    }

    /// Count of current cases that errored before producing a record.
    pub fn current_errors(&self) -> usize {
        self.per_case
            .values()
            .filter(|c| matches!(c, CaseComparison::CurrentError))
            .count()
    }

    /// Whether this comparison gates the run: at least one confirmed
    /// regression or one current run error. The single gating authority for
    /// baseline runs.
    pub fn gates(&self) -> bool {
        self.confirmed_regressions() > 0 || self.current_errors() > 0
    }

    /// Case ids removed since the baseline (warned).
    pub fn removed(&self) -> Vec<&str> {
        self.per_case
            .iter()
            .filter(|(_, c)| matches!(c, CaseComparison::Removed))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Count of live regressions downgraded to flaky-unconfirmed.
    pub fn flaky_unconfirmed(&self) -> usize {
        self.per_case
            .values()
            .filter(|c| matches!(c, CaseComparison::FlakyUnconfirmed))
            .count()
    }

    /// The comparison as a JSON value for machine-readable reports: per-case
    /// classifications plus the aggregate gate summary. The `exit_code` a CI
    /// artifact would otherwise have to reverse-engineer is carried explicitly
    /// by the caller embedding this value.
    pub fn to_json_value(&self) -> serde_json::Value {
        let per_case: serde_json::Map<String, serde_json::Value> = self
            .per_case
            .iter()
            .map(|(id, c)| {
                let v = match c {
                    CaseComparison::New => serde_json::json!({ "classification": "new" }),
                    CaseComparison::Removed => serde_json::json!({ "classification": "removed" }),
                    CaseComparison::CurrentError => {
                        serde_json::json!({ "classification": "current_error" })
                    }
                    CaseComparison::Unverifiable => {
                        serde_json::json!({ "classification": "unverifiable" })
                    }
                    CaseComparison::Regression { categories } => {
                        let cats: Vec<&str> = categories.iter().map(|c| c.as_str()).collect();
                        serde_json::json!({ "classification": "regression", "categories": cats })
                    }
                    CaseComparison::FlakyUnconfirmed => {
                        serde_json::json!({ "classification": "flaky_unconfirmed" })
                    }
                    CaseComparison::Improvement => {
                        serde_json::json!({ "classification": "improvement" })
                    }
                    CaseComparison::Unchanged { token_delta_pct } => serde_json::json!({
                        "classification": "unchanged",
                        "token_delta_pct": token_delta_pct,
                    }),
                };
                (id.clone(), v)
            })
            .collect();
        serde_json::json!({
            "per_case": per_case,
            "confirmed_regressions": self.confirmed_regressions(),
            "current_errors": self.current_errors(),
            "flaky_unconfirmed": self.flaky_unconfirmed(),
            "gates": self.gates(),
        })
    }
}

fn current_key(rec: &crate::record::RunRecord) -> (&str, Mode, &str, &[String], &SandboxStamp) {
    (
        rec.case_hash.as_str(),
        rec.mode,
        rec.provider_ref.as_str(),
        rec.tool_surface.as_slice(),
        &rec.sandbox,
    )
}

/// The distinct categories of the current case's failing grades.
fn flipped_categories(case: &CaseReport) -> Vec<GradeCategory> {
    let mut out: Vec<GradeCategory> = Vec::new();
    for g in case.grades.iter().filter(|g| !g.passed) {
        if !out.contains(&g.category) {
            out.push(g.category);
        }
    }
    out
}

/// Compare a suite report against a baseline, keyed by case id. Pure: the live
/// flakiness retry is applied separately by the caller.
///
/// Fails closed: duplicate case ids in the current run are an error (one
/// result could silently mask another). The baseline side is validated at
/// parse time by [`Baseline::from_json`]. A current case that errored before
/// producing a record is classified [`CaseComparison::CurrentError`] rather
/// than being absent (which would misreport it as `Removed`).
pub fn compare(current: &SuiteReport, baseline: &Baseline) -> anyhow::Result<BaselineComparison> {
    let base_map: BTreeMap<&str, &BaselineEntry> = baseline
        .entries
        .iter()
        .map(|e| (e.case_id.as_str(), e))
        .collect();

    let mut cur_map: BTreeMap<&str, &CaseReport> = BTreeMap::new();
    for case in &current.cases {
        // An errored case has no record; its report identity is its name.
        let id = case
            .record
            .as_ref()
            .map(|r| r.case_id.as_str())
            .unwrap_or(case.name.as_str());
        if id.is_empty() {
            anyhow::bail!("current run case with empty id cannot be compared");
        }
        if cur_map.insert(id, case).is_some() {
            anyhow::bail!(
                "duplicate case id {id:?} in current run: one result could silently mask another"
            );
        }
    }

    let mut per_case = BTreeMap::new();
    let ids: std::collections::BTreeSet<&str> =
        base_map.keys().chain(cur_map.keys()).copied().collect();

    for id in ids {
        let classification = match (cur_map.get(id), base_map.get(id)) {
            (Some(case), _) if case.error.is_some() => CaseComparison::CurrentError,
            (Some(case), base) => match case.record.as_ref() {
                // No record and no error should not happen, but if it does the
                // case cannot be compared; fail closed as a current error.
                None => CaseComparison::CurrentError,
                Some(rec) => match base {
                    None => CaseComparison::New,
                    Some(base) => {
                        if current_key(rec) != base.key() {
                            CaseComparison::Unverifiable
                        } else {
                            let base_pass = base.verdict == Verdict::Pass;
                            let cur_pass = case.passed();
                            match (base_pass, cur_pass) {
                                (true, false) => CaseComparison::Regression {
                                    categories: flipped_categories(case),
                                },
                                (false, true) => CaseComparison::Improvement,
                                _ => {
                                    let cur_total = rec.input_tokens + rec.output_tokens;
                                    let delta = token_delta_pct(base.total_tokens, cur_total);
                                    CaseComparison::Unchanged {
                                        token_delta_pct: delta,
                                    }
                                }
                            }
                        }
                    }
                },
            },
            (None, Some(_)) => CaseComparison::Removed,
            (None, None) => unreachable!("id came from one of the maps"),
        };
        per_case.insert(id.to_string(), classification);
    }

    Ok(BaselineComparison { per_case })
}

/// Downgrade live regressions that passed on a single re-run to
/// `FlakyUnconfirmed` (reported, never gated). Only applies when `mode` is Live;
/// replay flips the gate directly with no retry (deterministic).
/// `rerun_passed[case_id] == true` means that case's one re-run passed. Returns
/// the case ids downgraded to flaky.
pub fn downgrade_flaky_regressions(
    comparison: &mut BaselineComparison,
    mode: Mode,
    rerun_passed: &BTreeMap<String, bool>,
) -> Vec<String> {
    if mode != Mode::Live {
        return Vec::new();
    }
    let mut flaky = Vec::new();
    for (id, classification) in comparison.per_case.iter_mut() {
        let regressed = matches!(classification, CaseComparison::Regression { .. });
        if regressed && rerun_passed.get(id).copied().unwrap_or(false) {
            *classification = CaseComparison::FlakyUnconfirmed;
            flaky.push(id.clone());
        }
    }
    flaky
}

fn token_delta_pct(base: u64, current: u64) -> Option<f64> {
    if base == 0 {
        return None;
    }
    Some((current as f64 - base as f64) / base as f64 * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grader::{GradeCategory, GradeResult};
    use crate::record::{RECORD_SCHEMA, RunRecord, SandboxStamp};

    fn rec(case_id: &str, tokens: u64) -> RunRecord {
        RunRecord {
            schema: RECORD_SCHEMA.to_string(),
            mode: Mode::Replay,
            case_id: case_id.to_string(),
            case_hash: "hash".to_string(),
            provider_ref: "scripted".to_string(),
            tool_surface: Vec::new(),
            sandbox: SandboxStamp {
                autonomy: "supervised".to_string(),
                workspace_only: false,
            },
            final_response: String::new(),
            history: Vec::new(),
            tools_called: Vec::new(),
            all_tools_succeeded: true,
            input_tokens: tokens,
            output_tokens: 0,
            duration_ms: 0,
            llm_calls: 0,
        }
    }

    fn grade(check: &str, passed: bool, category: GradeCategory) -> GradeResult {
        GradeResult {
            check: check.to_string(),
            passed,
            detail: String::new(),
            category,
        }
    }

    fn case(id: &str, grades: Vec<GradeResult>, tokens: u64) -> CaseReport {
        CaseReport {
            name: id.to_string(),
            source: "f.json".to_string(),
            record: Some(rec(id, tokens)),
            grades,
            error: None,
        }
    }

    fn baseline_of(current: &SuiteReport) -> Baseline {
        Baseline::from_report(current).expect("every fixture case carries a record")
    }

    #[test]
    fn changed_case_hash_is_unverifiable_not_regression() {
        let pass = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = baseline_of(&pass);
        // Now the case fails AND its hash changed.
        let mut failing = case("a", vec![grade("c", false, GradeCategory::Response)], 10);
        failing.record.as_mut().unwrap().case_hash = "different".to_string();
        let current = SuiteReport {
            cases: vec![failing],
        };
        let cmp = compare(&current, &baseline).unwrap();
        assert_eq!(cmp.per_case["a"], CaseComparison::Unverifiable);
        assert_eq!(cmp.confirmed_regressions(), 0);
    }

    #[test]
    fn category_flip_classification() {
        let pass = SuiteReport {
            cases: vec![case(
                "a",
                vec![
                    grade("r", true, GradeCategory::Response),
                    grade("t", true, GradeCategory::Tool),
                ],
                10,
            )],
        };
        let baseline = baseline_of(&pass);
        let current = SuiteReport {
            cases: vec![case(
                "a",
                vec![
                    grade("r", true, GradeCategory::Response),
                    grade("t", false, GradeCategory::Tool),
                ],
                10,
            )],
        };
        let cmp = compare(&current, &baseline).unwrap();
        match &cmp.per_case["a"] {
            CaseComparison::Regression { categories } => {
                assert_eq!(categories, &vec![GradeCategory::Tool]);
            }
            other => panic!("expected tool regression, got {other:?}"),
        }
        assert_eq!(cmp.confirmed_regressions(), 1);
    }

    #[test]
    fn improvement_and_new_and_removed_and_unchanged() {
        // Baseline: a fails, b passes, c passes.
        let base_report = SuiteReport {
            cases: vec![
                case("a", vec![grade("c", false, GradeCategory::Response)], 10),
                case("b", vec![grade("c", true, GradeCategory::Response)], 10),
                case("c", vec![grade("c", true, GradeCategory::Response)], 100),
            ],
        };
        let baseline = baseline_of(&base_report);
        // Current: a passes (improvement), b passes+more tokens (unchanged), c gone
        // (removed), d new.
        let current = SuiteReport {
            cases: vec![
                case("a", vec![grade("c", true, GradeCategory::Response)], 10),
                case("b", vec![grade("c", true, GradeCategory::Response)], 20),
                case("d", vec![grade("c", true, GradeCategory::Response)], 10),
            ],
        };
        let cmp = compare(&current, &baseline).unwrap();
        assert_eq!(cmp.per_case["a"], CaseComparison::Improvement);
        assert!(matches!(
            cmp.per_case["b"],
            CaseComparison::Unchanged { .. }
        ));
        assert_eq!(cmp.per_case["c"], CaseComparison::Removed);
        assert_eq!(cmp.per_case["d"], CaseComparison::New);
        assert_eq!(cmp.removed(), vec!["c"]);
        assert_eq!(cmp.confirmed_regressions(), 0);
    }

    #[test]
    fn from_report_rejects_report_with_missing_record() {
        // A case that errored before producing a RunRecord cannot be represented in a
        // baseline. Skipping it would make the case merely `New` on a later run, and a
        // failing `New` case never gates — permanently excusing the regression.
        let mut errored = case("errored", vec![], 0);
        errored.record = None;
        errored.error = Some("provider exploded".to_string());
        let report = SuiteReport {
            cases: vec![
                case("ok", vec![grade("c", true, GradeCategory::Response)], 10),
                errored,
            ],
        };
        let err = Baseline::from_report(&report)
            .expect_err("a report with an errored case must not yield a baseline");
        let msg = err.to_string();
        assert!(
            msg.contains("errored"),
            "the error must name the offending case_id: {msg}"
        );
        assert!(
            msg.contains("refusing to write a baseline"),
            "the error must state the refusal: {msg}"
        );
    }

    #[test]
    fn from_report_accepts_a_complete_report() {
        // Every case has a record — including a *failing* one. Only a missing record
        // (a run error) blocks the write; an honest failure is baseline-able.
        let report = SuiteReport {
            cases: vec![
                case("pass", vec![grade("c", true, GradeCategory::Response)], 10),
                case("fail", vec![grade("c", false, GradeCategory::Response)], 10),
            ],
        };
        let baseline = Baseline::from_report(&report).expect("a complete report is baseline-able");
        assert_eq!(baseline.entries.len(), 2);
        assert_eq!(baseline.entries[0].verdict, Verdict::Pass);
        assert_eq!(baseline.entries[1].verdict, Verdict::Fail);
    }

    #[test]
    fn baseline_round_trips_through_json() {
        let report = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("c", true, GradeCategory::Response)],
                35,
            )],
        };
        let baseline = Baseline::from_report(&report).unwrap();
        let parsed = Baseline::from_json(&baseline.to_json()).unwrap();
        assert_eq!(parsed.schema, BASELINE_SCHEMA);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].case_id, "a");
        assert_eq!(parsed.entries[0].verdict, Verdict::Pass);
        assert_eq!(parsed.entries[0].total_tokens, 35);
    }

    #[test]
    fn live_flip_retries_once_and_reports_flaky() {
        // A live case that regressed; the single re-run passed, so it is downgraded
        // to flaky and no longer counts as a regression.
        let base_report = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = baseline_of(&base_report);
        let mut current = case("a", vec![grade("c", false, GradeCategory::Response)], 10);
        // Live-mode record so the flaky rule applies.
        current.record.as_mut().unwrap().mode = Mode::Live;
        let mut base_live = baseline;
        base_live.entries[0].mode = Mode::Live;
        let current = SuiteReport {
            cases: vec![current],
        };
        let mut cmp = compare(&current, &base_live).unwrap();
        assert_eq!(cmp.confirmed_regressions(), 1);

        let mut rerun = BTreeMap::new();
        rerun.insert("a".to_string(), true);
        let flaky = downgrade_flaky_regressions(&mut cmp, Mode::Live, &rerun);
        assert_eq!(flaky, vec!["a".to_string()]);
        assert_eq!(cmp.per_case["a"], CaseComparison::FlakyUnconfirmed);
        assert_eq!(cmp.confirmed_regressions(), 0);

        // Replay never retries: the regression stands.
        let mut cmp2 = compare(&current, &base_live).unwrap();
        let flaky2 = downgrade_flaky_regressions(&mut cmp2, Mode::Replay, &rerun);
        assert!(flaky2.is_empty());
        assert_eq!(cmp2.confirmed_regressions(), 1);
    }

    #[test]
    fn removed_case_warns() {
        let base_report = SuiteReport {
            cases: vec![case(
                "gone",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = baseline_of(&base_report);
        let current = SuiteReport { cases: vec![] };
        let cmp = compare(&current, &baseline).unwrap();
        assert_eq!(cmp.per_case["gone"], CaseComparison::Removed);
        assert_eq!(cmp.removed(), vec!["gone"]);
    }

    #[test]
    fn from_json_rejects_wrong_schema() {
        let mut baseline = Baseline {
            schema: "zeroclaw-eval/baseline/v999".to_string(),
            entries: Vec::new(),
        };
        let err = Baseline::from_json(&baseline.to_json()).unwrap_err();
        assert!(err.to_string().contains("unsupported baseline schema"));
        // The message must tell the operator how to recover: the `sandbox` field
        // widened the entry schema, so pre-existing baselines need one regeneration.
        assert!(
            err.to_string().contains("--write-baseline"),
            "the rejection must name the regeneration step: {err}"
        );
        // Arbitrary non-baseline schema strings are rejected too.
        baseline.schema = "not-a-baseline".to_string();
        assert!(Baseline::from_json(&baseline.to_json()).is_err());
    }

    #[test]
    fn from_json_rejects_duplicate_and_empty_case_ids() {
        let report = SuiteReport {
            cases: vec![
                case("dup", vec![grade("c", true, GradeCategory::Response)], 10),
                case("dup", vec![grade("c", false, GradeCategory::Response)], 10),
            ],
        };
        let baseline = Baseline::from_report(&report).unwrap();
        let err = Baseline::from_json(&baseline.to_json()).unwrap_err();
        assert!(err.to_string().contains("duplicate case_id"));

        let empty_report = SuiteReport {
            cases: vec![case(
                "",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let empty_baseline = Baseline::from_report(&empty_report).unwrap();
        let err = Baseline::from_json(&empty_baseline.to_json()).unwrap_err();
        assert!(err.to_string().contains("empty case_id"));
    }

    #[test]
    fn compare_rejects_duplicate_current_case_ids() {
        let base_report = SuiteReport {
            cases: vec![case(
                "dup",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = baseline_of(&base_report);
        let current = SuiteReport {
            cases: vec![
                case("dup", vec![grade("c", true, GradeCategory::Response)], 10),
                case("dup", vec![grade("c", false, GradeCategory::Response)], 10),
            ],
        };
        let err = compare(&current, &baseline).unwrap_err();
        assert!(err.to_string().contains("duplicate case id"));
    }

    #[test]
    fn changed_sandbox_posture_is_unverifiable_not_comparable() {
        let pass = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = baseline_of(&pass);
        // Same hash/mode/provider/tools, but the sandbox posture changed and
        // the case now fails: not comparable, must not be a regression.
        let mut failing = case("a", vec![grade("c", false, GradeCategory::Response)], 10);
        failing.record.as_mut().unwrap().sandbox = SandboxStamp {
            autonomy: "full".to_string(),
            workspace_only: true,
        };
        let current = SuiteReport {
            cases: vec![failing],
        };
        let cmp = compare(&current, &baseline).unwrap();
        assert_eq!(cmp.per_case["a"], CaseComparison::Unverifiable);
        assert_eq!(cmp.confirmed_regressions(), 0);
    }

    #[test]
    fn errored_current_case_is_current_error_not_removed() {
        let base_report = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = baseline_of(&base_report);
        // The case errored before producing a record: it is present in the
        // suite (by name) but absent from the record map.
        let errored = CaseReport {
            name: "a".to_string(),
            source: "f.json".to_string(),
            record: None,
            grades: Vec::new(),
            error: Some("trace exhausted".to_string()),
        };
        let current = SuiteReport {
            cases: vec![errored],
        };
        let cmp = compare(&current, &baseline).unwrap();
        assert_eq!(cmp.per_case["a"], CaseComparison::CurrentError);
        assert_eq!(cmp.current_errors(), 1);
        assert!(cmp.gates());
        assert!(cmp.removed().is_empty());
    }

    #[test]
    fn gates_reflects_regressions_and_current_errors_only() {
        let mut per_case = BTreeMap::new();
        per_case.insert("new".to_string(), CaseComparison::New);
        per_case.insert(
            "unchanged".to_string(),
            CaseComparison::Unchanged {
                token_delta_pct: None,
            },
        );
        per_case.insert("flaky".to_string(), CaseComparison::FlakyUnconfirmed);
        per_case.insert("changed".to_string(), CaseComparison::Unverifiable);
        let mut cmp = BaselineComparison { per_case };
        assert!(!cmp.gates());
        cmp.per_case.insert(
            "bad".to_string(),
            CaseComparison::Regression {
                categories: vec![GradeCategory::Response],
            },
        );
        assert!(cmp.gates());
    }

    #[test]
    fn suite_kind_resolves_capability_by_dir_name() {
        assert_eq!(
            SuiteKind::resolve(std::path::Path::new("evals/capability"), None),
            SuiteKind::Capability
        );
        assert_eq!(
            SuiteKind::resolve(std::path::Path::new("evals/regression"), None),
            SuiteKind::Regression
        );
        // Explicit override wins.
        assert_eq!(
            SuiteKind::resolve(
                std::path::Path::new("evals/regression"),
                Some(SuiteKind::Capability)
            ),
            SuiteKind::Capability
        );
    }
}
