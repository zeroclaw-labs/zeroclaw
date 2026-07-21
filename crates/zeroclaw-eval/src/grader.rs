//! Grading: non-panicking checks over a [`RunRecord`].

use crate::case::{BudgetExpects, TraceExpects, WorkspaceExpects, validate_workspace_rel_path};
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
    /// When true, this grade is informational and never fails the case (e.g. an
    /// uncalibrated or ungated judge dimension). Defaults to false.
    #[serde(default)]
    pub diagnostic: bool,
}

impl GradeResult {
    fn new(
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
            diagnostic: false,
        }
    }

    /// Mark this grade diagnostic (never gates the case).
    fn diagnostic(mut self) -> Self {
        self.diagnostic = true;
        self
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
                run.input_tokens + run.output_tokens,
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

/// System prompt for the LLM judge. One dimension of one run against one rubric.
pub const JUDGE_SYSTEM: &str =
    "You are an evaluation judge for an AI agent harness. You grade one dimension
of one agent run against one rubric. Think through the evidence first, then
answer with ONLY a JSON object on the final line, no other text after it:
{\"score\": <float 0.0-1.0>, \"unknown\": <bool>, \"reason\": \"<one sentence>\"}
Set \"unknown\": true when the transcript lacks the evidence to judge the
rubric; never guess. Scores: 1.0 fully satisfies the rubric, 0.0 clearly
violates it.";

/// Everything the judge grader needs beyond the rubrics: a shared judge provider,
/// its resolved model, and whether judge grades gate (judge_gate + calibration).
#[derive(Clone)]
pub struct JudgeDeps {
    pub provider: std::sync::Arc<dyn zeroclaw_api::model_provider::ModelProvider>,
    pub model: String,
    pub judge_ref: String,
    pub gates: bool,
    /// Canonical per-suite collection of judge results eligible for calibration.
    pub records_sink: std::sync::Arc<std::sync::Mutex<Vec<crate::calibration::JudgeRunRecord>>>,
}

/// Grades per-dimension LLM-judge rubrics with one isolated judge call each.
pub struct JudgeGrader {
    pub rubrics: Vec<crate::case::JudgeRubric>,
    pub task_turns: Vec<String>,
    pub deps: JudgeDeps,
}

/// Render a transcript from the run history: one line per message.
fn render_transcript(history: &[zeroclaw_api::model_provider::ConversationMessage]) -> String {
    let mut out = String::new();
    for msg in history {
        let line = format!("{msg:?}");
        let truncated: String = line.chars().take(500).collect();
        out.push_str(&truncated);
        out.push('\n');
    }
    out
}

/// Build the judge user message for one rubric (never includes the case's
/// `expects`, which would leak the answer key).
fn judge_message(
    task_turns: &[String],
    final_response: &str,
    history: &[zeroclaw_api::model_provider::ConversationMessage],
    rubric: &crate::case::JudgeRubric,
) -> String {
    let mut tasks = String::new();
    for (i, t) in task_turns.iter().enumerate() {
        tasks.push_str(&format!("{}. {t}\n", i + 1));
    }
    let mut msg = format!(
        "## Task given to the agent\n{tasks}\n## Agent's final response\n{final_response}\n"
    );
    if rubric.include_transcript {
        msg.push_str(&format!(
            "\n## Transcript (tool calls and results)\n{}",
            render_transcript(history)
        ));
    }
    msg.push_str(&format!("\n## Rubric: {}\n{}", rubric.name, rubric.rubric));
    msg
}

/// Parse the judge reply: the LAST line that is a JSON object with a numeric
/// `score`. Returns `(score_clamped, unknown, reason)`, or `None` if malformed.
fn parse_judge_reply(reply: &str) -> Option<(f64, bool, String)> {
    for line in reply.lines().rev() {
        let line = line.trim();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        let Some(score) = obj.get("score").and_then(serde_json::Value::as_f64) else {
            continue;
        };
        let unknown = obj
            .get("unknown")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let reason = obj
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        return Some((score.clamp(0.0, 1.0), unknown, reason));
    }
    None
}

#[async_trait::async_trait]
impl Grader for JudgeGrader {
    fn name(&self) -> &str {
        "judge"
    }

    async fn grade(&self, run: &RunRecord, _ctx: &GradeContext<'_>) -> Vec<GradeResult> {
        let mut out = Vec::new();
        for rubric in &self.rubrics {
            let message =
                judge_message(&self.task_turns, &run.final_response, &run.history, rubric);
            let check = format!("judge:{}", rubric.name);
            // Temperature is explicitly 0.0; a transport error is diagnostic, never
            // a case failure.
            let reply = self
                .deps
                .provider
                .chat_with_system(Some(JUDGE_SYSTEM), &message, &self.deps.model, Some(0.0))
                .await;
            let grade = match reply {
                Err(e) => GradeResult::new(
                    check,
                    true,
                    format!("UNKNOWN (diagnostic): transport error: {e}"),
                    GradeCategory::Judge,
                )
                .diagnostic(),
                Ok(text) => match parse_judge_reply(&text) {
                    None => GradeResult::new(
                        check,
                        true,
                        "UNKNOWN (diagnostic): judge output was not parseable JSON",
                        GradeCategory::Judge,
                    )
                    .diagnostic(),
                    Some((_, true, reason)) => GradeResult::new(
                        check,
                        true,
                        format!("UNKNOWN (diagnostic): {reason}"),
                        GradeCategory::Judge,
                    )
                    .diagnostic(),
                    Some((score, false, reason)) => {
                        let record = crate::calibration::JudgeRunRecord::new(
                            crate::calibration::JudgeRunRecordInput {
                                judge_ref: self.deps.judge_ref.clone(),
                                case_id: run.case_id.clone(),
                                case_hash: run.case_hash.clone(),
                                rubric_name: rubric.name.clone(),
                                rubric_text: rubric.rubric.clone(),
                                threshold: rubric.threshold,
                                task_turns: self.task_turns.clone(),
                                final_response: run.final_response.clone(),
                                score,
                                reason: reason.clone(),
                            },
                        );
                        let mut records = match self.deps.records_sink.lock() {
                            Ok(records) => records,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        records.push(record);

                        let passed = score >= rubric.threshold;
                        let detail = if passed {
                            format!("score={score:.2}")
                        } else {
                            format!("score={score:.2} reason={reason}")
                        };
                        let mut g = GradeResult::new(check, passed, detail, GradeCategory::Judge);
                        // Diagnostic (never gates) unless the judge is gated+calibrated.
                        if !self.deps.gates {
                            g = g.diagnostic();
                        }
                        g
                    }
                },
            };
            out.push(grade);
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
        let parsed = parse_response_json(&run.final_response);
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

/// Build the case's graders and run them while the workspace is alive, returning
/// every grade concatenated. Always runs [`ExpectationsGrader`], plus a
/// [`WorkspaceGrader`], [`BudgetGrader`], and [`ResponseJsonGrader`] when the
/// case declares the matching `expects` fields.
pub async fn grade_run(
    trace: &crate::case::LlmTrace,
    record: &RunRecord,
    workspace: &std::path::Path,
    judge: Option<&JudgeDeps>,
) -> Vec<GradeResult> {
    let ctx = GradeContext { workspace };
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
    // A judge grader runs one isolated call per rubric, but only when the case
    // declares rubrics AND a judge provider is configured.
    if let Some(deps) = judge.filter(|_| !expects.judge.is_empty()) {
        graders.push(Box::new(JudgeGrader {
            rubrics: expects.judge.clone(),
            task_turns: trace.turns.iter().map(|t| t.user_input.clone()).collect(),
            deps: deps.clone(),
        }));
    }
    let mut grades = Vec::new();
    for grader in &graders {
        grades.extend(grader.grade(record, &ctx).await);
    }
    grades
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
            schema: crate::record::RECORD_SCHEMA.to_string(),
            mode: crate::Mode::Replay,
            case_id: "test".to_string(),
            case_hash: "case-hash".to_string(),
            provider_ref: "scripted".to_string(),
            tool_surface: Vec::new(),
            sandbox: crate::record::SandboxStamp {
                autonomy: "supervised".to_string(),
                workspace_only: false,
            },
            final_response: resp.to_string(),
            history: Vec::new(),
            tools_called: tools.iter().map(|s| s.to_string()).collect(),
            all_tools_succeeded: all_ok,
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: 0,
            llm_calls: 0,
            judge_ref: None,
            judge_usage: None,
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
        record.input_tokens = 100;
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
        // join per-grade categories against category_totals.
        for cat in [
            GradeCategory::Response,
            GradeCategory::Tool,
            GradeCategory::SideEffect,
            GradeCategory::Budget,
            GradeCategory::Judge,
        ] {
            let serde_label = serde_json::to_value(cat).unwrap();
            assert_eq!(serde_label.as_str(), Some(cat.as_str()));
        }
    }

    fn judge_provider(
        replies: &[&str],
    ) -> std::sync::Arc<dyn zeroclaw_api::model_provider::ModelProvider> {
        let steps: Vec<String> = replies
            .iter()
            .map(|r| {
                format!(
                    r#"{{"response":{{"type":"text","content":{}}}}}"#,
                    serde_json::to_string(r).unwrap()
                )
            })
            .collect();
        let json = format!(
            r#"{{"model_name":"j","turns":[{{"user_input":"","steps":[{}]}}]}}"#,
            steps.join(",")
        );
        let trace: crate::case::LlmTrace = serde_json::from_str(&json).unwrap();
        std::sync::Arc::new(crate::replay::TraceLlmProvider::try_from_trace(&trace).unwrap())
    }

    fn rubric(name: &str, threshold: f64) -> crate::case::JudgeRubric {
        crate::case::JudgeRubric {
            name: name.to_string(),
            rubric: "grade it".to_string(),
            threshold,
            include_transcript: false,
        }
    }

    async fn judge_grade(
        replies: &[&str],
        rubrics: Vec<crate::case::JudgeRubric>,
        gates: bool,
    ) -> Vec<GradeResult> {
        judge_grade_with_records(replies, rubrics, gates).await.0
    }

    async fn judge_grade_with_records(
        replies: &[&str],
        rubrics: Vec<crate::case::JudgeRubric>,
        gates: bool,
    ) -> (Vec<GradeResult>, Vec<crate::calibration::JudgeRunRecord>) {
        let records_sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let deps = JudgeDeps {
            provider: judge_provider(replies),
            model: "m".to_string(),
            judge_ref: "judge.m:x".to_string(),
            gates,
            records_sink: records_sink.clone(),
        };
        let grader = JudgeGrader {
            rubrics,
            task_turns: vec!["do the task".to_string()],
            deps,
        };
        let grades = grader
            .grade(&run("final response", &[], true), &dummy_ctx())
            .await;
        let records = match records_sink.lock() {
            Ok(records) => records.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        (grades, records)
    }

    #[tokio::test]
    async fn judge_passes_at_threshold_boundary() {
        let g = judge_grade(
            &[r#"{"score":0.7,"unknown":false,"reason":"ok"}"#],
            vec![rubric("helpfulness", 0.7)],
            true,
        )
        .await;
        assert_eq!(g[0].check, "judge:helpfulness");
        assert!(g[0].passed, "score == threshold must pass");
    }

    #[tokio::test]
    async fn judge_below_threshold_fails_dimension() {
        let g = judge_grade(
            &[r#"{"score":0.5,"unknown":false,"reason":"weak"}"#],
            vec![rubric("helpfulness", 0.7)],
            true,
        )
        .await;
        assert!(!g[0].passed);
        assert!(g[0].detail.contains("reason=weak"));
    }

    #[tokio::test]
    async fn judge_malformed_json_is_unknown_diagnostic() {
        let g = judge_grade(&["not json at all"], vec![rubric("h", 0.7)], true).await;
        assert!(g[0].passed, "malformed judge output never fails");
        assert!(g[0].diagnostic);
        assert!(g[0].detail.contains("UNKNOWN"));
    }

    #[tokio::test]
    async fn judge_unknown_never_affects_exit() {
        let g = judge_grade(
            &[r#"{"score":0.0,"unknown":true,"reason":"no evidence"}"#],
            vec![rubric("h", 0.7)],
            true,
        )
        .await;
        assert!(g[0].passed, "unknown never fails");
        assert!(g[0].diagnostic);
    }

    #[tokio::test]
    async fn judge_gate_without_calibration_stays_diagnostic() {
        // gates=false models judge_gate off or no calibration file.
        let g = judge_grade(
            &[r#"{"score":0.5,"unknown":false,"reason":"weak"}"#],
            vec![rubric("h", 0.7)],
            false,
        )
        .await;
        assert!(!g[0].passed, "dimension is below threshold");
        assert!(g[0].diagnostic, "but diagnostic, so it does not gate");
    }

    #[tokio::test]
    async fn judge_gate_with_calibration_flips_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("judge.json");
        let calibration = crate::calibration::CalibrationFile {
            schema: crate::calibration::CALIBRATION_SCHEMA.to_string(),
            judge_ref: "judge.m:x".to_string(),
            labeled_records: crate::calibration::MIN_CALIBRATION_RECORDS,
            agreement: 0.9,
            labeler: "tester".to_string(),
            date: "2026-07-21".to_string(),
        };
        std::fs::write(&path, serde_json::to_vec(&calibration).unwrap()).unwrap();
        let gates = crate::calibration::load_calibration(&path, "judge.m:x").is_ok();
        let g = judge_grade(
            &[r#"{"score":0.5,"unknown":false,"reason":"weak"}"#],
            vec![rubric("h", 0.7)],
            gates,
        )
        .await;
        assert!(!g[0].passed);
        assert!(
            !g[0].diagnostic,
            "gated judge grade must count toward the gate"
        );
    }

    #[tokio::test]
    async fn judge_gate_with_invalid_calibration_stays_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("judge.json");
        let calibration = crate::calibration::CalibrationFile {
            schema: crate::calibration::CALIBRATION_SCHEMA.to_string(),
            judge_ref: "judge.m:x".to_string(),
            labeled_records: crate::calibration::MIN_CALIBRATION_RECORDS - 1,
            agreement: 1.0,
            labeler: "tester".to_string(),
            date: "2026-07-21".to_string(),
        };
        std::fs::write(&path, serde_json::to_vec(&calibration).unwrap()).unwrap();
        let gates = crate::calibration::load_calibration(&path, "judge.m:x").is_ok();
        let grades = judge_grade(
            &[r#"{"score":0.5,"unknown":false,"reason":"weak"}"#],
            vec![rubric("h", 0.7)],
            gates,
        )
        .await;
        assert!(!grades[0].passed);
        assert!(
            grades[0].diagnostic,
            "an invalid calibration must not make the judge grade gating"
        );
    }

    #[tokio::test]
    async fn judge_records_sink_captures_only_calibratable_results() {
        let (grades, records) = judge_grade_with_records(
            &[
                r#"{"score":0.83,"unknown":false,"reason":"solid"}"#,
                r#"{"score":0.1,"unknown":true,"reason":"insufficient evidence"}"#,
            ],
            vec![
                rubric("helpfulness", 0.8),
                rubric("unknown", 0.5),
                rubric("transport", 0.5),
            ],
            false,
        )
        .await;

        assert_eq!(grades.len(), 3);
        assert_eq!(
            records.len(),
            1,
            "unknown and transport errors are excluded"
        );
        let record = &records[0];
        assert_eq!(record.schema, crate::calibration::JUDGE_RECORD_SCHEMA);
        assert_eq!(record.judge_ref, "judge.m:x");
        assert_eq!(record.case_id, "test");
        assert_eq!(record.case_hash, "case-hash");
        assert_eq!(record.rubric_name, "helpfulness");
        assert_eq!(record.rubric_text, "grade it");
        assert_eq!(record.threshold, 0.8);
        assert_eq!(record.task_turns, ["do the task"]);
        assert_eq!(record.final_response, "final response");
        assert_eq!(record.score, 0.83);
        assert!(record.judge_pass);
        assert_eq!(record.reason, "solid");
    }

    #[tokio::test]
    async fn per_dimension_isolated_calls() {
        // Two rubrics consume two distinct scripted replies -> two isolated calls.
        let g = judge_grade(
            &[
                r#"{"score":0.9,"unknown":false,"reason":"a"}"#,
                r#"{"score":0.2,"unknown":false,"reason":"b"}"#,
            ],
            vec![rubric("first", 0.5), rubric("second", 0.5)],
            true,
        )
        .await;
        assert_eq!(g.len(), 2);
        assert!(g[0].passed, "first: 0.9 >= 0.5");
        assert!(!g[1].passed, "second: 0.2 < 0.5");
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
}
