//! Git-versioned baseline files and per-case regression diffing.
//!
//! A baseline captures each case's verdict and comparability key from a prior
//! run. A later run compares against it per case id: gating is strictly on
//! per-case confirmed Pass -> Fail flips, never on aggregate score deltas.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Mode;
use crate::grader::GradeCategory;
use crate::report::{CaseReport, SuiteReport};

/// The schema tag stamped on every baseline file.
pub const BASELINE_SCHEMA: &str = "zeroclaw-eval/baseline/v1";

/// Whether a suite gates CI. A suite directory named `capability` (or the
/// `--suite-kind capability` override) is tracked but never gating; everything
/// else has regression semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuiteKind {
    Regression,
    Capability,
}

impl SuiteKind {
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
    pub verdict: Verdict,
    /// Per-check pass/fail, keyed by check name.
    pub checks: BTreeMap<String, bool>,
    pub total_tokens: u64,
    pub score: f64,
    /// Judge provider reference when any judge rubric ran (joins the comparability
    /// key: a judge swap makes cases unverifiable, not silently compared).
    #[serde(default)]
    pub judge_ref: Option<String>,
}

/// The comparability key: two runs are comparable only when these agree.
type ComparabilityKey<'a> = (&'a str, Mode, &'a str, &'a [String], Option<&'a str>);

impl BaselineEntry {
    fn key(&self) -> ComparabilityKey<'_> {
        (
            self.case_hash.as_str(),
            self.mode,
            self.provider_ref.as_str(),
            self.tool_surface.as_slice(),
            self.judge_ref.as_deref(),
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
    /// Build a baseline from a completed suite report (cases without a record,
    /// i.e. errored before producing one, are skipped).
    pub fn from_report(report: &SuiteReport) -> Baseline {
        let entries = report.cases.iter().filter_map(entry_from_case).collect();
        Baseline {
            schema: BASELINE_SCHEMA.to_string(),
            entries,
        }
    }

    /// Serialize as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Parse a baseline from JSON text.
    pub fn from_json(text: &str) -> anyhow::Result<Baseline> {
        Ok(serde_json::from_str(text)?)
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
        judge_ref: rec.judge_ref.clone(),
    })
}

/// The per-case classification of a comparison against a baseline.
#[derive(Debug, Clone, PartialEq)]
pub enum CaseComparison {
    /// Present now, absent from the baseline.
    New,
    /// In the baseline, absent now (warned, never gated).
    Removed,
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

    /// Case ids removed since the baseline (warned).
    pub fn removed(&self) -> Vec<&str> {
        self.per_case
            .iter()
            .filter(|(_, c)| matches!(c, CaseComparison::Removed))
            .map(|(id, _)| id.as_str())
            .collect()
    }
}

fn current_key(rec: &crate::record::RunRecord) -> ComparabilityKey<'_> {
    (
        rec.case_hash.as_str(),
        rec.mode,
        rec.provider_ref.as_str(),
        rec.tool_surface.as_slice(),
        rec.judge_ref.as_deref(),
    )
}

/// The distinct categories of the current case's failing grades.
fn flipped_categories(case: &CaseReport) -> Vec<GradeCategory> {
    let mut out: Vec<GradeCategory> = Vec::new();
    for g in case
        .grades
        .iter()
        .filter(|grade| !grade.passed && !grade.diagnostic)
    {
        if !out.contains(&g.category) {
            out.push(g.category);
        }
    }
    out
}

