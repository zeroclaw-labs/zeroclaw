//! Pass/fail aggregation and rendering.

use crate::grader::{GradeResult, grades_pass};
use serde::{Deserialize, Serialize};

/// A case's comparison id: the record's `case_id` when present, else its name.
/// The single canonical case-identity derivation (baseline skip-matching and the
/// JUnit writer both rely on it).
pub(crate) fn case_id(case: &CaseReport) -> &str {
    case.record
        .as_ref()
        .map(|r| r.case_id.as_str())
        .unwrap_or(&case.name)
}

/// The result of running a single eval case.
#[derive(Debug)]
pub struct CaseReport {
    /// The trace's `model_name`.
    pub name: String,
    /// The fixture file name the case came from.
    pub source: String,
    /// The run record (receipt + transcript). `None` when the run errored before
    /// producing a record.
    pub record: Option<crate::record::RunRecord>,
    /// Per-check grades.
    pub grades: Vec<GradeResult>,
    /// Set if the run itself errored (e.g. trace exhausted) — counts as a failure.
    pub error: Option<String>,
    /// Repeated-run statistics when the case ran more than once (live `repeat > 1`).
    pub repeat: Option<crate::stats::RepeatStats>,
    /// Optional cluster label from the case, for correlated-family error bars.
    pub cluster: Option<String>,
}

impl CaseReport {
    /// A case passes when it ran without error and every non-diagnostic check
    /// passed. Diagnostic grades (e.g. an uncalibrated judge dimension) never gate.
    pub fn passed(&self) -> bool {
        self.error.is_none() && grades_pass(&self.grades)
    }

    fn checks_passed(&self) -> usize {
        self.grades.iter().filter(|g| g.passed).count()
    }

    /// Partial-credit score: fraction of checks passed. A case with no checks
    /// scores 1.0 (it passes vacuously). Informational; the gate is pass/fail.
    pub fn score(&self) -> f64 {
        if self.grades.is_empty() {
            1.0
        } else {
            self.checks_passed() as f64 / self.grades.len() as f64
        }
    }

    /// Per-category `(passed, total)` tallies, keyed by the category's snake_case
    /// label. Only categories with at least one grade appear.
    fn category_totals(&self) -> serde_json::Value {
        use std::collections::BTreeMap;
        let mut totals: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
        for g in &self.grades {
            let entry = totals.entry(g.category.as_str()).or_insert((0, 0));
            entry.1 += 1;
            if g.passed {
                entry.0 += 1;
            }
        }
        let map: serde_json::Map<String, serde_json::Value> = totals
            .into_iter()
            .map(|(cat, (passed, total))| {
                (
                    cat.to_string(),
                    serde_json::json!({ "passed": passed, "total": total }),
                )
            })
            .collect();
        serde_json::Value::Object(map)
    }
}

/// Numeric suite-level repeated-run summary. This is the canonical source for
/// both human rendering and machine-readable JSON.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RepeatCi {
    /// Mean pass proportion after correlated cases are collapsed by cluster.
    pub pass_rate: f64,
    /// Two-sided 95% confidence-interval half-width. Absent when fewer than two
    /// independent units make an interval unidentifiable.
    pub ci_half_width: Option<f64>,
    /// Number of independent units after cluster collapsing.
    pub independent_units: usize,
}

/// Aggregated results for a whole suite.
#[derive(Debug)]
pub struct SuiteReport {
    pub cases: Vec<CaseReport>,
}

impl SuiteReport {
    pub fn passed_count(&self) -> usize {
        self.cases.iter().filter(|c| c.passed()).count()
    }

    pub fn failed_count(&self) -> usize {
        self.cases.len() - self.passed_count()
    }

    pub fn all_passed(&self) -> bool {
        self.cases.iter().all(CaseReport::passed)
    }

