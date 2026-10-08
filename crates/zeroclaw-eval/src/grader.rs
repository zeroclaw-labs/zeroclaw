//! Grading: non-panicking checks over a [`RunRecord`].

use crate::case::{
    BudgetExpects, ToolPayloadExpect, TraceExpects, WorkspaceExpects, validate_workspace_rel_path,
};
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
    /// The case itself is misconfigured (e.g. it declares no effective checks).
    Config,
}

impl GradeCategory {
    /// The snake_case label used as a key in the JSON report's category totals.
    pub fn as_str(self) -> &'static str {
        match self {
            GradeCategory::Response => "response",
            GradeCategory::Tool => "tool",
            GradeCategory::SideEffect => "side_effect",
            GradeCategory::Budget => "budget",
            GradeCategory::Judge => "judge",
            GradeCategory::Config => "config",
        }
    }
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

/// Grades end-state files in the case workspace. Every path is validated first;
/// a path that escapes the workspace is a FAILED grade, never a filesystem access.
pub struct WorkspaceGrader {
    pub expects: WorkspaceExpects,
}

#[async_trait::async_trait]
impl Grader for WorkspaceGrader {
    fn name(&self) -> &str {
        "workspace"
    }

    async fn grade(&self, _run: &RunRecord, ctx: &GradeContext<'_>) -> Vec<GradeResult> {
        let mut out = Vec::new();

        for rel in &self.expects.file_exists {
            let check = format!("file_exists({rel:?})");
            match validate_workspace_rel_path(rel) {
                Ok(()) => {
                    let exists = ctx.workspace.join(rel).is_file();
                    out.push(GradeResult::new(
                        check,
                        exists,
                        if exists { "present" } else { "missing" },
                        GradeCategory::SideEffect,
                    ));
                }
                Err(_) => out.push(GradeResult::new(
                    check,
                    false,
                    "path escapes workspace",
                    GradeCategory::SideEffect,
                )),
            }
        }

        for rel in &self.expects.file_absent {
            let check = format!("file_absent({rel:?})");
            match validate_workspace_rel_path(rel) {
                Ok(()) => {
                    let absent = !ctx.workspace.join(rel).exists();
                    out.push(GradeResult::new(
                        check,
                        absent,
                        if absent {
                            "absent"
                        } else {
                            "unexpectedly present"
                        },
                        GradeCategory::SideEffect,
                    ));
                }
                Err(_) => out.push(GradeResult::new(
                    check,
                    false,
                    "path escapes workspace",
                    GradeCategory::SideEffect,
                )),
            }
        }

        for (rel, needles) in &self.expects.file_contains {
            if validate_workspace_rel_path(rel).is_err() {
                out.push(GradeResult::new(
                    format!("file_contains({rel:?})"),
                    false,
                    "path escapes workspace",
                    GradeCategory::SideEffect,
                ));
                continue;
            }
            let contents = std::fs::read_to_string(ctx.workspace.join(rel));
            for needle in needles {
                let check = format!("file_contains({rel:?}, {needle:?})");
                match &contents {
                    Ok(text) => {
                        let found = text.contains(needle);
                        out.push(GradeResult::new(
                            check,
                            found,
                            if found { "found" } else { "not found in file" },
                            GradeCategory::SideEffect,
                        ));
                    }
                    Err(e) => out.push(GradeResult::new(
                        check,
                        false,
                        format!("cannot read file: {e}"),
                        GradeCategory::SideEffect,
                    )),
                }
            }
        }

        out
    }
}

/// Grades a run against resource ceilings. Each present bound is one check, and
/// each bound is inclusive (`actual <= max` passes).
pub struct BudgetGrader {
    pub expects: BudgetExpects,
}

#[async_trait::async_trait]
impl Grader for BudgetGrader {
    fn name(&self) -> &str {
        "budget"
    }

