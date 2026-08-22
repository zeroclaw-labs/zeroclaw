//! Grading: non-panicking checks over a [`RunRecord`].

use crate::case::TraceExpects;
use crate::record::RunRecord;
use serde::{Deserialize, Serialize};

/// Which dimension of a run a check scores. Surfaced in the JSON report so
/// per-category totals and (later) regression classification are possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeCategory {
    Response,
    Tool,
    SideEffect,
    Budget,
    Judge,
    /// The run itself, independent of any declared expectation. Lets a case that
    /// deliberately declares no expectations still emit one honest grade instead
    /// of an empty vector (see [`RunCompletedGrader`]).
    Run,
}

/// The outcome of a single check.
#[derive(Debug, Clone, Serialize)]
pub struct GradeResult {
    /// Short identifier for the check, e.g. `response_contains("hello")`.
    pub check: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Human-readable detail (especially useful on failure).
    pub detail: String,
    /// Which run dimension this check scores.
    pub category: GradeCategory,
}

impl GradeResult {
    /// Construct a grade. Public because [`Grader`] is a public trait: an
    /// out-of-crate grader (including the live-path integration tests) needs a
    /// way to produce results without depending on field order.
    pub fn new(
        check: String,
        passed: bool,
        detail: impl Into<String>,
        category: GradeCategory,
    ) -> Self {
        Self {
            check,
            passed,
            detail: detail.into(),
            category,
        }
    }
}

/// Context available to graders while the case's workspace still exists.
pub struct GradeContext<'a> {
    pub workspace: &'a std::path::Path,
}

/// A scorer over a completed run. The trait is async and workspace-aware so
/// later graders can inspect the case's temp workspace before it is torn down.
#[async_trait::async_trait]
pub trait Grader: Send + Sync {
    fn name(&self) -> &str;
    async fn grade(&self, run: &RunRecord, ctx: &GradeContext<'_>) -> Vec<GradeResult>;
}

/// Grades a run against declarative [`TraceExpects`].
pub struct ExpectationsGrader {
    pub expects: TraceExpects,
}

#[async_trait::async_trait]
impl Grader for ExpectationsGrader {
    fn name(&self) -> &str {
        "expectations"
    }

    async fn grade(&self, run: &RunRecord, _ctx: &GradeContext<'_>) -> Vec<GradeResult> {
        evaluate_expects(&self.expects, run)
    }
}

/// Emits one grade recording that the run reached grading at all.
///
/// This exists so "no grades" can never mean "passed". [`CaseReport::passed`]
/// requires a non-empty grade vector, which would otherwise make a case with
/// `allow_no_expectations` unpassable; this grader gives such a case one honest
/// thing to report instead of an empty vector. Every case gets it, so the
/// invariant "a graded case has at least one grade" holds unconditionally.
///
/// [`CaseReport::passed`]: crate::report::CaseReport::passed
pub struct RunCompletedGrader;

#[async_trait::async_trait]
impl Grader for RunCompletedGrader {
    fn name(&self) -> &str {
        "run_completed"
    }

    async fn grade(&self, _run: &RunRecord, _ctx: &GradeContext<'_>) -> Vec<GradeResult> {
        vec![GradeResult::new(
            "run_completed".to_string(),
            true,
            "the case ran to completion and reached grading",
            GradeCategory::Run,
        )]
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
            GradeCategory::Response,
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
            GradeCategory::Response,
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
            GradeCategory::Tool,
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
            GradeCategory::Tool,
        ));
    }

    if let Some(max) = expects.max_tool_calls {
        let actual = run.tools_called.len();
        let passed = actual <= max;
        out.push(GradeResult::new(
            format!("max_tool_calls({max})"),
            passed,
            format!("{actual} tool call(s)"),
            GradeCategory::Tool,
        ));
    }

    if let Some(expected) = expects.all_tools_succeeded {
        let passed = run.all_tools_succeeded == expected;
        out.push(GradeResult::new(
            format!("all_tools_succeeded({expected})"),
            passed,
            format!("actual all_tools_succeeded = {}", run.all_tools_succeeded),
            GradeCategory::Tool,
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
                    GradeCategory::Response,
                ));
            }
            Err(e) => out.push(GradeResult::new(
                format!("response_matches({pattern:?})"),
                false,
                format!("invalid regex: {e}"),
                GradeCategory::Response,
            )),
        }
    }

    out
}

/// The production grader catalog for a case: the checks every case gets, in the
/// order their grades appear in the report.
///
/// Split out from [`grade_run`] so the runner can accept a caller-supplied
/// catalog (`run_case_with_graders`) while still defaulting to exactly this set
/// — a test-injected grader therefore runs on the same code path production
/// uses, not a parallel one.
pub fn default_graders(trace: &crate::case::LlmTrace) -> Vec<Box<dyn Grader>> {
    vec![
        Box::new(RunCompletedGrader),
        Box::new(ExpectationsGrader {
            expects: trace.expects.clone(),
        }),
    ]
}

/// Run `graders` against the record while the case workspace is still alive,
/// returning every grade concatenated.
pub async fn grade_with(
    graders: &[Box<dyn Grader>],
    record: &RunRecord,
    workspace: &std::path::Path,
) -> Vec<GradeResult> {
    let ctx = GradeContext { workspace };
    let mut grades = Vec::new();
    for grader in graders {
        grades.extend(grader.grade(record, &ctx).await);
    }
    grades
}

