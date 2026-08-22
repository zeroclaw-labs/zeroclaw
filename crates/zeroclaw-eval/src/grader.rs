//! Grading: non-panicking checks over a [`RunRecord`].

use crate::case::{ToolPayloadExpect, TraceExpects};
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

/// Which half of a recorded tool call a [`ToolPayloadExpect`] inspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadKind {
    Arguments,
    Result,
}

impl PayloadKind {
    fn check_name(self) -> &'static str {
        match self {
            PayloadKind::Arguments => "tool_arguments_contain",
            PayloadKind::Result => "tool_results_contain",
        }
    }
}

/// Grade one argument/result expectation against the calls actually dispatched.
///
/// The failure detail always names the observed payload(s) so a CI failure is
/// diagnosable without re-running locally.
fn grade_payload(expect: &ToolPayloadExpect, run: &RunRecord, kind: PayloadKind) -> GradeResult {
    let tool = expect.tool.as_str();
    let needle = expect.needle.as_str();
    let payload_of = |c: &crate::observer::RecordedCall| match kind {
        PayloadKind::Arguments => c.arguments.clone(),
        PayloadKind::Result => c.result.clone(),
    };

    let matching: Vec<String> = run
        .tool_calls
        .iter()
        .filter(|c| c.name == tool)
        .map(payload_of)
        .collect();

    let check = match expect.call_index {
        Some(idx) => format!("{}({tool:?}[{idx}], {needle:?})", kind.check_name()),
        None => format!("{}({tool:?}, {needle:?})", kind.check_name()),
    };

    match expect.call_index {
        Some(idx) => match matching.get(idx) {
            Some(payload) => {
                let passed = payload.contains(needle);
                GradeResult::new(
                    check,
                    passed,
                    if passed {
                        format!("found in call {idx}")
                    } else {
                        format!("call {idx} payload was {payload:?}")
                    },
                )
            }
            None => GradeResult::new(
                check,
                false,
                format!(
                    "no call {idx} to {tool:?}; only {} call(s) observed: {matching:?}",
                    matching.len()
                ),
            ),
        },
        None => {
            if matching.is_empty() {
                GradeResult::new(
                    check,
                    false,
                    format!(
                        "{tool:?} was never called; tools called: {:?}",
                        run.tools_called
                    ),
                )
            } else {
                let passed = matching.iter().any(|p| p.contains(needle));
                GradeResult::new(
                    check,
                    passed,
                    if passed {
                        "found".to_string()
                    } else {
                        format!("not found; observed payloads: {matching:?}")
                    },
                )
            }
        }
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

    if let Some(min) = expects.min_tool_calls {
        let actual = run.tools_called.len();
        let passed = actual >= min;
        out.push(GradeResult::new(
            format!("min_tool_calls({min})"),
            passed,
            format!("{actual} tool call(s)"),
        ));
    }

    if let Some(exact) = expects.exact_tool_calls {
        let actual = run.tools_called.len();
        let passed = actual == exact;
        out.push(GradeResult::new(
            format!("exact_tool_calls({exact})"),
            passed,
            format!("{actual} tool call(s): {:?}", run.tools_called),
        ));
    }

    for expect in &expects.tool_arguments_contain {
        out.push(grade_payload(expect, run, PayloadKind::Arguments));
    }

    for expect in &expects.tool_results_contain {
        out.push(grade_payload(expect, run, PayloadKind::Result));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{ToolPayloadExpect, TraceExpects};
    use crate::observer::RecordedCall;
    use crate::record::RunRecord;

    fn run(resp: &str, tools: &[&str], all_ok: bool) -> RunRecord {
        RunRecord {
            final_response: resp.to_string(),
            history: Vec::new(),
            tools_called: tools.iter().map(|s| s.to_string()).collect(),
            tool_calls: tools
                .iter()
                .map(|s| RecordedCall {
                    name: (*s).to_string(),
                    arguments: String::new(),
                    result: String::new(),
                    success: all_ok,
                })
                .collect(),
            all_tools_succeeded: all_ok,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// A record whose recorded calls carry real argument/result payloads.
    fn run_with_calls(resp: &str, calls: Vec<RecordedCall>) -> RunRecord {
        RunRecord {
            final_response: resp.to_string(),
            history: Vec::new(),
            tools_called: calls.iter().map(|c| c.name.clone()).collect(),
            all_tools_succeeded: calls.iter().all(|c| c.success),
            tool_calls: calls,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    fn call(name: &str, arguments: &str, result: &str) -> RecordedCall {
        RecordedCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
            result: result.to_string(),
            success: true,
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

    // ---- B1: argument / result round-trip expectations ----

    #[test]
    fn tool_results_contain_passes_on_exact_unicode() {
        // The Unicode string must come back from the *tool result*, not from a
        // scripted final response.
        let record = run_with_calls(
            "Echoed: naïve café 日本語 ✓",
            vec![call(
                "echo",
                r#"{"message":"naïve café 日本語 ✓"}"#,
                "naïve café 日本語 ✓",
            )],
        );
        let expects = TraceExpects {
            tool_arguments_contain: vec![ToolPayloadExpect {
                tool: "echo".to_string(),
                needle: "naïve café 日本語 ✓".to_string(),
                call_index: None,
            }],
            tool_results_contain: vec![ToolPayloadExpect {
                tool: "echo".to_string(),
                needle: "naïve café 日本語 ✓".to_string(),
                call_index: None,
            }],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &record);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|g| g.passed), "grades: {out:?}");
    }

    #[test]
    fn tool_arguments_contain_fails_when_argument_mutated() {
        // The mutation proof: `echo` still dispatched and still succeeded, and the
        // final response still carries the Unicode text — but the argument that
        // crossed the dispatch boundary was mangled. The grade must go red.
        let record = run_with_calls(
            "Echoed: naïve café 日本語 ✓",
            vec![call(
                "echo",
                r#"{"message":"naive cafe ??? x"}"#,
                "naive cafe ??? x",
            )],
        );
        let expects = TraceExpects {
            response_contains: vec!["naïve café 日本語 ✓".to_string()],
            tools_used: vec!["echo".to_string()],
            all_tools_succeeded: Some(true),
            tool_arguments_contain: vec![ToolPayloadExpect {
                tool: "echo".to_string(),
                needle: "naïve café 日本語 ✓".to_string(),
                call_index: None,
            }],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &record);
        // Everything the old fixture asserted still passes...
        for name in ["response_contains", "tools_used", "all_tools_succeeded"] {
            let g = out.iter().find(|g| g.check.starts_with(name)).unwrap();
            assert!(g.passed, "{name} should still pass: {g:?}");
        }
        // ...and only the boundary check catches the regression.
        let arg_grade = out
            .iter()
            .find(|g| g.check.starts_with("tool_arguments_contain"))
            .unwrap();
        assert!(
            !arg_grade.passed,
            "mutated argument must fail the grade: {arg_grade:?}"
        );
        assert!(arg_grade.detail.contains("naive cafe"));
    }

    #[test]
    fn tool_results_contain_fails_when_result_mutated() {
        let record = run_with_calls(
            "Echoed: naïve café 日本語 ✓",
            vec![call(
                "echo",
                r#"{"message":"naïve café 日本語 ✓"}"#,
                "(empty)",
            )],
        );
        let expects = TraceExpects {
            tool_results_contain: vec![ToolPayloadExpect {
                tool: "echo".to_string(),
                needle: "naïve café 日本語 ✓".to_string(),
                call_index: None,
            }],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &record);
        assert_eq!(out.len(), 1);
        assert!(!out[0].passed);
        assert!(out[0].detail.contains("(empty)"));
    }

    #[test]
    fn tool_payload_expect_fails_when_tool_never_called() {
        let record = run_with_calls("no tools here", vec![]);
        let expects = TraceExpects {
            tool_arguments_contain: vec![ToolPayloadExpect {
                tool: "echo".to_string(),
                needle: "alpha".to_string(),
                call_index: None,
            }],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &record);
        assert!(!out[0].passed);
        assert!(out[0].detail.contains("never called"));
    }

    // ---- B2: exact call count and per-call ordering ----

    #[test]
    fn exact_tool_calls_fails_when_one_dispatch_missing() {
        // The reviewer's counterexample verbatim: one `echo` call, a scripted
        // "Echoed: beta" final response, everything successful. The old
        // expectations pass; `exact_tool_calls(2)` must not.
        let record = run_with_calls(
            "Echoed: beta",
            vec![call("echo", r#"{"message":"beta"}"#, "beta")],
        );
        let expects = TraceExpects {
            response_contains: vec!["beta".to_string()],
            tools_used: vec!["echo".to_string()],
            max_tool_calls: Some(2),
            all_tools_succeeded: Some(true),
            exact_tool_calls: Some(2),
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &record);
        for name in [
            "response_contains",
            "tools_used",
            "max_tool_calls",
            "all_tools_succeeded",
        ] {
            let g = out.iter().find(|g| g.check.starts_with(name)).unwrap();
            assert!(g.passed, "{name} should still pass: {g:?}");
        }
        let exact = out
            .iter()
            .find(|g| g.check.starts_with("exact_tool_calls"))
            .unwrap();
        assert!(!exact.passed, "a missing dispatch must fail: {exact:?}");
        assert!(exact.detail.contains("1 tool call(s)"));
    }

    #[test]
    fn exact_tool_calls_passes_on_two_dispatches() {
        let record = run_with_calls(
            "Echoed: beta",
            vec![
                call("echo", r#"{"message":"alpha"}"#, "alpha"),
                call("echo", r#"{"message":"beta"}"#, "beta"),
            ],
        );
        let expects = TraceExpects {
            exact_tool_calls: Some(2),
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &record);
        assert_eq!(out.len(), 1);
        assert!(out[0].passed, "grades: {out:?}");
    }

    #[test]
    fn indexed_payload_expect_grades_per_call_ordering() {
        let record = run_with_calls(
            "Echoed: beta",
            vec![
                call("echo", r#"{"message":"alpha"}"#, "alpha"),
                call("echo", r#"{"message":"beta"}"#, "beta"),
            ],
        );
        let ordered = TraceExpects {
            tool_arguments_contain: vec![
                ToolPayloadExpect {
                    tool: "echo".to_string(),
                    needle: "alpha".to_string(),
                    call_index: Some(0),
                },
                ToolPayloadExpect {
                    tool: "echo".to_string(),
                    needle: "beta".to_string(),
                    call_index: Some(1),
                },
            ],
            ..Default::default()
        };
        let out = evaluate_expects(&ordered, &record);
        assert!(out.iter().all(|g| g.passed), "grades: {out:?}");

        // Swapped order must fail — this is what makes the ordering claim graded
        // rather than implied by the scripted text.
        let swapped = TraceExpects {
            tool_arguments_contain: vec![ToolPayloadExpect {
                tool: "echo".to_string(),
                needle: "beta".to_string(),
                call_index: Some(0),
            }],
            ..Default::default()
        };
        let out = evaluate_expects(&swapped, &record);
        assert!(!out[0].passed, "grades: {out:?}");
        assert!(out[0].detail.contains("alpha"));
    }

    #[test]
    fn indexed_payload_expect_fails_when_index_out_of_range() {
        let record = run_with_calls(
            "Echoed: beta",
            vec![call("echo", r#"{"message":"beta"}"#, "beta")],
        );
        let expects = TraceExpects {
            tool_arguments_contain: vec![ToolPayloadExpect {
                tool: "echo".to_string(),
                needle: "beta".to_string(),
                call_index: Some(1),
            }],
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &record);
        assert!(!out[0].passed);
        assert!(out[0].detail.contains("only 1 call(s) observed"));
    }

    #[test]
    fn min_tool_calls_bounds_below() {
        let record = run_with_calls("done", vec![call("echo", "{}", "x")]);
        let expects = TraceExpects {
            min_tool_calls: Some(2),
            ..Default::default()
        };
        let out = evaluate_expects(&expects, &record);
        assert!(!out[0].passed);
        assert_eq!(out[0].check, "min_tool_calls(2)");
    }
}