    async fn grade(&self, run: &RunRecord, _ctx: &GradeContext<'_>) -> Vec<GradeResult> {
        let run = run.completion_or_default();
        // A bound is one inclusive check (`actual <= max`), tagged Budget.
        let check = |label: &str, max: u64, actual: u64| {
            GradeResult::new(
                format!("{label}({max})"),
                actual <= max,
                format!("actual {actual}"),
                GradeCategory::Budget,
            )
        };
        let mut out = Vec::new();
        if let Some(max) = self.expects.max_input_tokens {
            out.push(check("max_input_tokens", max, run.input_tokens));
        }
        if let Some(max) = self.expects.max_output_tokens {
            out.push(check("max_output_tokens", max, run.output_tokens));
        }
        if let Some(max) = self.expects.max_total_tokens {
            out.push(check(
                "max_total_tokens",
                max,
                run.input_tokens.saturating_add(run.output_tokens),
            ));
        }
        if let Some(max) = self.expects.max_duration_ms {
            out.push(check("max_duration_ms", max, run.duration_ms));
        }
        if let Some(max) = self.expects.max_llm_calls {
            out.push(check(
                "max_llm_calls",
                u64::from(max),
                u64::from(run.llm_calls),
            ));
        }
        out
    }
}

/// Grades JSON-pointer checks against the final response parsed as JSON.
pub struct ResponseJsonGrader {
    pub pointers: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Parse `text` as JSON, falling back to the first ```json fenced block.
fn parse_response_json(text: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) {
        return Some(value);
    }
    let start = text.find("```json")? + "```json".len();
    let rest = &text[start..];
    let end = rest.find("```")?;
    serde_json::from_str(rest[..end].trim()).ok()
}

#[async_trait::async_trait]
impl Grader for ResponseJsonGrader {
    fn name(&self) -> &str {
        "response_json"
    }

    async fn grade(&self, run: &RunRecord, _ctx: &GradeContext<'_>) -> Vec<GradeResult> {
        let parsed = parse_response_json(&run.completion_or_default().final_response);
        self.pointers
            .iter()
            .map(|(pointer, expected)| {
                let check = format!("response_json({pointer:?})");
                match &parsed {
                    None => GradeResult::new(
                        check,
                        false,
                        "response is not JSON",
                        GradeCategory::Response,
                    ),
                    Some(value) => {
                        let actual = value.pointer(pointer);
                        let passed = actual == Some(expected);
                        let detail = match actual {
                            Some(a) => format!("got {a}"),
                            None => "pointer not present".to_string(),
                        };
                        GradeResult::new(check, passed, detail, GradeCategory::Response)
                    }
                }
            })
            .collect()
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
fn grade_payload(
    expect: &ToolPayloadExpect,
    run: &crate::record::RunCompletion,
    kind: PayloadKind,
) -> GradeResult {
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
                    GradeCategory::Tool,
                )
            }
            None => GradeResult::new(
                check,
                false,
                format!(
                    "no call {idx} to {tool:?}; only {} call(s) observed: {matching:?}",
                    matching.len()
                ),
                GradeCategory::Tool,
            ),
        },
        None => {
            if matching.is_empty() {
                GradeResult::new(
                    check,
                    false,
                    format!(
                        "{tool:?} was never called; tools called: {:?}",
                        run.tool_names()
                    ),
                    GradeCategory::Tool,
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
                    GradeCategory::Tool,
                )
            }
        }
    }
}

