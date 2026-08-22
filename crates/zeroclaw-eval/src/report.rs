//! Pass/fail aggregation and rendering.

use crate::grader::GradeResult;

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
}

impl CaseReport {
    /// A case passes when it ran without error and every check passed.
    pub fn passed(&self) -> bool {
        self.error.is_none() && self.grades.iter().all(|g| g.passed)
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

/// Aggregated results for a whole suite.
#[derive(Debug)]
pub struct SuiteReport {
    pub cases: Vec<CaseReport>,
}

/// Capability-suite presentation statistics (see
/// [`SuiteReport::capability_stats`]).
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityStats {
    /// Current pass rate as a percentage (0.0 when the suite is empty).
    pub pass_rate: f64,
    /// The baseline's pass rate as a percentage, when a baseline was given.
    pub baseline_rate: Option<f64>,
    /// Whether the suite is saturated (>= 95% pass rate).
    pub saturated: bool,
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
    /// - Regression suites, with a baseline: the comparison is the single
    ///   authority — 1 iff
    ///   [`BaselineComparison::gates`](crate::baseline::BaselineComparison::gates),
    ///   i.e. at least one confirmed per-case Pass->Fail regression or a
    ///   current run error
    ///   (classified `CurrentError`; an errored case has no trustworthy
    ///   comparison). Failures classified `New`, `Unchanged`, `Unverifiable`,
    ///   or `FlakyUnconfirmed` are reported but never gate; a case that failed
    ///   in both runs is not a flip.
    /// - Capability suites: always 0 unless a case ERRORED (a run error, not a
    ///   check failure), which still exits 1.
    ///
    /// Kept as a pure function so the CLI gate is testable at its real boundary.
    pub fn exit_code(
        &self,
        kind: crate::baseline::SuiteKind,
        comparison: Option<&crate::baseline::BaselineComparison>,
    ) -> i32 {
        use crate::baseline::SuiteKind;
        match kind {
            SuiteKind::Regression => match comparison {
                None => i32::from(!self.all_passed()),
                // One authoritative policy: the per-case classification decides
                // the gate (confirmed regressions + current run errors).
                Some(cmp) => i32::from(cmp.gates()),
            },
            SuiteKind::Capability => {
                // Never gate on failing checks; only a run error fails a capability run.
                i32::from(self.cases.iter().any(|c| c.error.is_some()))
            }
        }
    }

    /// Capability-suite statistics for presentation: the current pass rate, the
    /// baseline's pass rate when given, and whether the suite is saturated
    /// (>= 95% pass rate, a candidate for graduating to regression/).
    /// Rendering is the caller's concern (the CLI localizes it).
    pub fn capability_stats(
        &self,
        baseline: Option<&crate::baseline::Baseline>,
    ) -> CapabilityStats {
        let total = self.cases.len();
        let pass_rate = if total == 0 {
            0.0
        } else {
            self.passed_count() as f64 / total as f64 * 100.0
        };
        let baseline_rate = baseline.map(|base| {
            let bt = base.entries.len();
            let bp = base
                .entries
                .iter()
                .filter(|e| e.verdict == crate::baseline::Verdict::Pass)
                .count();
            if bt == 0 {
                0.0
            } else {
                bp as f64 / bt as f64 * 100.0
            }
        });
        CapabilityStats {
            pass_rate,
            baseline_rate,
            saturated: pass_rate >= 95.0,
        }
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
        s
    }

    /// Render the report as pretty JSON for machine consumption / CI artifacts.
    ///
    /// When a baseline comparison was performed, it MUST be passed here along
    /// with the resolved suite kind: the artifact then carries a top-level
    /// `baseline` section (per-case classifications, gate summary) and the
    /// `exit_code` the process will exit with, so CI never receives a failing
    /// artifact that omits why the gate failed.
    pub fn to_json(
        &self,
        kind: crate::baseline::SuiteKind,
        comparison: Option<&crate::baseline::BaselineComparison>,
    ) -> String {
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
                }
                obj
            })
            .collect();

