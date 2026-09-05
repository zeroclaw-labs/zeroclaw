//! Git-versioned baseline files and per-case regression diffing.
//!
//! A baseline captures each case's verdict and comparability key from a prior
//! run. A later run compares against it per case id: gating is strictly on
//! per-case confirmed Pass -> Fail flips, never on aggregate score deltas.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Mode;
use crate::grader::GradeCategory;
use crate::record::{SandboxStamp, ToolSurface};
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
#[serde(deny_unknown_fields)]
pub struct BaselineEntry {
    pub case_id: String,
    pub case_hash: String,
    pub mode: Mode,
    pub provider_ref: String,
    pub tool_surface: ToolSurface,
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
    fn key(&self) -> (&str, Mode, &str, &ToolSurface, &SandboxStamp) {
        (
            self.case_hash.as_str(),
            self.mode,
            self.provider_ref.as_str(),
            &self.tool_surface,
            &self.sandbox,
        )
    }
}

/// A baseline file: every case's entry from a prior run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub schema: String,
    pub entries: Vec<BaselineEntry>,
}

impl Baseline {
    /// Build a baseline from a completed suite report.
    ///
    /// Fails closed: a baseline must describe **every completed, gradeable** case
    /// in the suite. Provenance exists even for an execution error, but that does
    /// not make the failed attempt a trustworthy reference run. Silently omitting
    /// or recording it would weaken later Pass-to-Fail classification.
    pub fn from_report(report: &SuiteReport) -> anyhow::Result<Baseline> {
        let mut incomplete: Vec<&str> = report
            .cases
            .iter()
            .filter(|case| {
                case.error.is_some()
                    || case.score().is_none()
                    || !case
                        .record
                        .as_ref()
                        .is_some_and(crate::record::RunRecord::is_complete)
            })
            .map(|c| c.name.as_str())
            .collect();
        if !incomplete.is_empty() {
            incomplete.sort_unstable();
            anyhow::bail!(
                "refusing to write a baseline: {} case(s) did not complete a gradeable run ({}); \
                 fix the run errors and regenerate",
                incomplete.len(),
                incomplete.join(", ")
            );
        }
        let entries = report
            .cases
            .iter()
            .map(entry_from_case)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Baseline {
            schema: BASELINE_SCHEMA.to_string(),
            entries,
        })
    }

    /// Serialize as pretty JSON.
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
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

fn entry_from_case(case: &CaseReport) -> anyhow::Result<BaselineEntry> {
    let rec = case
        .record
        .as_ref()
        .ok_or_else(|| anyhow::Error::msg(format!("case {:?} has no run record", case.name)))?;
    let completion = rec.completion.as_ref().ok_or_else(|| {
        anyhow::Error::msg(format!("case {:?} has no completion data", case.name))
    })?;
    let score = case
        .score()
        .ok_or_else(|| anyhow::Error::msg(format!("case {:?} produced no grades", case.name)))?;
    Ok(BaselineEntry {
        case_id: rec.provenance.case_id.clone(),
        case_hash: rec.provenance.case_hash.clone(),
        mode: rec.provenance.mode,
        provider_ref: rec.provenance.provider_ref.clone(),
        tool_surface: rec.provenance.tool_surface.clone(),
        sandbox: rec.provenance.sandbox.clone(),
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
        total_tokens: completion
            .input_tokens
            .saturating_add(completion.output_tokens),
        score,
    })
}

/// The per-case classification of a comparison against a baseline.
#[derive(Debug, Clone, PartialEq)]
pub enum CaseComparison {
    /// Present now, absent from the baseline.
    New,
    /// In the baseline, absent now (warned, never gated).
    Removed,
    /// The current run errored; provenance may exist, but no trustworthy
    /// completed comparison exists. Always gates (a run error, not a check failure).
    CurrentError,
    /// Comparability key changed; cannot be compared or gated.
    Unverifiable,
    /// Baseline passed, current failed: a confirmed regression, with the
    /// categories whose checks flipped.
    Regression { categories: Vec<GradeCategory> },
    /// A live regression whose re-run cleared the case's effective repeat
    /// policy (pass^k): reported, never gated.
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