/// Evaluate every declared expectation against the run, one [`GradeResult`] per check.
pub fn evaluate_expects(expects: &TraceExpects, run: &RunRecord) -> Vec<GradeResult> {
    let run = run.completion_or_default();
    let mut out = Vec::new();
    let resp = run.final_response.as_str();
    let tool_names = run.tool_names();

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
        let passed = tool_names.iter().any(|name| *name == tool);
        out.push(GradeResult::new(
            format!("tools_used({tool:?})"),
            passed,
            if passed {
                "called".to_string()
            } else {
                format!("not called; tools called: {tool_names:?}")
            },
            GradeCategory::Tool,
        ));
    }

    for tool in &expects.tools_not_used {
        let passed = !tool_names.iter().any(|name| *name == tool);
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
        let actual = run.tool_calls.len();
        let passed = actual <= max;
        out.push(GradeResult::new(
            format!("max_tool_calls({max})"),
            passed,
            format!("{actual} tool call(s)"),
            GradeCategory::Tool,
        ));
    }

    if let Some(min) = expects.min_tool_calls {
        let actual = run.tool_calls.len();
        let passed = actual >= min;
        out.push(GradeResult::new(
            format!("min_tool_calls({min})"),
            passed,
            format!("{actual} tool call(s)"),
            GradeCategory::Tool,
        ));
    }

    if let Some(exact) = expects.exact_tool_calls {
        let actual = run.tool_calls.len();
        let passed = actual == exact;
        out.push(GradeResult::new(
            format!("exact_tool_calls({exact})"),
            passed,
            format!("{actual} tool call(s): {tool_names:?}"),
            GradeCategory::Tool,
        ));
    }

    for expect in &expects.tool_arguments_contain {
        out.push(grade_payload(expect, &run, PayloadKind::Arguments));
    }

    for expect in &expects.tool_results_contain {
        out.push(grade_payload(expect, &run, PayloadKind::Result));
    }

    if let Some(expected) = expects.all_tools_succeeded {
        let actual = run.all_tools_succeeded();
        let passed = actual == expected;
        out.push(GradeResult::new(
            format!("all_tools_succeeded({expected})"),
            passed,
            format!("actual all_tools_succeeded = {actual}"),
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

/// Build the production grader catalog for a case.
///
/// Keeping construction separate lets the runner accept a test-supplied
/// catalog while production still has one canonical default.
pub fn default_graders(trace: &crate::case::LlmTrace) -> Vec<Box<dyn Grader>> {
    let expects = &trace.expects;
    let mut graders: Vec<Box<dyn Grader>> = vec![Box::new(ExpectationsGrader {
        expects: expects.clone(),
    })];
    if let Some(workspace) = &expects.workspace {
        graders.push(Box::new(WorkspaceGrader {
            expects: workspace.clone(),
        }));
    }
    if let Some(budget) = &expects.budget {
        graders.push(Box::new(BudgetGrader {
            expects: budget.clone(),
        }));
    }
    if !expects.response_json.is_empty() {
        graders.push(Box::new(ResponseJsonGrader {
            pointers: expects.response_json.clone(),
        }));
    }
    graders
}

/// Run a supplied grader catalog while the workspace is alive, returning all
/// grades in catalog order.
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
    // Fail closed: a case that produced no grade asserted nothing about the run,
    // so an empty grade list must not read as success. `TraceExpects::validate`
    // rejects most of these at load time; this is the runtime backstop for cases
    // built in-process (tests, embedded fixtures) that never went through it.
    if grades.is_empty() {
        grades.push(GradeResult::new(
            "effective_checks".to_string(),
            false,
            "case declares no effective checks",
            GradeCategory::Config,
        ));
    }
    grades
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{ToolPayloadExpect, TraceExpects};
    use crate::observer::RecordedCall;
    use crate::record::RunRecord;

    #[tokio::test]
    async fn grades_run_while_workspace_alive() {
        // Control for the two runner-path regressions in `runner.rs` and
        // `live.rs`: those assert the runner still has the workspace alive when
        // it awaits grading, and this proves that exists() check is meaningful
        // rather than tautological, because the same probe on the same path
        // flips to false once the directory is dropped.
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
            provenance: crate::record::CaseProvenance {
                schema: crate::record::RECORD_SCHEMA.to_string(),
                mode: crate::Mode::Replay,
                case_id: "test".to_string(),
                case_hash: String::new(),
                provider_ref: "scripted".to_string(),
                tool_surface: crate::record::ToolSurface::default(),
                sandbox: crate::record::SandboxStamp {
                    autonomy: "supervised".to_string(),
                    workspace_only: false,
                },
            },
            completion: Some(crate::record::RunCompletion {
                final_response: resp.to_string(),
                tool_calls: tools
                    .iter()
                    .map(|s| RecordedCall {
                        name: (*s).to_string(),
                        arguments: String::new(),
                        result: String::new(),
                        success: all_ok,
                    })
                    .collect(),
                ..crate::record::RunCompletion::default()
            }),
        }
    }

    /// A record whose recorded calls carry real argument/result payloads.
    fn run_with_calls(resp: &str, calls: Vec<RecordedCall>) -> RunRecord {
        let mut record = run(resp, &[], true);
        record.completion.as_mut().unwrap().tool_calls = calls;
        record
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
    fn empty_expectations_grade_as_an_explicit_configuration_failure() {
        // Replaces `empty_expectations_produce_no_results`, which codified the
        // silent-green behavior. A case that declares nothing must now surface a
        // failing `config` grade rather than an empty (vacuously passing) list.
        let trace: crate::case::LlmTrace =
            serde_json::from_str(r#"{"model_name":"vacuous","turns":[],"expects":{}}"#).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let grades = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(grade_with(
                &default_graders(&trace),
                &run("hi", &[], true),
                tmp.path(),
            ));
        assert_eq!(grades.len(), 1, "expected one config grade: {grades:?}");
        assert!(!grades[0].passed, "the config grade must fail: {grades:?}");
        assert_eq!(grades[0].category, GradeCategory::Config);
        assert!(
            grades[0].detail.contains("no effective checks"),
            "detail must explain the failure: {:?}",
            grades[0].detail
        );
        // The raw expectation evaluator still emits nothing; the fail-closed
        // decision lives in grade_with, so this documents the boundary.
        assert!(evaluate_expects(&TraceExpects::default(), &run("hi", &[], true)).is_empty());
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
        assert!(!evaluate_expects(&want_true, &run("", &["echo"], false))[0].passed);

        let want_false = TraceExpects {
            all_tools_succeeded: Some(false),
            ..Default::default()
        };
        assert!(evaluate_expects(&want_false, &run("", &["echo"], false))[0].passed);
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

    use std::collections::BTreeMap;

    fn find<'a>(grades: &'a [GradeResult], check_prefix: &str) -> &'a GradeResult {
        grades
            .iter()
            .find(|g| g.check.starts_with(check_prefix))
            .unwrap_or_else(|| panic!("no grade starting with {check_prefix:?} in {grades:?}"))
    }

    fn dummy_ctx() -> GradeContext<'static> {
        GradeContext {
            workspace: std::path::Path::new("."),
        }
    }

    #[tokio::test]
    async fn workspace_grader_checks_exists_absent_contains() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("out.txt"), "hello world").unwrap();
        let expects = WorkspaceExpects {
            file_exists: vec!["out.txt".to_string()],
            file_absent: vec!["nope.txt".to_string()],
            file_contains: BTreeMap::from([(
                "out.txt".to_string(),
                vec!["hello".to_string(), "missing".to_string()],
            )]),
        };
        let grades = WorkspaceGrader { expects }
            .grade(
                &run("", &[], true),
                &GradeContext {
                    workspace: tmp.path(),
                },
            )
            .await;
        assert!(find(&grades, "file_exists(\"out.txt\")").passed);
        assert!(find(&grades, "file_absent(\"nope.txt\")").passed);
        assert!(find(&grades, "file_contains(\"out.txt\", \"hello\")").passed);
        assert!(!find(&grades, "file_contains(\"out.txt\", \"missing\")").passed);
        assert!(
            grades
                .iter()
                .all(|g| g.category == GradeCategory::SideEffect)
        );
    }

    #[tokio::test]
    async fn workspace_grader_rejects_escaping_paths_as_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let expects = WorkspaceExpects {
            file_exists: vec!["../escape.txt".to_string()],
            file_absent: vec!["/etc/passwd".to_string()],
            file_contains: BTreeMap::from([("../x".to_string(), vec!["y".to_string()])]),
        };
        let grades = WorkspaceGrader { expects }
            .grade(
                &run("", &[], true),
                &GradeContext {
                    workspace: tmp.path(),
                },
            )
            .await;
        assert_eq!(grades.len(), 3);
        assert!(grades.iter().all(|g| !g.passed));
        assert!(grades.iter().all(|g| g.detail == "path escapes workspace"));
    }

    #[tokio::test]
    async fn budget_grader_boundary_inclusive() {
        let mut record = run("", &[], true);
        record.completion.as_mut().unwrap().input_tokens = 100;
        let at_limit = BudgetGrader {
            expects: BudgetExpects {
                max_input_tokens: Some(100),
                ..Default::default()
            },
        }
        .grade(&record, &dummy_ctx())
        .await;
        assert!(at_limit[0].passed, "limit == actual must pass (inclusive)");

        let below = BudgetGrader {
            expects: BudgetExpects {
                max_input_tokens: Some(99),
                ..Default::default()
            },
        }
        .grade(&record, &dummy_ctx())
        .await;
        assert!(!below[0].passed, "limit-1 < actual must fail");
        assert!(at_limit[0].category == GradeCategory::Budget);
    }

    #[tokio::test]
    async fn budget_total_saturates_instead_of_wrapping() {
        let mut record = run("", &[], true);
        let completion = record.completion.as_mut().unwrap();
        completion.input_tokens = u64::MAX;
        completion.output_tokens = 1;
        let grades = BudgetGrader {
            expects: BudgetExpects {
                max_total_tokens: Some(u64::MAX - 1),
                ..BudgetExpects::default()
            },
        }
        .grade(&record, &dummy_ctx())
        .await;
        assert_eq!(grades.len(), 1);
        assert!(!grades[0].passed);
        assert_eq!(grades[0].detail, format!("actual {}", u64::MAX));
    }

    #[tokio::test]
    async fn response_json_pointer_hits_and_misses() {
        let pointers = BTreeMap::from([
            ("/status".to_string(), serde_json::json!("ok")),
            ("/count".to_string(), serde_json::json!(5)),
            ("/missing".to_string(), serde_json::json!("x")),
        ]);
        let record = run(r#"{"status":"ok","count":5}"#, &[], true);
        let grades = ResponseJsonGrader { pointers }
            .grade(&record, &dummy_ctx())
            .await;
        assert!(find(&grades, "response_json(\"/status\")").passed);
        assert!(find(&grades, "response_json(\"/count\")").passed);
        assert!(!find(&grades, "response_json(\"/missing\")").passed);
        assert!(grades.iter().all(|g| g.category == GradeCategory::Response));
    }

    #[test]
    fn grade_category_as_str_matches_serde() {
        // as_str() (the category_totals key) and the serde snake_case (the
        // grade.category value) must stay in lockstep so report consumers can
        // join per-grade categories against category_totals. `Config` belongs
        // in this list like every other variant: it is the category the
        // fail-closed backstop emits, so a consumer that cannot join it would
        // lose exactly the grade that says the case asserted nothing.
        let all = [
            GradeCategory::Response,
            GradeCategory::Tool,
            GradeCategory::SideEffect,
            GradeCategory::Budget,
            GradeCategory::Judge,
            GradeCategory::Config,
        ];
        for cat in all {
            // A new variant makes this match non-exhaustive, so the compiler
            // stops here and the added arm is the prompt to list it in `all`.
            match cat {
                GradeCategory::Response
                | GradeCategory::Tool
                | GradeCategory::SideEffect
                | GradeCategory::Budget
                | GradeCategory::Judge
                | GradeCategory::Config => {}
            }
            let serde_label = serde_json::to_value(cat).unwrap();
            assert_eq!(serde_label.as_str(), Some(cat.as_str()));
        }
    }

    #[tokio::test]
    async fn response_json_fenced_block_fallback() {
        let pointers = BTreeMap::from([("/ok".to_string(), serde_json::json!(true))]);
        let fenced = "Here is the result:\n```json\n{\"ok\": true}\n```\nDone.";
        let grades = ResponseJsonGrader {
            pointers: pointers.clone(),
        }
        .grade(&run(fenced, &[], true), &dummy_ctx())
        .await;
        assert!(grades[0].passed, "fenced json block must be parsed");

        let bad = ResponseJsonGrader { pointers }
            .grade(&run("not json at all", &[], true), &dummy_ctx())
            .await;
        assert!(!bad[0].passed);
        assert_eq!(bad[0].detail, "response is not JSON");
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
        assert_eq!(arg_grade.category, GradeCategory::Tool);
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
