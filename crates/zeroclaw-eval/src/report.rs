//! Pass/fail aggregation and rendering.

use crate::grader::GradeResult;

/// The result of running a single eval case.
#[derive(Debug)]
pub struct CaseReport {
    /// The trace's `model_name`.
    pub name: String,
    /// The fixture file name the case came from.
    pub source: String,
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

    /// Process exit code for a completed run: 0 iff every case passed.
    /// Kept as a pure function so the CLI gate is testable at its real boundary.
    pub fn exit_code(&self) -> i32 {
        if self.all_passed() { 0 } else { 1 }
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
    pub fn to_json(&self) -> String {
        let cases: Vec<serde_json::Value> = self
            .cases
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "source": c.source,
                    "passed": c.passed(),
                    "error": c.error,
                    "grades": c.grades,
                })
            })
            .collect();

        let value = serde_json::json!({
            "passed": self.passed_count(),
            "failed": self.failed_count(),
            "total": self.cases.len(),
            "all_passed": self.all_passed(),
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
        }
    }

    fn case(name: &str, grades: Vec<GradeResult>, error: Option<&str>) -> CaseReport {
        CaseReport {
            name: name.to_string(),
            source: "fixture.json".to_string(),
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

    #[test]
    fn exit_code_is_zero_when_all_cases_pass() {
        let suite = SuiteReport {
            cases: vec![case("ok", vec![grade("c", true, "")], None)],
        };
        assert!(suite.all_passed());
        assert_eq!(suite.exit_code(), 0);
    }

    #[test]
    fn exit_code_is_one_when_any_case_fails() {
        let suite = SuiteReport {
            cases: vec![
                case("ok", vec![grade("c", true, "")], None),
                case("bad", vec![grade("c", false, "")], None),
            ],
        };
        assert!(!suite.all_passed());
        assert_eq!(suite.exit_code(), 1);
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
    }
}
