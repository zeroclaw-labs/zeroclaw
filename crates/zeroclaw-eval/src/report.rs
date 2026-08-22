//! Pass/fail aggregation and rendering.

use crate::grader::GradeResult;

/// A case's comparison id: the record's `case_id` when present, else its name.
fn case_id(case: &CaseReport) -> &str {
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
    /// A case passes when it ran without error, produced at least one grade, and
    /// every check passed.
    ///
    /// The non-empty requirement is the terminal fail-closed guard: an empty
    /// grade list satisfies `.all()` vacuously, so a case that asserted nothing
    /// — or that errored before any grade was produced — would otherwise be
    /// reported green by a required CI gate.
    pub fn passed(&self) -> bool {
        self.error.is_none() && !self.grades.is_empty() && self.grades.iter().all(|g| g.passed)
    }

    fn checks_passed(&self) -> usize {
        self.grades.iter().filter(|g| g.passed).count()
    }

    /// Partial-credit score: fraction of checks passed. A case with no checks
    /// scores 0.0 — it asserted nothing, so it earned no credit. Informational;
    /// the gate is pass/fail.
    pub fn score(&self) -> f64 {
        if self.grades.is_empty() {
            0.0
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

    /// Error bar over *repeated* cases only: `repeated-case pass rate p̄ ±t·SEM
    /// (95% CI)`, annotated with how many of the suite's cases that covers.
    ///
    /// The population is deliberately restricted to cases that actually
    /// repeated (effective `k > 1`) and produced statistics. Effective `k = 1`
    /// cases contribute no within-case success proportion, so folding them in
    /// would give a single-shot case the same weight as a 20-run case and
    /// misrepresent the interval's precision. The label therefore names the
    /// restricted estimand rather than presenting it as the suite pass rate —
    /// the suite's own `passed/total` line is reported separately by
    /// [`Self::render_table`].
    ///
    /// Per-case success proportions are collapsed by cluster first (one value
    /// per cluster), then averaged, so correlated resamples do not fake
    /// precision. `None` when no case repeated. Fewer than two independent
    /// units report the observed rate without an interval because SEM is not
    /// estimable.
    pub fn repeat_ci_line(&self) -> Option<String> {
        let items: Vec<(Option<String>, f64)> = self
            .cases
            .iter()
            .filter(|c| c.error.is_none())
            .filter_map(|c| {
                c.repeat
                    .as_ref()
                    .map(|r| (c.cluster.clone(), r.proportion()))
            })
            .collect();
        if items.is_empty() {
            return None;
        }
        // Names the population so the restricted rate can never be read as the
        // suite pass rate printed directly above it.
        let scope = format!("{} of {} cases repeated", items.len(), self.cases.len());
        let values = crate::stats::cluster_means(&items);
        let mean = crate::stats::mean(&values);
        if values.len() < 2 {
            return Some(format!(
                "repeated-case pass rate {:.0}% ({scope}; 95% CI unavailable: insufficient independent units)",
                mean * 100.0
            ));
        }
        // Student-t multiplier on (n-1) df: the normal z=1.96 understates the
        // interval for the few-unit suites repeated runs typically produce.
        let df = values.len() - 1;
        let ci = crate::stats::t95_multiplier(df) * crate::stats::sem(&values);
        Some(format!(
            "repeated-case pass rate {:.0}% +/-{:.0}% (95% CI, {scope})",
            mean * 100.0,
            ci * 100.0
        ))
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
                // A truncated repeat set reports an error *and* carries partial
                // statistics; fall through so that evidence is still shown.
                if case.repeat.is_none() {
                    continue;
                }
            } else {
                s.push_str(&format!(
                    "  {icon} {} ({})  {}/{} checks\n",
                    case.name,
                    case.source,
                    case.checks_passed(),
                    case.grades.len()
                ));
            }
            // Per-case repeat diagnostics belong in the default report, not only
            // in JSON: passes/k plus the consistency verdict are this feature's
            // primary output.
            if let Some(r) = &case.repeat {
                s.push_str(&format!("      repeat {}/{}", r.passes, r.k));
                if r.truncated() {
                    s.push_str(&format!(" ({} completed)", r.completed));
                }
                s.push_str(&format!(
                    "  pass@k {}  pass^k {}",
                    if r.pass_at_k() { "yes" } else { "no" },
                    if r.pass_hat_k() { "yes" } else { "no" }
                ));
                if !r.check_flips.is_empty() {
                    let flips: Vec<String> = r
                        .check_flips
                        .iter()
                        .map(|(name, n)| format!("{name}×{n}"))
                        .collect();
                    s.push_str(&format!("  flips: {}", flips.join(", ")));
                }
                s.push('\n');
                if let Some(note) = r.suspect_note() {
                    s.push_str(&format!("      {note}\n"));
                }
            }
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
                }
                if let (Some(r), Some(map)) = (&c.repeat, obj.as_object_mut()) {
                    map.insert(
                        "repeat".into(),
                        serde_json::json!({
                            "k": r.k,
                            "passes": r.passes,
                            "completed": r.completed,
                            "truncated": r.truncated(),
                            "error": r.error,
                            "attempts": r.attempts,
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
            "repeat_ci": self.repeat_ci_line(),
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

    fn repeated_case(name: &str, passes: u32, k: u32, cluster: Option<&str>) -> CaseReport {
        let runs: Vec<crate::stats::RunSample> = (0..k)
            .map(|index| crate::stats::RunSample {
                passed: index < passes,
                input_tokens: index as u64 + 1,
                output_tokens: 1,
                duration_ms: 10 + index as u64,
                llm_calls: 1,
                checks: vec![("response".to_string(), index < passes)],
            })
            .collect();
        CaseReport {
            name: name.to_string(),
            source: "fixture.json".to_string(),
            record: None,
            // The representative outcome's grades, as `run_case_repeated`
            // returns them: it selects the first failing repetition when one
            // exists, otherwise a passing one. A repeated case is never
            // grade-less in production, and `passed()` no longer treats an
            // empty grade list as a pass.
            grades: vec![grade("response", passes >= k, "")],
            error: None,
            repeat: Some(crate::stats::RepeatStats::from_runs(k, &runs)),
            cluster: cluster.map(str::to_string),
        }
    }

    #[test]
    fn repeated_single_case_reports_insufficient_units_without_nan() {
        let suite = SuiteReport {
            cases: vec![repeated_case("only", 1, 2, None)],
        };

        let line = suite.repeat_ci_line().expect("repeated case has a summary");
        assert!(line.contains("pass rate 50%"), "got: {line}");
        assert!(
            line.contains("insufficient independent units"),
            "got: {line}"
        );
        assert!(!line.contains("NaN"), "got: {line}");
        assert!(!suite.render_table().contains("NaN"));
        assert!(!suite.to_json().contains("NaN"));
    }

    #[test]
    fn repeated_single_effective_cluster_reports_insufficient_units() {
        let suite = SuiteReport {
            cases: vec![
                repeated_case("cluster-a", 2, 2, Some("family")),
                repeated_case("cluster-b", 0, 2, Some("family")),
            ],
        };

        let line = suite
            .repeat_ci_line()
            .expect("repeated cases have a summary");
        assert!(line.contains("pass rate 50%"), "got: {line}");
        assert!(
            line.contains("insufficient independent units"),
            "got: {line}"
        );
        assert!(!line.contains("NaN"), "got: {line}");
    }

    /// The exact misleading pairing from review: a suite that prints
    /// `1/2 cases passed` must not also present an unqualified `pass rate 100%`.
    #[test]
    fn mixed_suite_never_labels_the_restricted_rate_as_the_suite_pass_rate() {
        let suite = SuiteReport {
            cases: vec![
                // Repeated and fully passing -> the restricted population.
                repeated_case("repeated-ok", 2, 2, None),
                // Effective k=1: no repeat stats, and it fails the suite.
                case("single-fail", vec![grade("c", false, "")], None),
            ],
        };

        assert_eq!(suite.passed_count(), 1);
        assert_eq!(suite.cases.len(), 2);

        let line = suite.repeat_ci_line().expect("one case repeated");
        // The restricted population is 100%, and that is legitimate — but it
        // must be named, and must not claim to be the suite pass rate.
        assert!(
            line.contains("repeated-case pass rate 100%"),
            "restricted estimand must be named: {line}"
        );
        assert!(
            line.contains("1 of 2 cases repeated"),
            "population must be disclosed: {line}"
        );

        let table = suite.render_table();
        assert!(table.contains("1/2 cases passed"), "got: {table}");
        // Guard the regression directly: no bare "pass rate" label may appear,
        // only the qualified one.
        assert!(
            !table.contains(" pass rate") || table.contains("repeated-case pass rate"),
            "unqualified pass rate leaked into the table: {table}"
        );
    }

    /// Errored and effective-k=1 cases are excluded from the restricted
    /// population by design; the disclosed denominator must still be the whole
    /// suite so the exclusion is visible.
    #[test]
    fn repeat_population_discloses_errored_and_single_run_cases() {
        let mut errored = repeated_case("errored", 0, 2, None);
        errored.error = Some("provider exploded".to_string());
        let suite = SuiteReport {
            cases: vec![
                repeated_case("rep-a", 2, 2, None),
                repeated_case("rep-b", 1, 2, None),
                case("single", vec![grade("c", true, "")], None),
                errored,
            ],
        };

        let line = suite.repeat_ci_line().expect("repeated cases present");
        assert!(
            line.contains("2 of 4 cases repeated"),
            "denominator must be the whole suite: {line}"
        );
    }

    /// The per-case repeat verdict is the feature's primary diagnostic and must
    /// be in the default table, not only in JSON.
    #[test]
    fn default_table_exposes_per_case_repeat_verdict() {
        let suite = SuiteReport {
            cases: vec![repeated_case("flaky-case", 3, 5, None)],
        };

        let table = suite.render_table();
        assert!(
            table.contains("repeat 3/5"),
            "passes/k must be in the table: {table}"
        );
        assert!(
            table.contains("pass@k yes"),
            "pass@k verdict must be in the table: {table}"
        );
        assert!(
            table.contains("pass^k no"),
            "pass^k verdict must be in the table: {table}"
        );
    }

    /// A truncated repeat set must fail and must still show its partial
    /// evidence: the completed repetitions were paid for.
    #[test]
    fn truncated_repeat_set_fails_and_retains_partial_evidence() {
        let mut c = repeated_case("truncated", 2, 5, None);
        let completed = vec![
            crate::stats::RunSample {
                passed: true,
                input_tokens: 2,
                output_tokens: 1,
                duration_ms: 10,
                llm_calls: 1,
                checks: vec![("response".to_string(), true)],
            },
            crate::stats::RunSample {
                passed: true,
                input_tokens: 3,
                output_tokens: 1,
                duration_ms: 11,
                llm_calls: 1,
                checks: vec![("response".to_string(), true)],
            },
        ];
        c.repeat = Some(crate::stats::RepeatStats::from_partial_runs(
            5,
            &completed,
            "provider timeout".to_string(),
        ));
        // Mirrors what the runner records for a truncated set.
        c.error = Some(
            "repeat 2/5 runs completed (pass^k not established): provider timeout".to_string(),
        );

        assert!(
            !c.passed(),
            "a truncated set must not pass: only 2 of 5 runs completed"
        );
        let r = c.repeat.as_ref().unwrap();
        assert!(r.truncated());
        assert!(
            !r.pass_hat_k(),
            "pass^k must stay fail-closed on truncation"
        );

        let suite = SuiteReport { cases: vec![c] };
        let table = suite.render_table();
        assert!(
            table.contains("repeat 2/5") || table.contains("(2 completed)"),
            "partial evidence must survive into the table: {table}"
        );
        assert!(
            table.contains("provider timeout"),
            "the truncating error must be reported: {table}"
        );
        assert!(suite.to_json().contains("\"truncated\": true"));
        let json: serde_json::Value = serde_json::from_str(&suite.to_json()).unwrap();
        let attempts = json["cases"][0]["repeat"]["attempts"]
            .as_array()
            .expect("repeat attempts are serialized");
        assert_eq!(attempts.len(), 3);
        assert_eq!(attempts[0]["attempt"].as_u64(), Some(1));
        assert_eq!(attempts[2]["outcome"].as_str(), Some("error"));
        assert_eq!(attempts[2]["error"].as_str(), Some("provider timeout"));
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
    }

    /// Fail closed at the terminal layer: an empty grade list satisfies
    /// `.all()` vacuously, so a case that asserted nothing - or that errored
    /// before producing a grade - would be reported green by the required
    /// regression gate. A green case must mean at least one assertion ran.
    #[test]
    fn empty_grade_list_is_not_a_pass() {
        let report = case("assertion-free", vec![], None);
        assert!(report.error.is_none());
        assert!(report.grades.is_empty());
        assert!(
            !report.passed(),
            "a case with no grades must not pass vacuously"
        );
        assert_eq!(
            report.score(),
            0.0,
            "a case that asserted nothing earns no partial credit"
        );

        // And it gates: a grade-less case fails the suite.
        let suite = SuiteReport {
            cases: vec![report],
        };
        assert!(!suite.all_passed());
        assert_eq!(
            suite.exit_code(crate::baseline::SuiteKind::Regression, None),
            1
        );
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
}