        let mut value = serde_json::json!({
            "passed": self.passed_count(),
            "failed": self.failed_count(),
            "total": self.cases.len(),
            "all_passed": self.all_passed(),
            "suite_kind": kind.as_str(),
            "exit_code": self.exit_code(kind, comparison),
            "cases": cases,
        });
        if let (Some(cmp), Some(map)) = (comparison, value.as_object_mut()) {
            map.insert("baseline".into(), cmp.to_json_value());
        }
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
        }
    }

    fn case(name: &str, grades: Vec<GradeResult>, error: Option<&str>) -> CaseReport {
        CaseReport {
            name: name.to_string(),
            source: "fixture.json".to_string(),
            record: None,
            grades,
            error: error.map(str::to_string),
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
    fn exit_regression_new_failing_case_does_not_gate() {
        // A newly added failing case is classified `New`: reported, not a
        // confirmed Pass->Fail flip, so it does not gate the baseline run.
        let s = SuiteReport {
            cases: vec![case("fresh", vec![grade("c", false, "")], None)],
        };
        let cmp = cmp_of(vec![("fresh", CaseComparison::New)]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 0);
    }

    #[test]
    fn exit_regression_failed_in_both_runs_does_not_gate() {
        // A case that failed in the baseline and still fails is `Unchanged`:
        // no verdict flip, so no gate.
        let s = SuiteReport {
            cases: vec![case("still-bad", vec![grade("c", false, "")], None)],
        };
        let cmp = cmp_of(vec![(
            "still-bad",
            CaseComparison::Unchanged {
                token_delta_pct: None,
            },
        )]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 0);
    }

    #[test]
    fn exit_regression_run_error_gates_even_with_baseline() {
        // An errored case has no trustworthy comparison; `compare` classifies
        // it CurrentError and it must gate.
        let s = SuiteReport {
            cases: vec![case("err", vec![], Some("boom"))],
        };
        let cmp = cmp_of(vec![("err", CaseComparison::CurrentError)]);
        assert_eq!(s.exit_code(SuiteKind::Regression, Some(&cmp)), 1);
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
    fn capability_stats_report_rate_trend_and_saturation() {
        let s = SuiteReport {
            cases: vec![case("ok", vec![grade("c", true, "")], None)],
        };
        let stats = s.capability_stats(None);
        assert!((stats.pass_rate - 100.0).abs() < f64::EPSILON);
        assert!(stats.saturated);
        assert_eq!(stats.baseline_rate, None);
        // An empty suite is 0% and not saturated.
        let empty = SuiteReport { cases: vec![] };
        let stats = empty.capability_stats(None);
        assert!(stats.pass_rate.abs() < f64::EPSILON);
        assert!(!stats.saturated);
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
        let json: serde_json::Value =
            serde_json::from_str(&suite.to_json(SuiteKind::Regression, None)).unwrap();
        assert_eq!(json["passed"].as_u64(), Some(1));
        assert_eq!(json["failed"].as_u64(), Some(1));
        assert_eq!(json["total"].as_u64(), Some(2));
        assert_eq!(json["all_passed"].as_bool(), Some(false));
        assert_eq!(json["suite_kind"].as_str(), Some("regression"));
        // No baseline: exit code mirrors all_passed, and no baseline section.
        assert_eq!(json["exit_code"].as_i64(), Some(1));
        assert!(json.get("baseline").is_none());
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
    fn to_json_with_baseline_carries_gate_outcome() {
        // A failing case classified New: reported in the artifact, gate open.
        let suite = SuiteReport {
            cases: vec![case("fresh", vec![grade("c", false, "")], None)],
        };
        let cmp = cmp_of(vec![("fresh", CaseComparison::New)]);
        let json: serde_json::Value =
            serde_json::from_str(&suite.to_json(SuiteKind::Regression, Some(&cmp))).unwrap();
        assert_eq!(json["exit_code"].as_i64(), Some(0));
        assert_eq!(json["baseline"]["gates"].as_bool(), Some(false));
        assert_eq!(json["baseline"]["confirmed_regressions"].as_u64(), Some(0));
        assert_eq!(
            json["baseline"]["per_case"]["fresh"]["classification"].as_str(),
            Some("new")
        );

        // A confirmed regression: the artifact says why the gate failed.
        let cmp = cmp_of(vec![(
            "fresh",
            CaseComparison::Regression {
                categories: vec![crate::grader::GradeCategory::Tool],
            },
        )]);
        let json: serde_json::Value =
            serde_json::from_str(&suite.to_json(SuiteKind::Regression, Some(&cmp))).unwrap();
        assert_eq!(json["exit_code"].as_i64(), Some(1));
        assert_eq!(json["baseline"]["gates"].as_bool(), Some(true));
        assert_eq!(json["baseline"]["confirmed_regressions"].as_u64(), Some(1));
        assert_eq!(
            json["baseline"]["per_case"]["fresh"]["classification"].as_str(),
            Some("regression")
        );
        assert_eq!(
            json["baseline"]["per_case"]["fresh"]["categories"][0].as_str(),
            Some("tool")
        );

        // A current run error is carried explicitly.
        let err_suite = SuiteReport {
            cases: vec![case("err", vec![], Some("boom"))],
        };
        let cmp = cmp_of(vec![("err", CaseComparison::CurrentError)]);
        let json: serde_json::Value =
            serde_json::from_str(&err_suite.to_json(SuiteKind::Regression, Some(&cmp))).unwrap();
        assert_eq!(json["exit_code"].as_i64(), Some(1));
        assert_eq!(json["baseline"]["current_errors"].as_u64(), Some(1));
        assert_eq!(
            json["baseline"]["per_case"]["err"]["classification"].as_str(),
            Some("current_error")
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
}