    /// Count of current cases whose run errored.
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

fn current_key(rec: &crate::record::RunRecord) -> (&str, Mode, &str, &ToolSurface, &SandboxStamp) {
    (
        rec.provenance.case_hash.as_str(),
        rec.provenance.mode,
        rec.provenance.provider_ref.as_str(),
        &rec.provenance.tool_surface,
        &rec.provenance.sandbox,
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
        // Provenance normally supplies the canonical id even for an errored case;
        // the report name is a fail-closed fallback for synthetic callers.
        let id = case
            .record
            .as_ref()
            .map(|r| r.provenance.case_id.as_str())
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
                                    let completion = rec.completion_or_default();
                                    let cur_total = completion
                                        .input_tokens
                                        .saturating_add(completion.output_tokens);
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

/// Downgrade live regressions whose re-run confirmed a pass to
/// `FlakyUnconfirmed` (reported, never gated). Only applies when `mode` is Live;
/// replay flips the gate directly with no retry (deterministic).
/// `rerun_passed[case_id] == true` means that case's re-run cleared its
/// effective repeat policy (pass^k), not merely one lucky attempt. Returns the
/// case ids downgraded to flaky.
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
    use crate::record::{
        CaseProvenance, RECORD_SCHEMA, RunCompletion, RunRecord, SandboxStamp, ToolSurface,
    };

    fn rec(case_id: &str, tokens: u64) -> RunRecord {
        RunRecord {
            provenance: CaseProvenance {
                schema: RECORD_SCHEMA.to_string(),
                mode: Mode::Replay,
                case_id: case_id.to_string(),
                case_hash: "hash".to_string(),
                provider_ref: "scripted".to_string(),
                tool_surface: ToolSurface::default(),
                sandbox: SandboxStamp {
                    autonomy: "supervised".to_string(),
                    workspace_only: false,
                },
            },
            completion: Some(RunCompletion {
                input_tokens: tokens,
                ..RunCompletion::default()
            }),
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
            repeat: None,
            cluster: None,
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
        failing.record.as_mut().unwrap().provenance.case_hash = "different".to_string();
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
    fn from_report_rejects_errored_run_with_provenance_only() {
        // The runner now preserves provenance for execution errors. That receipt
        // makes the failed attempt diagnosable, but it is still not a completed
        // reference run and must not become a baseline.
        let mut errored = case("errored", Vec::new(), 0);
        let provenance = errored.record.take().unwrap().provenance;
        errored.record = Some(RunRecord::from_provenance(provenance));
        errored.error = Some("provider exploded".to_string());
        let report = SuiteReport {
            cases: vec![errored],
        };

        let error = Baseline::from_report(&report)
            .expect_err("provenance-only errored runs must not become baselines");
        assert!(error.to_string().contains("errored"));
        assert!(error.to_string().contains("did not complete"));
    }

    #[test]
    fn from_report_accepts_a_complete_report() {
        // Every case has completion data and grades, including a *failing* one.
        // An execution error blocks the write; an honest check failure does not.
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
        let parsed = Baseline::from_json(&baseline.to_json().unwrap()).unwrap();
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
        current.record.as_mut().unwrap().provenance.mode = Mode::Live;
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
        let err = Baseline::from_json(&baseline.to_json().unwrap()).unwrap_err();
        assert!(err.to_string().contains("unsupported baseline schema"));
        // The message must tell the operator how to recover after the entry's
        // capability key widened.
        assert!(
            err.to_string().contains("--write-baseline"),
            "the rejection must name the regeneration step: {err}"
        );
        // Arbitrary non-baseline schema strings are rejected too.
        baseline.schema = "not-a-baseline".to_string();
        assert!(Baseline::from_json(&baseline.to_json().unwrap()).is_err());
    }

    #[test]
    fn from_json_rejects_unknown_fields_at_every_schema_level() {
        let report = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = Baseline::from_report(&report).unwrap();
        let original: serde_json::Value =
            serde_json::from_str(&baseline.to_json().unwrap()).unwrap();

        for path in ["root", "entry", "tool_surface", "sandbox"] {
            let mut changed = original.clone();
            let object = match path {
                "root" => changed.as_object_mut().unwrap(),
                "entry" => changed["entries"][0].as_object_mut().unwrap(),
                "tool_surface" => changed["entries"][0]["tool_surface"]
                    .as_object_mut()
                    .unwrap(),
                "sandbox" => changed["entries"][0]["sandbox"].as_object_mut().unwrap(),
                _ => unreachable!(),
            };
            object.insert("unexpected".to_string(), serde_json::json!(true));
            let text = serde_json::to_string(&changed).unwrap();
            assert!(
                Baseline::from_json(&text).is_err(),
                "unknown field at {path} must be rejected"
            );
        }
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
        let err = Baseline::from_json(&baseline.to_json().unwrap()).unwrap_err();
        assert!(err.to_string().contains("duplicate case_id"));

        let empty_report = SuiteReport {
            cases: vec![case(
                "",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let empty_baseline = Baseline::from_report(&empty_report).unwrap();
        let err = Baseline::from_json(&empty_baseline.to_json().unwrap()).unwrap_err();
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
        failing.record.as_mut().unwrap().provenance.sandbox = SandboxStamp {
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
    fn changed_effective_or_registered_tool_surface_is_unverifiable() {
        let pass = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("c", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = baseline_of(&pass);
        let changed_surfaces = [
            ToolSurface {
                requested: Vec::new(),
                effective: vec!["web".to_string()],
                registered: Vec::new(),
            },
            ToolSurface {
                requested: Vec::new(),
                effective: Vec::new(),
                registered: vec!["echo".to_string()],
            },
        ];

        for tool_surface in changed_surfaces {
            let mut failing = case("a", vec![grade("c", false, GradeCategory::Response)], 10);
            failing.record.as_mut().unwrap().provenance.tool_surface = tool_surface;
            let current = SuiteReport {
                cases: vec![failing],
            };
            let cmp = compare(&current, &baseline).unwrap();
            assert_eq!(cmp.per_case["a"], CaseComparison::Unverifiable);
            assert_eq!(cmp.confirmed_regressions(), 0);
        }
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
            repeat: None,
            cluster: None,
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
