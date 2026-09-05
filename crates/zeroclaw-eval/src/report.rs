//! Pass/fail aggregation and rendering.

use crate::grader::GradeResult;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

/// A case's comparison id: the record's `case_id` when present, else its name.
/// The single canonical case-identity derivation (baseline skip-matching and the
/// JUnit writer both rely on it).
pub(crate) fn case_id(case: &CaseReport) -> &str {
    case.record
        .as_ref()
        .map(|record| record.provenance.case_id.as_str())
        .unwrap_or(&case.name)
}

/// The result of running a single eval case.
#[derive(Debug)]
pub struct CaseReport {
    /// The trace's `model_name`.
    pub name: String,
    /// The fixture file name the case came from.
    pub source: String,
    /// The run record (receipt + transcript). Normal execution errors preserve
    /// provenance with no completion; `None` is reserved for callers that could
    /// not construct even the pre-run provenance.
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
    /// A case passes when it ran without error, produced at least one grade,
    /// and every non-diagnostic grade passed.
    ///
    /// Fixture admission rejects assertion-free cases, but this aggregation
    /// boundary also fails closed so a caller cannot manufacture a green
    /// report from an empty grade vector.
    pub fn passed(&self) -> bool {
        self.error.is_none() && crate::grader::gating_grades_pass(&self.grades)
    }

    fn checks_passed(&self) -> usize {
        self.grades.iter().filter(|g| g.passed).count()
    }