    /// Process exit code for a completed run. Gating is strictly per-case:
    /// - Regression suites, no baseline: 0 iff every case passed.
    /// - Regression suites, with a baseline: 0 iff every case passed AND there are
    ///   zero confirmed per-case Pass->Fail regressions.
    /// - Capability suites: always 0 unless a case ERRORED (a run error, not a
    ///   check failure), which still exits 1.
    ///
    /// Kept as a pure function so the CLI gate is testable at its real boundary.
    pub fn exit_code(
        &self,
        kind: crate::baseline::SuiteKind,
        comparison: Option<&crate::baseline::BaselineComparison>,
    ) -> i32 {
        use crate::baseline::{CaseComparison, SuiteKind};
        match kind {
            SuiteKind::Regression => match comparison {
                None => i32::from(!self.all_passed()),
                Some(cmp) => {
                    // A failing case gates unless the comparison excuses it: an
                    // Unverifiable case (hash changed, refresh the baseline) or a
                    // FlakyUnconfirmed live case (regressed but passed on re-run).
                    let gating_failure = self.cases.iter().any(|c| {
                        !c.passed()
                            && !matches!(
                                cmp.per_case.get(case_id(c)),
                                Some(CaseComparison::FlakyUnconfirmed)
                                    | Some(CaseComparison::Unverifiable)
                            )
                    });
                    i32::from(gating_failure)
                }
            },
            SuiteKind::Capability => {
                // Never gate on failing checks; only a run error fails a capability run.
                i32::from(self.cases.iter().any(|c| c.error.is_some()))
            }
        }
    }

    /// A one-line capability summary: current pass rate, the baseline's pass rate
    /// when given, and a saturation warning at or above 95%.
    pub fn capability_summary(&self, baseline: Option<&crate::baseline::Baseline>) -> String {
        let total = self.cases.len();
        let rate = if total == 0 {
            0.0
        } else {
            self.passed_count() as f64 / total as f64 * 100.0
        };
        let mut s = format!("pass rate {rate:.0}%");
        if let Some(base) = baseline {
            let bt = base.entries.len();
            let bp = base
                .entries
                .iter()
                .filter(|e| e.verdict == crate::baseline::Verdict::Pass)
                .count();
            let brate = if bt == 0 {
                0.0
            } else {
                bp as f64 / bt as f64 * 100.0
            };
            s.push_str(&format!(" (was {brate:.0}%)"));
        }
        if rate >= 95.0 {
            s.push_str("\n  saturation warning: >=95% - consider graduating to regression/");
        }
        s
    }

    /// Suite pass-rate summary for repeated (live) runs. When any case repeated,
    /// all cases contribute their success proportion: repeated cases contribute
    /// `passes/k`, while one-shot cases contribute 1 or 0. Correlated cases are
    /// cluster-averaged before the SEM so they do not fake precision.
    pub fn repeat_ci(&self) -> Option<RepeatCi> {
        if !self.cases.iter().any(|case| case.repeat.is_some()) {
            return None;
        }
        let items: Vec<(Option<String>, f64)> = self
            .cases
            .iter()
            .map(|case| {
                let proportion = case.repeat.as_ref().map_or_else(
                    || if case.passed() { 1.0 } else { 0.0 },
                    |repeat| repeat.proportion(),
                );
                (case.cluster.clone(), proportion)
            })
            .collect();
        let values = crate::stats::cluster_means(&items);
        let independent_units = values.len();
        let ci_half_width = (independent_units >= 2).then(|| {
            // Student-t multiplier on (n-1) df: the normal z=1.96 understates the
            // interval for the few-unit suites repeated runs typically produce.
            crate::stats::t95_multiplier(independent_units - 1) * crate::stats::sem(&values)
        });
        Some(RepeatCi {
            pass_rate: crate::stats::mean(&values),
            ci_half_width,
            independent_units,
        })
    }

    /// Render the canonical numeric repeated-run summary for the table output.
    pub fn repeat_ci_line(&self) -> Option<String> {
        let ci = self.repeat_ci()?;
        Some(match ci.ci_half_width {
            Some(half_width) => format!(
                "pass rate {:.0}% +/-{:.0}% (95% CI)",
                ci.pass_rate * 100.0,
                half_width * 100.0
            ),
            None => format!(
                "pass rate {:.0}% (95% CI unavailable: fewer than 2 independent units)",
                ci.pass_rate * 100.0
            ),
        })
    }