/// Build the case's default graders and run them while the workspace is alive.
pub async fn grade_run(
    trace: &crate::case::LlmTrace,
    record: &RunRecord,
    workspace: &std::path::Path,
) -> Vec<GradeResult> {
    grade_with(&default_graders(trace), record, workspace).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::TraceExpects;
    use crate::record::RunRecord;

    #[tokio::test]
    async fn grades_run_while_workspace_alive() {
        // A grader receives, through GradeContext, a workspace path that exists at
        // grade time. `run_case` awaits `grade_run` before its `tmp` (TempDir)
        // drops, so a workspace-aware grader always sees a live directory. The
        // control below (drop, then re-check the same path) proves this exists()
        // check is meaningful, not tautological: it flips to false once dropped.
        struct Probe;
        #[async_trait::async_trait]
        impl Grader for Probe {
            fn name(&self) -> &str {
                "probe"
            }
            async fn grade(&self, _run: &RunRecord, ctx: &GradeContext<'_>) -> Vec<GradeResult> {
                vec![GradeResult::new(
                    "workspace_alive".to_string(),
                    ctx.workspace.exists(),
                    "",
                    GradeCategory::SideEffect,
                )]
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let record = run("hi", &[], true);
        let grades = Probe
            .grade(&record, &GradeContext { workspace: &path })
            .await;
        assert!(grades[0].passed, "workspace must exist during grading");

        // Control: once the workspace drops, the same probe fails on the same path,
        // so the assertion above is not vacuously true.
        drop(tmp);
        let after = Probe
            .grade(&record, &GradeContext { workspace: &path })
            .await;
        assert!(
            !after[0].passed,
            "probe must fail once the workspace is torn down"
        );
    }

    fn run(resp: &str, tools: &[&str], all_ok: bool) -> RunRecord {
        RunRecord {
            final_response: resp.to_string(),
            history: Vec::new(),
            tools_called: tools.iter().map(|s| s.to_string()).collect(),
            all_tools_succeeded: all_ok,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    #[test]
    fn empty_expectations_produce_no_results() {
        let out = evaluate_expects(&TraceExpects::default(), &run("hi", &[], true));
        assert!(out.is_empty());
    }

    #[test]
    fn response_contains_passes_and_fails() {
        let expects = TraceExpects {
            response_contains: vec!["hello".to_string(), "missing".to_string()],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &run("hello world", &[], true));
        assert_eq!(out.len(), 2);
        assert!(out[0].passed);
        assert_eq!(out[0].check, r#"response_contains("hello")"#);
        assert!(!out[1].passed);
    }

    #[test]
    fn response_not_contains_inverts_the_check() {
        let expects = TraceExpects {
            response_not_contains: vec!["secret".to_string(), "world".to_string()],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &run("hello world", &[], true));
        assert!(out[0].passed); // "secret" absent -> pass
        assert!(!out[1].passed); // "world" present -> fail
    }

    #[test]
    fn tools_used_and_not_used_are_evaluated_in_order() {
        let expects = TraceExpects {
            tools_used: vec!["search".to_string(), "absent".to_string()],
            tools_not_used: vec!["danger".to_string(), "search".to_string()],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &run("", &["search", "read"], true));
        assert!(out[0].passed); // tools_used("search") -> called
        assert!(!out[1].passed); // tools_used("absent") -> not called
        assert!(out[2].passed); // tools_not_used("danger") -> not called
        assert!(!out[3].passed); // tools_not_used("search") -> called
    }

    #[test]
    fn max_tool_calls_is_inclusive() {
        let expects = TraceExpects {
            max_tool_calls: Some(2),
            ..Default::default()
        };
        assert!(evaluate_expects(&expects, &run("", &["a", "b"], true))[0].passed);
        assert!(!evaluate_expects(&expects, &run("", &["a", "b", "c"], true))[0].passed);
    }

    #[test]
    fn all_tools_succeeded_matches_expected_value() {
        let want_true = TraceExpects {
            all_tools_succeeded: Some(true),
            ..Default::default()
        };
        assert!(evaluate_expects(&want_true, &run("", &[], true))[0].passed);
        assert!(!evaluate_expects(&want_true, &run("", &[], false))[0].passed);

        let want_false = TraceExpects {
            all_tools_succeeded: Some(false),
            ..Default::default()
        };
        assert!(evaluate_expects(&want_false, &run("", &[], false))[0].passed);
    }

    #[test]
    fn response_matches_regex_and_reports_invalid_pattern() {
        let expects = TraceExpects {
            response_matches: vec!["^h.*o$".to_string(), "(unclosed".to_string()],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &run("hello", &[], true));
        assert!(out[0].passed); // matches ^h.*o$
        assert!(!out[1].passed); // invalid regex -> fail, not a panic
        assert!(out[1].detail.contains("invalid regex"));
    }

    #[test]
    fn invalid_response_regex_does_not_short_circuit_later_checks() {
        let expects = TraceExpects {
            response_matches: vec!["(unclosed".to_string(), "world$".to_string()],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &run("hello world", &[], true));
        assert_eq!(out.len(), 2);
        assert!(!out[0].passed);
        assert!(out[0].detail.contains("invalid regex"));
        assert!(out[1].passed);
        assert_eq!(out[1].detail, "matched");
    }
}