    /// Partial-credit score: fraction of checks passed, or `None` when the case
    /// produced no checks (it errored before grading, or asserted nothing).
    /// A vacuous case is not a perfect one, so it has no score rather than 1.0.
    /// Informational; the gate is pass/fail.
    pub fn score(&self) -> Option<f64> {
        if self.grades.is_empty() {
            None
        } else {
            Some(self.checks_passed() as f64 / self.grades.len() as f64)
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

/// Machine-readable confidence summary derived from completed repeated cases.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RepeatCi {
    /// Mean success proportion across independent units, in `[0, 1]`.
    pub pass_rate: f64,
    /// Bounded lower 95% confidence limit, absent when fewer than two
    /// independent units are available.
    pub lower: Option<f64>,
    /// Bounded upper 95% confidence limit, absent when fewer than two
    /// independent units are available.
    pub upper: Option<f64>,
    pub repeated_cases: usize,
    pub total_cases: usize,
    pub independent_units: usize,
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

    /// Machine-readable error bar over completed repeated cases only.
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
    pub fn repeat_ci(&self) -> Option<RepeatCi> {
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
        let values = crate::stats::cluster_means(&items);
        let mean = crate::stats::mean(&values);
        let bounds = (values.len() >= 2).then(|| {
            let df = values.len() - 1;
            let margin = crate::stats::t95_multiplier(df) * crate::stats::sem(&values);
            (
                (mean - margin).clamp(0.0, 1.0),
                (mean + margin).clamp(0.0, 1.0),
            )
        });
        Some(RepeatCi {
            pass_rate: mean,
            lower: bounds.map(|(lower, _)| lower),
            upper: bounds.map(|(_, upper)| upper),
            repeated_cases: items.len(),
            total_cases: self.cases.len(),
            independent_units: values.len(),
        })
    }

    /// Localized human-readable rendering of [`Self::repeat_ci`].
    pub fn repeat_ci_line(&self) -> Option<String> {
        let ci = self.repeat_ci()?;
        // Names the population so the restricted rate can never be read as the
        // suite pass rate printed directly above it.
        let repeated = ci.repeated_cases.to_string();
        let total = ci.total_cases.to_string();
        let scope = get_required_cli_string_with_args(
            "cli-eval-repeat-scope",
            &[("repeated", repeated.as_str()), ("total", total.as_str())],
        );
        let rate = format!("{:.0}", ci.pass_rate * 100.0);
        let (Some(lower), Some(upper)) = (ci.lower, ci.upper) else {
            return Some(get_required_cli_string_with_args(
                "cli-eval-repeat-ci-unavailable",
                &[("rate", rate.as_str()), ("scope", scope.as_str())],
            ));
        };
        let lower = format!("{:.0}", lower * 100.0);
        let upper = format!("{:.0}", upper * 100.0);
        Some(get_required_cli_string_with_args(
            "cli-eval-repeat-ci",
            &[
                ("rate", rate.as_str()),
                ("lower", lower.as_str()),
                ("upper", upper.as_str()),
                ("scope", scope.as_str()),
            ],
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
                let passes = r.passes.to_string();
                let total = r.k.to_string();
                s.push_str("      ");
                s.push_str(&get_required_cli_string_with_args(
                    "cli-eval-repeat-case",
                    &[("passes", passes.as_str()), ("total", total.as_str())],
                ));
                if r.truncated() {
                    let completed = r.completed.to_string();
                    s.push(' ');
                    s.push_str(&get_required_cli_string_with_args(
                        "cli-eval-repeat-completed",
                        &[("completed", completed.as_str())],
                    ));
                }
                let pass_at = get_required_cli_string(if r.pass_at_k() {
                    "cli-status-word-yes"
                } else {
                    "cli-status-word-no"
                });
                let pass_hat = get_required_cli_string(if r.pass_hat_k() {
                    "cli-status-word-yes"
                } else {
                    "cli-status-word-no"
                });
                s.push_str("  ");
                s.push_str(&get_required_cli_string_with_args(
                    "cli-eval-repeat-verdicts",
                    &[
                        ("pass_at", pass_at.as_str()),
                        ("pass_hat", pass_hat.as_str()),
                    ],
                ));
                if !r.check_flips.is_empty() {
                    let flips: Vec<String> = r
                        .check_flips
                        .iter()
                        .map(|(name, n)| format!("{name}×{n}"))
                        .collect();
                    let flips = flips.join(", ");
                    s.push_str("  ");
                    s.push_str(&get_required_cli_string_with_args(
                        "cli-eval-repeat-flips",
                        &[("flips", flips.as_str())],
                    ));
                }
                s.push('\n');
                let note_key = if r.passes == 0 && r.k >= 20 {
                    Some("cli-eval-repeat-suspect")
                } else if r.passes == 0 && r.k >= 5 {
                    Some("cli-eval-repeat-low-signal")
                } else {
                    None
                };
                if let Some(key) = note_key {
                    let total = r.k.to_string();
                    let note = get_required_cli_string_with_args(key, &[("total", total.as_str())]);
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
                    // Provenance is emitted unconditionally: it is knowable before
                    // execution, so an errored case still carries a joinable receipt.
                    let p = &rec.provenance;
                    map.insert("schema".into(), p.schema.clone().into());
                    map.insert(
                        "mode".into(),
                        serde_json::to_value(p.mode).unwrap_or_default(),
                    );
                    map.insert("case_id".into(), p.case_id.clone().into());
                    map.insert("case_hash".into(), p.case_hash.clone().into());
                    map.insert("provider_ref".into(), p.provider_ref.clone().into());
                    map.insert(
                        "tool_surface".into(),
                        serde_json::to_value(&p.tool_surface).unwrap_or_default(),
                    );
                    map.insert(
                        "sandbox".into(),
                        serde_json::to_value(&p.sandbox).unwrap_or_default(),
                    );
                    if let Some(judge_ref) = &p.judge_ref {
                        map.insert("judge_ref".into(), judge_ref.clone().into());
                    }
                    // Completion-only fields stay absent for a run that never
                    // finished, rather than being reported as a real zero.
                    if let Some(done) = &rec.completion {
                        map.insert(
                            "total_tokens".into(),
                            done.input_tokens.saturating_add(done.output_tokens).into(),
                        );
                    }
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

        let mut value = serde_json::json!({
            "passed": self.passed_count(),
            "failed": self.failed_count(),
            "total": self.cases.len(),
            "all_passed": self.all_passed(),
            "suite_kind": kind.as_str(),
            "exit_code": self.exit_code(kind, comparison),
            "repeat_ci": self.repeat_ci(),
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

    fn repeated_case(name: &str, passes: u32, k: u32, cluster: Option<&str>) -> CaseReport {
        let runs: Vec<crate::stats::RunSample> = (0..k)
            .map(|index| crate::stats::RunSample {
                passed: index < passes,
                input_tokens: index as u64 + 1,
                output_tokens: 1,
                duration_ms: 10 + index as u64,
                llm_calls: 1,
                checks: vec![("response".to_string(), index < passes, false)],
            })
            .collect();
        CaseReport {
            name: name.to_string(),
            source: "fixture.json".to_string(),
            record: None,
            grades: vec![grade("response", passes == k, "repeat representative")],
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
        assert!(
            !suite
                .to_json(crate::baseline::SuiteKind::Regression, None)
                .contains("NaN")
        );
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
            line.contains("1 of 2 cases have complete repeat statistics"),
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
            line.contains("2 of 4 cases have complete repeat statistics"),
            "denominator must be the whole suite: {line}"
        );
    }

    #[test]
    fn repeat_confidence_interval_is_bounded_to_probability_range() {
        let suite = SuiteReport {
            cases: vec![
                repeated_case("always", 2, 2, None),
                repeated_case("never", 0, 2, None),
            ],
        };

        let line = suite.repeat_ci_line().expect("repeated cases present");
        assert!(line.contains("95% CI 0%–100%"), "got: {line}");

        let json: serde_json::Value =
            serde_json::from_str(&suite.to_json(crate::baseline::SuiteKind::Regression, None))
                .unwrap();
        assert_eq!(json["repeat_ci"]["pass_rate"].as_f64(), Some(0.5));
        assert_eq!(json["repeat_ci"]["lower"].as_f64(), Some(0.0));
        assert_eq!(json["repeat_ci"]["upper"].as_f64(), Some(1.0));
        assert_eq!(json["repeat_ci"]["independent_units"].as_u64(), Some(2));
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
                checks: vec![("response".to_string(), true, false)],
            },
            crate::stats::RunSample {
                passed: true,
                input_tokens: 3,
                output_tokens: 1,
                duration_ms: 11,
                llm_calls: 1,
                checks: vec![("response".to_string(), true, false)],
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
        let report_json = suite.to_json(crate::baseline::SuiteKind::Regression, None);
        assert!(report_json.contains("\"truncated\": true"));
        let json: serde_json::Value = serde_json::from_str(&report_json).unwrap();
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
        let mut diagnostic = grade("judge", false, "advisory");
        diagnostic.diagnostic = true;
        assert!(
            case("a", vec![grade("c1", true, ""), diagnostic], None).passed(),
            "a diagnostic failure must not become a gating case failure"
        );
        // No checks cannot certify a passing case.
        assert!(!case("a", vec![], None).passed());
    }

    #[test]
    fn case_with_no_grades_has_no_score_and_does_not_pass() {
        let vacuous = case("vacuous", vec![], None);
        assert!(
            !vacuous.passed(),
            "a case with zero checks must not pass vacuously"
        );
        assert_eq!(
            vacuous.score(),
            None,
            "a case with zero checks has no score, not a perfect one"
        );
    }

    #[test]
    fn errored_case_reports_null_score_in_json() {
        // Regression: an errored case used to emit `passed: false` next to
        // `score: 1.0`, so machine consumers read a provider/setup failure as a
        // perfect run.
        let suite = SuiteReport {
            cases: vec![case("err", vec![], Some("provider timed out"))],
        };
        let json: serde_json::Value =
            serde_json::from_str(&suite.to_json(SuiteKind::Regression, None)).unwrap();
        assert_eq!(json["cases"][0]["passed"].as_bool(), Some(false));
        assert!(
            json["cases"][0]["score"].is_null(),
            "errored case must not report a numeric score, got: {}",
            json["cases"][0]["score"]
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
        assert!((report.score().unwrap() - 0.75).abs() < f64::EPSILON);
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