/// Compare a suite report against a baseline, keyed by case id. Pure: the live
/// flakiness retry is applied separately by the caller.
pub fn compare(current: &SuiteReport, baseline: &Baseline) -> BaselineComparison {
    let base_map: BTreeMap<&str, &BaselineEntry> = baseline
        .entries
        .iter()
        .map(|e| (e.case_id.as_str(), e))
        .collect();
    let cur_map: BTreeMap<&str, &CaseReport> = current
        .cases
        .iter()
        .filter_map(|c| c.record.as_ref().map(|r| (r.case_id.as_str(), c)))
        .collect();

    let mut per_case = BTreeMap::new();
    let ids: std::collections::BTreeSet<&str> =
        base_map.keys().chain(cur_map.keys()).copied().collect();

    for id in ids {
        let classification = match (cur_map.get(id), base_map.get(id)) {
            (Some(_), None) => CaseComparison::New,
            (None, Some(_)) => CaseComparison::Removed,
            (Some(case), Some(base)) => {
                // Safe: cur_map only holds cases with a record.
                let rec = case.record.as_ref().expect("cur_map cases have a record");
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
            (None, None) => unreachable!("id came from one of the maps"),
        };
        per_case.insert(id.to_string(), classification);
    }

    BaselineComparison { per_case }
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
            judge_ref: None,
            judge_usage: None,
        }
    }

    fn grade(check: &str, passed: bool, category: GradeCategory) -> GradeResult {
        GradeResult {
            check: check.to_string(),
            passed,
            detail: String::new(),
            category,
            diagnostic: false,
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
        Baseline::from_report(current)
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
        let cmp = compare(&current, &baseline);
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
        let cmp = compare(&current, &baseline);
        match &cmp.per_case["a"] {
            CaseComparison::Regression { categories } => {
                assert_eq!(categories, &vec![GradeCategory::Tool]);
            }
            other => panic!("expected tool regression, got {other:?}"),
        }
        assert_eq!(cmp.confirmed_regressions(), 1);
    }

    #[test]
    fn regression_categories_exclude_diagnostic_failures() {
        let pass = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("response", true, GradeCategory::Response)],
                10,
            )],
        };
        let baseline = baseline_of(&pass);
        let mut diagnostic = grade("judge:quality", false, GradeCategory::Judge);
        diagnostic.diagnostic = true;
        let current = SuiteReport {
            cases: vec![case(
                "a",
                vec![
                    grade("response", false, GradeCategory::Response),
                    diagnostic,
                ],
                10,
            )],
        };
        let comparison = compare(&current, &baseline);
        assert_eq!(
            comparison.per_case["a"],
            CaseComparison::Regression {
                categories: vec![GradeCategory::Response]
            }
        );
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
        let cmp = compare(&current, &baseline);
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
    fn baseline_round_trips_through_json() {
        let report = SuiteReport {
            cases: vec![case(
                "a",
                vec![grade("c", true, GradeCategory::Response)],
                35,
            )],
        };
        let baseline = Baseline::from_report(&report);
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
        let mut cmp = compare(&current, &base_live);
        assert_eq!(cmp.confirmed_regressions(), 1);

        let mut rerun = BTreeMap::new();
        rerun.insert("a".to_string(), true);
        let flaky = downgrade_flaky_regressions(&mut cmp, Mode::Live, &rerun);
        assert_eq!(flaky, vec!["a".to_string()]);
        assert_eq!(cmp.per_case["a"], CaseComparison::FlakyUnconfirmed);
        assert_eq!(cmp.confirmed_regressions(), 0);

        // Replay never retries: the regression stands.
        let mut cmp2 = compare(&current, &base_live);
        let flaky2 = downgrade_flaky_regressions(&mut cmp2, Mode::Replay, &rerun);
        assert!(flaky2.is_empty());
        assert_eq!(cmp2.confirmed_regressions(), 1);
    }

    #[test]
    fn judge_ref_joins_comparability_key() {
        // Same case, different judge_ref => not comparable (a judge swap must not
        // be silently compared).
        let mut base_case = case("a", vec![grade("c", true, GradeCategory::Response)], 10);
        base_case.record.as_mut().unwrap().judge_ref = Some("judge.v1:m".to_string());
        let baseline = baseline_of(&SuiteReport {
            cases: vec![base_case],
        });
        let mut cur = case("a", vec![grade("c", true, GradeCategory::Response)], 10);
        cur.record.as_mut().unwrap().judge_ref = Some("judge.v2:m".to_string());
        let cmp = compare(&SuiteReport { cases: vec![cur] }, &baseline);
        assert_eq!(cmp.per_case["a"], CaseComparison::Unverifiable);
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
        let cmp = compare(&current, &baseline);
        assert_eq!(cmp.per_case["gone"], CaseComparison::Removed);
        assert_eq!(cmp.removed(), vec!["gone"]);
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