    /// Render a human-readable table. Failing checks are listed beneath their case.
    pub fn render_table(&self) -> String {
        let mut s = String::new();
        s.push('\n');
        for case in &self.cases {
            let icon = if case.passed() { "✓" } else { "✗" };
            if let Some(err) = &case.error {
                s.push_str(&format!(
                    "  {icon} {} ({})  —  run error: {err}\n",
                    case.name, case.source
                ));
                continue;
            }
            s.push_str(&format!(
                "  {icon} {} ({})  {}/{} checks\n",
                case.name,
                case.source,
                case.checks_passed(),
                case.grades.len()
            ));
            for g in case.grades.iter().filter(|g| !g.passed) {
                s.push_str(&format!("      ✗ {}: {}\n", g.check, g.detail));
            }
        }
        s.push('\n');
        s.push_str(&format!(
            "  {}/{} cases passed",
            self.passed_count(),
            self.cases.len()
        ));
        if self.all_passed() {
            s.push_str("  \u{2713}\n");
        } else {
            s.push_str(&format!("  ({} failed)\n", self.failed_count()));
        }
        if let Some(ci) = self.repeat_ci_line() {
            s.push_str(&format!("  {ci}\n"));
        }
        s
    }

    /// Render the report as pretty JSON for machine consumption / CI artifacts.
    pub fn to_json(&self) -> String {
        let cases: Vec<serde_json::Value> = self
            .cases
            .iter()
            .map(|c| {
                let mut obj = serde_json::json!({
                    "name": c.name,
                    "source": c.source,
                    "passed": c.passed(),
                    "score": c.score(),
                    "category_totals": c.category_totals(),
                    "error": c.error,
                    "grades": c.grades,
                });
                if let (Some(rec), Some(map)) = (&c.record, obj.as_object_mut()) {
                    map.insert("schema".into(), rec.schema.clone().into());
                    map.insert(
                        "mode".into(),
                        serde_json::to_value(rec.mode).unwrap_or_default(),
                    );
                    map.insert("case_id".into(), rec.case_id.clone().into());
                    map.insert("case_hash".into(), rec.case_hash.clone().into());
                    map.insert("provider_ref".into(), rec.provider_ref.clone().into());
                    map.insert(
                        "tool_surface".into(),
                        serde_json::to_value(&rec.tool_surface).unwrap_or_default(),
                    );
                    map.insert(
                        "sandbox".into(),
                        serde_json::to_value(&rec.sandbox).unwrap_or_default(),
                    );
                    map.insert(
                        "total_tokens".into(),
                        (rec.input_tokens + rec.output_tokens).into(),
                    );
                    if let Some(judge_ref) = &rec.judge_ref {
                        map.insert("judge_ref".into(), judge_ref.clone().into());
                    }
                }
                if let (Some(r), Some(map)) = (&c.repeat, obj.as_object_mut()) {
                    map.insert(
                        "repeat".into(),
                        serde_json::json!({
                            "k": r.k,
                            "passes": r.passes,
                            "pass_at_k": r.pass_at_k(),
                            "pass_hat_k": r.pass_hat_k(),
                            "token_mean": r.token_mean,
                            "token_stddev": r.token_stddev,
                            "duration_mean_ms": r.duration_mean,
                            "duration_stddev_ms": r.duration_stddev,
                            "check_flips": r.check_flips,
                            "suspect": r.suspect_note(),
                        }),
                    );
                }
                obj
            })
            .collect();

        let value = serde_json::json!({
            "passed": self.passed_count(),
            "failed": self.failed_count(),
            "total": self.cases.len(),
            "all_passed": self.all_passed(),
            "repeat_ci": self.repeat_ci(),
            "cases": cases,
        });
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(check: &str, passed: bool, detail: &str) -> GradeResult {
        GradeResult {
            check: check.to_string(),
            passed,
            detail: detail.to_string(),
            category: crate::grader::GradeCategory::Response,
            diagnostic: false,
        }
    }

    fn case(name: &str, grades: Vec<GradeResult>, error: Option<&str>) -> CaseReport {
        CaseReport {
            name: name.to_string(),
            source: "fixture.json".to_string(),
            record: None,
            grades,
            error: error.map(str::to_string),
            repeat: None,
            cluster: None,
        }
    }

    #[test]
    fn case_passes_only_when_no_error_and_all_checks_pass() {
        assert!(
            case(
                "a",
                vec![grade("c1", true, ""), grade("c2", true, "")],
                None
            )
            .passed()
        );
        // One failing check fails the case.
        assert!(
            !case(
                "a",
                vec![grade("c1", true, ""), grade("c2", false, "")],
                None
            )
            .passed()
        );
        // A run error fails the case even when every check passed.
        assert!(!case("a", vec![grade("c1", true, "")], Some("trace exhausted")).passed());
        // No checks and no error passes vacuously.
        assert!(case("a", vec![], None).passed());
    }

    #[test]
    fn suite_counts_reflect_per_case_pass_fail() {
        let suite = SuiteReport {
            cases: vec![
                case("ok", vec![grade("c", true, "")], None),
                case("bad", vec![grade("c", false, "")], None),
                case("err", vec![], Some("boom")),
            ],
        };
        assert_eq!(suite.passed_count(), 1);
        assert_eq!(suite.failed_count(), 2);
        assert!(!suite.all_passed());
    }

    use crate::baseline::{BaselineComparison, CaseComparison, SuiteKind};

    fn cmp_of(pairs: Vec<(&str, CaseComparison)>) -> BaselineComparison {
        BaselineComparison {
            per_case: pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }

    #[test]
    fn exit_regression_no_baseline_all_pass_is_zero() {
        let s = SuiteReport {
            cases: vec![case("ok", vec![grade("c", true, "")], None)],
        };
        assert_eq!(s.exit_code(SuiteKind::Regression, None), 0);
    }

    #[test]
    fn exit_regression_no_baseline_any_fail_is_one() {
        let s = SuiteReport {
            cases: vec![case("bad", vec![grade("c", false, "")], None)],
        };
        assert_eq!(s.exit_code(SuiteKind::Regression, None), 1);
    }

    #[test]
    fn exit_regression_with_baseline_clean_is_zero() {
        let s = SuiteReport {
            cases: vec![case("ok", vec![grade("c", true, "")], None)],
        };
        let cmp = cmp_of(vec![(
            "ok",
            CaseComparison::Unchanged {
                token_delta_pct: None,
            },
        )]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 0);
    }

    #[test]
    fn exit_regression_with_baseline_confirmed_regression_is_one() {
        let s = SuiteReport {
            cases: vec![case("bad", vec![grade("c", false, "")], None)],
        };
        let cmp = cmp_of(vec![(
            "bad",
            CaseComparison::Regression {
                categories: vec![crate::grader::GradeCategory::Response],
            },
        )]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 1);
    }

    #[test]
    fn improvement_never_fails_exit() {
        let s = SuiteReport {
            cases: vec![case("ok", vec![grade("c", true, "")], None)],
        };
        let cmp = cmp_of(vec![("ok", CaseComparison::Improvement)]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 0);
    }

    #[test]
    fn exit_regression_flaky_failure_is_excused() {
        // A failing live case downgraded to flaky must not gate.
        let s = SuiteReport {
            cases: vec![case("live", vec![grade("c", false, "")], None)],
        };
        let cmp = cmp_of(vec![("live", CaseComparison::FlakyUnconfirmed)]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 0);
    }

    #[test]
    fn exit_regression_unverifiable_failure_is_excused() {
        // A failing case whose comparability key changed must not gate.
        let s = SuiteReport {
            cases: vec![case("changed", vec![grade("c", false, "")], None)],
        };
        let cmp = cmp_of(vec![("changed", CaseComparison::Unverifiable)]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 0);
    }

    #[test]
    fn exit_regression_mixed_excused_and_regression_gates() {
        // An excused flaky failure alongside a real regression still gates.
        let s = SuiteReport {
            cases: vec![
                case("flaky", vec![grade("c", false, "")], None),
                case("bad", vec![grade("c", false, "")], None),
            ],
        };
        let cmp = cmp_of(vec![
            ("flaky", CaseComparison::FlakyUnconfirmed),
            (
                "bad",
                CaseComparison::Regression {
                    categories: vec![crate::grader::GradeCategory::Response],
                },
            ),
        ]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 1);
    }

    #[test]
    fn exit_capability_all_pass_is_zero() {
        let s = SuiteReport {
            cases: vec![case("ok", vec![grade("c", true, "")], None)],
        };
        assert_eq!(s.exit_code(SuiteKind::Capability, None), 0);
    }

    #[test]
    fn exit_capability_check_failure_is_zero() {
        // A failing check does not gate a capability suite.
        let s = SuiteReport {
            cases: vec![case("low", vec![grade("c", false, "")], None)],
        };
        assert_eq!(s.exit_code(SuiteKind::Capability, None), 0);
    }

    #[test]
    fn exit_capability_run_error_is_one() {
        // A run error still gates a capability suite.
        let s = SuiteReport {
            cases: vec![case("err", vec![], Some("boom"))],
        };
        assert_eq!(s.exit_code(SuiteKind::Capability, None), 1);
    }

    #[test]
    fn capability_summary_reports_rate_trend_and_saturation() {
        let s = SuiteReport {
            cases: vec![case("ok", vec![grade("c", true, "")], None)],
        };
        let sum = s.capability_summary(None);
        assert!(sum.contains("pass rate 100%"), "got: {sum}");
        assert!(sum.contains("saturation warning"), "got: {sum}");
    }

    #[test]
    fn empty_suite_passes_vacuously() {
        let suite = SuiteReport { cases: vec![] };
        assert_eq!(suite.passed_count(), 0);
        assert_eq!(suite.failed_count(), 0);
        assert!(suite.all_passed());
    }

    #[test]
    fn render_table_marks_failures_and_lists_failing_checks() {
        let suite = SuiteReport {
            cases: vec![
                case("ok", vec![grade("c", true, "")], None),
                case(
                    "bad",
                    vec![grade("response_contains", false, "not found")],
                    None,
                ),
            ],
        };
        let table = suite.render_table();
        assert!(table.contains("✓ ok"));
        assert!(table.contains("✗ bad"));
        assert!(table.contains("response_contains: not found"));
        assert!(table.contains("1/2 cases passed"));
        assert!(table.contains("(1 failed)"));
    }

    #[test]
    fn render_table_reports_run_errors() {
        let suite = SuiteReport {
            cases: vec![case("err", vec![], Some("trace exhausted"))],
        };
        let table = suite.render_table();
        assert!(table.contains("run error: trace exhausted"));
    }

    #[test]
    fn to_json_serializes_aggregate_and_cases() {
        let suite = SuiteReport {
            cases: vec![
                case("ok", vec![grade("c", true, "")], None),
                case("bad", vec![grade("c", false, "")], None),
            ],
        };
        let json: serde_json::Value = serde_json::from_str(&suite.to_json()).unwrap();
        assert_eq!(json["passed"].as_u64(), Some(1));
        assert_eq!(json["failed"].as_u64(), Some(1));
        assert_eq!(json["total"].as_u64(), Some(2));
        assert_eq!(json["all_passed"].as_bool(), Some(false));
        assert_eq!(json["cases"].as_array().unwrap().len(), 2);
        assert_eq!(json["cases"][0]["name"].as_str(), Some("ok"));
        assert_eq!(json["cases"][0]["passed"].as_bool(), Some(true));
        // Each grade now carries its category (snake_case) in the JSON report.
        assert_eq!(
            json["cases"][0]["grades"][0]["category"].as_str(),
            Some("response")
        );
    }

    #[test]
    fn category_totals_aggregate_correctly() {
        use crate::grader::GradeCategory;
        let grade_cat = |passed: bool, category: GradeCategory| GradeResult {
            check: "c".to_string(),
            passed,
            detail: String::new(),
            category,
            diagnostic: false,
        };
        let report = CaseReport {
            name: "mixed".to_string(),
            source: "f.json".to_string(),
            record: None,
            grades: vec![
                grade_cat(true, GradeCategory::Response),
                grade_cat(false, GradeCategory::Response),
                grade_cat(true, GradeCategory::Tool),
                grade_cat(true, GradeCategory::SideEffect),
            ],
            error: None,
            repeat: None,
            cluster: None,
        };
        // score = 3/4 passed.
        assert!((report.score() - 0.75).abs() < f64::EPSILON);
        let totals = report.category_totals();
        assert_eq!(totals["response"]["passed"].as_u64(), Some(1));
        assert_eq!(totals["response"]["total"].as_u64(), Some(2));
        assert_eq!(totals["tool"]["passed"].as_u64(), Some(1));
        assert_eq!(totals["tool"]["total"].as_u64(), Some(1));
        assert_eq!(totals["side_effect"]["total"].as_u64(), Some(1));
        // Categories with no grades do not appear.
        assert!(totals.get("budget").is_none());
    }

    fn repeat_stats(k: u32, passes: u32) -> crate::stats::RepeatStats {
        crate::stats::RepeatStats {
            k,
            passes,
            token_mean: 0.0,
            token_stddev: 0.0,
            duration_mean: 0.0,
            duration_stddev: 0.0,
            check_flips: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn repeat_ci_single_unit_is_explicitly_unavailable() {
        let mut repeated = case("only", vec![grade("c", true, "")], None);
        repeated.repeat = Some(repeat_stats(2, 2));
        let suite = SuiteReport {
            cases: vec![repeated],
        };
        let ci = suite.repeat_ci().expect("repeat summary");
        assert_eq!(ci.pass_rate, 1.0);
        assert_eq!(ci.independent_units, 1);
        assert_eq!(ci.ci_half_width, None);
        let line = suite.repeat_ci_line().expect("repeat summary line");
        assert!(line.contains("CI unavailable"));
        assert!(!line.contains("NaN"));
    }

    #[test]
    fn repeat_ci_includes_one_shot_cases_in_mixed_suite() {
        let mut repeated = case("repeat-pass", vec![grade("c", true, "")], None);
        repeated.repeat = Some(repeat_stats(2, 2));
        let mut cases = vec![repeated];
        for index in 0..9 {
            cases.push(case(
                &format!("one-shot-fail-{index}"),
                vec![grade("c", false, "")],
                None,
            ));
        }
        let ci = SuiteReport { cases }.repeat_ci().expect("repeat summary");
        assert!((ci.pass_rate - 0.1).abs() < f64::EPSILON);
        assert_eq!(ci.independent_units, 10);
        assert!(ci.ci_half_width.is_some());
    }

    #[test]
    fn repeat_ci_single_cluster_does_not_produce_nan() {
        let mut first = case("first", vec![grade("c", true, "")], None);
        first.repeat = Some(repeat_stats(2, 2));
        first.cluster = Some("family".to_string());
        let mut second = case("second", vec![grade("c", false, "")], None);
        second.cluster = Some("family".to_string());
        let suite = SuiteReport {
            cases: vec![first, second],
        };
        let ci = suite.repeat_ci().expect("repeat summary");
        assert_eq!(ci.independent_units, 1);
        assert_eq!(ci.ci_half_width, None);
        assert!((ci.pass_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn repeat_ci_json_is_numeric_not_preformatted_text() {
        let mut first = case("first", vec![grade("c", true, "")], None);
        first.repeat = Some(repeat_stats(2, 2));
        let second = case("second", vec![grade("c", false, "")], None);
        let report = SuiteReport {
            cases: vec![first, second],
        };
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["repeat_ci"]["pass_rate"].as_f64(), Some(0.5));
        assert_eq!(json["repeat_ci"]["independent_units"].as_u64(), Some(2));
        assert!(json["repeat_ci"]["ci_half_width"].is_number());
    }
}
