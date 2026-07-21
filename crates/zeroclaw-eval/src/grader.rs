//! Grading: non-panicking checks over a [`RunRecord`].

use crate::case::{
    BudgetExpects, MemoryExpects, TraceExpects, WorkspaceExpects, validate_workspace_rel_path,
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
        }
    }
}

/// Context available to graders while the case's workspace still exists.
pub struct GradeContext<'a> {
    pub workspace: &'a std::path::Path,
    pub memory: Option<&'a dyn zeroclaw_memory::Memory>,
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

/// Grades end-state memory entries. Every key is validated before the memory
/// backend is accessed, so a case cannot use grading to escape its namespace.
pub struct MemoryGrader {
    pub expects: MemoryExpects,
}

#[async_trait::async_trait]
impl Grader for MemoryGrader {
    fn name(&self) -> &str {
        "memory"
    }

    async fn grade(&self, _run: &RunRecord, ctx: &GradeContext<'_>) -> Vec<GradeResult> {
        let mut out = Vec::new();

        for key in &self.expects.present {
            let check = format!("memory_present({key:?})");
            if validate_workspace_rel_path(key).is_err() {
                out.push(GradeResult::new(
                    check,
                    false,
                    "key escapes workspace",
                    GradeCategory::SideEffect,
                ));
                continue;
            }
            let Some(memory) = ctx.memory else {
                out.push(GradeResult::new(
                    check,
                    false,
                    "no memory backend for this run",
                    GradeCategory::SideEffect,
                ));
                continue;
            };
            match memory.get(key).await {
                Ok(entry) => {
                    let present = entry.is_some();
                    out.push(GradeResult::new(
                        check,
                        present,
                        if present { "present" } else { "missing" },
                        GradeCategory::SideEffect,
                    ));
                }
                Err(error) => out.push(GradeResult::new(
                    check,
                    false,
                    format!("memory backend error: {error}"),
                    GradeCategory::SideEffect,
                )),
            }
        }

        for key in &self.expects.absent {
            let check = format!("memory_absent({key:?})");
            if validate_workspace_rel_path(key).is_err() {
                out.push(GradeResult::new(
                    check,
                    false,
                    "key escapes workspace",
                    GradeCategory::SideEffect,
                ));
                continue;
            }
            let Some(memory) = ctx.memory else {
                out.push(GradeResult::new(
                    check,
                    false,
                    "no memory backend for this run",
                    GradeCategory::SideEffect,
                ));
                continue;
            };
            match memory.get(key).await {
                Ok(entry) => {
                    let absent = entry.is_none();
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
                Err(error) => out.push(GradeResult::new(
                    check,
                    false,
                    format!("memory backend error: {error}"),
                    GradeCategory::SideEffect,
                )),
            }
        }

        for (key, needles) in &self.expects.contains {
            if validate_workspace_rel_path(key).is_err() {
                if needles.is_empty() {
                    out.push(GradeResult::new(
                        format!("memory_contains({key:?})"),
                        false,
                        "key escapes workspace",
                        GradeCategory::SideEffect,
                    ));
                }
                for needle in needles {
                    out.push(GradeResult::new(
                        format!("memory_contains({key:?}, {needle:?})"),
                        false,
                        "key escapes workspace",
                        GradeCategory::SideEffect,
                    ));
                }
                continue;
            }
            if needles.is_empty() {
                out.push(GradeResult::new(
                    format!("memory_contains({key:?})"),
                    false,
                    "contains requires at least one substring",
                    GradeCategory::SideEffect,
                ));
                continue;
            }
            let Some(memory) = ctx.memory else {
                for needle in needles {
                    out.push(GradeResult::new(
                        format!("memory_contains({key:?}, {needle:?})"),
                        false,
                        "no memory backend for this run",
                        GradeCategory::SideEffect,
                    ));
                }
                continue;
            };
            match memory.get(key).await {
                Ok(entry) => {
                    for needle in needles {
                        let check = format!("memory_contains({key:?}, {needle:?})");
                        match &entry {
                            Some(entry) => {
                                let found = entry.content.contains(needle);
                                out.push(GradeResult::new(
                                    check,
                                    found,
                                    if found {
                                        "found"
                                    } else {
                                        "not found in memory"
                                    },
                                    GradeCategory::SideEffect,
                                ));
                            }
                            None => out.push(GradeResult::new(
                                check,
                                false,
                                "memory missing",
                                GradeCategory::SideEffect,
                            )),
                        }
                    }
                }
                Err(error) => {
                    for needle in needles {
                        out.push(GradeResult::new(
                            format!("memory_contains({key:?}, {needle:?})"),
                            false,
                            format!("memory backend error: {error}"),
                            GradeCategory::SideEffect,
                        ));
                    }
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
/// [`WorkspaceGrader`], [`MemoryGrader`], [`BudgetGrader`], and
/// [`ResponseJsonGrader`] when the case declares the matching `expects` fields.
pub async fn grade_run(
    trace: &crate::case::LlmTrace,
    record: &RunRecord,
    workspace: &std::path::Path,
    memory: Option<&dyn zeroclaw_memory::Memory>,
) -> Vec<GradeResult> {
    let ctx = GradeContext { workspace, memory };
    let expects = &trace.expects;
    let mut graders: Vec<Box<dyn Grader>> = vec![Box::new(ExpectationsGrader {
        expects: expects.clone(),
    })];
    if let Some(workspace) = &expects.workspace {
        graders.push(Box::new(WorkspaceGrader {
            expects: workspace.clone(),
        }));
    }
    if let Some(memory) = &expects.memory {
        graders.push(Box::new(MemoryGrader {
            expects: memory.clone(),
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
    use zeroclaw_memory::{Memory, MemoryCategory, MemoryEntry, SqliteMemory};

    struct PanicGetMemory;

    impl zeroclaw_api::attribution::Attributable for PanicGetMemory {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Memory(zeroclaw_api::attribution::MemoryKind::InMemory)
        }

        fn alias(&self) -> &str {
            "panic-get"
        }
    }

    #[async_trait::async_trait]
    impl Memory for PanicGetMemory {
        fn name(&self) -> &str {
            "panic-get"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            panic!("invalid key reached memory backend: {key}")
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
    }

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
            .grade(
                &record,
                &GradeContext {
                    workspace: &path,
                    memory: None,
                },
            )
            .await;
        assert!(grades[0].passed, "workspace must exist during grading");

        // Control: once the workspace drops, the same probe fails on the same path,
        // so the assertion above is not vacuously true.
        drop(tmp);
        let after = Probe
            .grade(
                &record,
                &GradeContext {
                    workspace: &path,
                    memory: None,
                },
            )
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
            duration_ms: 0,
            llm_calls: 0,
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
            memory: None,
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
                    memory: None,
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
                    memory: None,
                },
            )
            .await;
        assert_eq!(grades.len(), 3);
        assert!(grades.iter().all(|g| !g.passed));
        assert!(grades.iter().all(|g| g.detail == "path escapes workspace"));
    }

    #[tokio::test]
    async fn memory_grader_checks_present_absent_and_contains_with_sqlite() {
        let tmp = tempfile::tempdir().unwrap();
        let memory = SqliteMemory::new("grader-test", tmp.path()).unwrap();
        memory
            .store(
                "project/role",
                "zeroclaw_operator",
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();

        let expects = MemoryExpects {
            present: vec!["project/role".into(), "missing".into()],
            absent: vec!["missing".into(), "project/role".into()],
            contains: BTreeMap::from([
                (
                    "project/role".into(),
                    vec!["zeroclaw".into(), "missing_marker".into()],
                ),
                ("missing".into(), vec!["anything".into()]),
            ]),
        };
        let grades = MemoryGrader { expects }
            .grade(
                &run("", &[], true),
                &GradeContext {
                    workspace: tmp.path(),
                    memory: Some(&memory),
                },
            )
            .await;

        assert!(find(&grades, r#"memory_present("project/role")"#).passed);
        assert!(!find(&grades, r#"memory_present("missing")"#).passed);
        assert!(find(&grades, r#"memory_absent("missing")"#).passed);
        assert!(!find(&grades, r#"memory_absent("project/role")"#).passed);
        assert!(find(&grades, r#"memory_contains("project/role", "zeroclaw")"#).passed);
        assert!(
            !find(
                &grades,
                r#"memory_contains("project/role", "missing_marker")"#
            )
            .passed
        );
        assert!(!find(&grades, r#"memory_contains("missing", "anything")"#).passed);
        assert_eq!(grades.len(), 7);
        assert!(
            grades
                .iter()
                .all(|grade| grade.category == GradeCategory::SideEffect)
        );
    }

    #[tokio::test]
    async fn memory_grader_rejects_invalid_keys_before_backend_access() {
        let tmp = tempfile::tempdir().unwrap();
        let memory = PanicGetMemory;
        let expects = MemoryExpects {
            present: vec!["../escape".into()],
            absent: vec!["/absolute".into()],
            contains: BTreeMap::from([
                ("nested/../../escape".into(), vec!["x".into()]),
                ("../empty".into(), Vec::new()),
            ]),
        };
        let grades = MemoryGrader { expects }
            .grade(
                &run("", &[], true),
                &GradeContext {
                    workspace: tmp.path(),
                    memory: Some(&memory),
                },
            )
            .await;

        assert_eq!(grades.len(), 4);
        assert!(grades.iter().all(|grade| !grade.passed));
        assert!(
            grades
                .iter()
                .all(|grade| grade.detail == "key escapes workspace")
        );
        assert_eq!(grades[0].check, r#"memory_present("../escape")"#);
        assert_eq!(grades[1].check, r#"memory_absent("/absolute")"#);
        assert_eq!(
            find(&grades, r#"memory_contains("nested/../../escape", "x")"#).detail,
            "key escapes workspace"
        );
        assert_eq!(
            find(&grades, r#"memory_contains("../empty")"#).detail,
            "key escapes workspace"
        );
    }

    #[tokio::test]
    async fn memory_grader_without_backend_fails_every_declared_check() {
        let expects = MemoryExpects {
            present: vec!["present".into()],
            absent: vec!["absent".into()],
            contains: BTreeMap::from([("contains".into(), vec!["needle".into()])]),
        };
        let grades = MemoryGrader { expects }
            .grade(&run("", &[], true), &dummy_ctx())
            .await;

        assert_eq!(grades.len(), 3);
        assert!(grades.iter().all(|grade| !grade.passed));
        assert!(
            grades
                .iter()
                .all(|grade| grade.detail == "no memory backend for this run")
        );
        assert!(
            grades
                .iter()
                .all(|grade| grade.category == GradeCategory::SideEffect)
        );
    }

    #[tokio::test]
    async fn memory_grader_rejects_empty_contains_lists_without_backend_access() {
        let expects = MemoryExpects {
            contains: BTreeMap::from([("profile/role".into(), Vec::new())]),
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let with_backend = MemoryGrader {
            expects: expects.clone(),
        }
        .grade(
            &run("", &[], true),
            &GradeContext {
                workspace: tmp.path(),
                memory: Some(&PanicGetMemory),
            },
        )
        .await;
        let without_backend = MemoryGrader { expects }
            .grade(&run("", &[], true), &dummy_ctx())
            .await;

        for grades in [&with_backend, &without_backend] {
            assert_eq!(grades.len(), 1);
            assert_eq!(grades[0].check, r#"memory_contains("profile/role")"#);
            assert!(!grades[0].passed);
            assert_eq!(grades[0].detail, "contains requires at least one substring");
            assert_eq!(grades[0].category, GradeCategory::SideEffect);
        }
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
