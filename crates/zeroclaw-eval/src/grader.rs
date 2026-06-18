//! Grading: non-panicking checks over a [`RunRecord`].
//!
//! Phase 0 ships the expectation checks reshaped to *return* structured results
//! instead of asserting — so the harness can report every check (pass and fail) and
//! exit with a status code rather than panicking on the first failure. The
//! [`Grader`] trait is the extension point later phases hang side-effect, budget,
//! and LLM-judge graders off of.

use crate::case::TraceExpects;
use crate::record::RunRecord;
use serde::Serialize;

/// The outcome of a single check.
#[derive(Debug, Clone, Serialize)]
pub struct GradeResult {
    /// Short identifier for the check, e.g. `response_contains("hello")`.
    pub check: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Human-readable detail (especially useful on failure).
    pub detail: String,
}

impl GradeResult {
    fn new(check: String, passed: bool, detail: impl Into<String>) -> Self {
        Self {
            check,
            passed,
            detail: detail.into(),
        }
    }
}

/// A scorer over a completed run. Phase 0 has a single implementation
/// ([`ExpectationsGrader`]); the trait exists so later phases can add more.
pub trait Grader: Send + Sync {
    fn name(&self) -> &str;
    fn grade(&self, run: &RunRecord) -> Vec<GradeResult>;
}

/// Grades a run against declarative [`TraceExpects`].
pub struct ExpectationsGrader {
    pub expects: TraceExpects,
}

impl Grader for ExpectationsGrader {
    fn name(&self) -> &str {
        "expectations"
    }

    fn grade(&self, run: &RunRecord) -> Vec<GradeResult> {
        evaluate_expects(&self.expects, run)
    }
}

/// Evaluate every declared expectation against the run, one [`GradeResult`] per check.
pub fn evaluate_expects(expects: &TraceExpects, run: &RunRecord) -> Vec<GradeResult> {
    let mut out = Vec::new();
    let resp = run.final_response.as_str();

    for needle in &expects.response_contains {
        let passed = resp.contains(needle);
        out.push(GradeResult::new(
            format!("response_contains({needle:?})"),
            passed,
            if passed {
                "found".to_string()
            } else {
                format!("not found in response: {resp:?}")
            },
        ));
    }

    for needle in &expects.response_not_contains {
        let passed = !resp.contains(needle);
        out.push(GradeResult::new(
            format!("response_not_contains({needle:?})"),
            passed,
            if passed {
                "absent".to_string()
            } else {
                format!("unexpectedly present in response: {resp:?}")
            },
        ));
    }

    for tool in &expects.tools_used {
        let passed = run.tools_called.iter().any(|t| t == tool);
        out.push(GradeResult::new(
            format!("tools_used({tool:?})"),
            passed,
            if passed {
                "called".to_string()
            } else {
                format!("not called; tools called: {:?}", run.tools_called)
            },
        ));
    }

    for tool in &expects.tools_not_used {
        let passed = !run.tools_called.iter().any(|t| t == tool);
        out.push(GradeResult::new(
            format!("tools_not_used({tool:?})"),
            passed,
            if passed {
                "not called".to_string()
            } else {
                "unexpectedly called".to_string()
            },
        ));
    }

    if let Some(max) = expects.max_tool_calls {
        let actual = run.tools_called.len();
        let passed = actual <= max;
        out.push(GradeResult::new(
            format!("max_tool_calls({max})"),
            passed,
            format!("{actual} tool call(s)"),
        ));
    }

    if let Some(expected) = expects.all_tools_succeeded {
        let passed = run.all_tools_succeeded == expected;
        out.push(GradeResult::new(
            format!("all_tools_succeeded({expected})"),
            passed,
            format!("actual all_tools_succeeded = {}", run.all_tools_succeeded),
        ));
    }

    for pattern in &expects.response_matches {
        match regex::Regex::new(pattern) {
            Ok(re) => {
                let passed = re.is_match(resp);
                out.push(GradeResult::new(
                    format!("response_matches({pattern:?})"),
                    passed,
                    if passed {
                        "matched".to_string()
                    } else {
                        format!("no match in response: {resp:?}")
                    },
                ));
            }
            Err(e) => out.push(GradeResult::new(
                format!("response_matches({pattern:?})"),
                false,
                format!("invalid regex: {e}"),
            )),
        }
    }

    out
}
