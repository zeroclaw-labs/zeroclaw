//! The evaluation case format — JSON trace fixtures for deterministic replay.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A complete LLM conversation trace loaded from a JSON fixture.
///
/// `deny_unknown_fields`: a fixture key the harness does not understand is a
/// typo, and a typo must be a load error rather than a silently-ignored key.
/// Without it, `respose_contains` (say) parses into an empty expectation block
/// and the case scores green while asserting nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmTrace {
    /// Identifier for the trace (surfaced in reports).
    pub model_name: String,
    /// Optional stable report identity. When set, reports and receipts use this
    /// instead of `model_name`; readers should go through [`LlmTrace::display_id`].
    #[serde(default)]
    pub id: Option<String>,
    /// Conversation turns, replayed in order.
    pub turns: Vec<TraceTurn>,
    /// Declarative expectations graded against the run.
    #[serde(default)]
    pub expects: TraceExpects,
    /// Pre-run environment preparation for the case (live mode).
    #[serde(default)]
    pub setup: Option<CaseSetup>,
    /// Tool names this case requests. Live mode only; ignored in replay.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// Explicit smoke-case contract: this case deliberately declares no
    /// effective expectation. Without it, a no-op expectation block is a load
    /// error (see [`LlmTrace::from_file`]), so "asserts nothing" is always a
    /// declared choice rather than an accident.
    #[serde(default)]
    pub allow_no_expectations: bool,
}

/// Pre-run environment preparation for a case.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseSetup {
    /// Files written into the case's temp workspace before the run.
    /// Keys are workspace-relative paths; absolute paths and `..` are rejected.
    #[serde(default)]
    pub workspace_files: std::collections::BTreeMap<String, String>,
}

/// A single conversation turn (user input + scripted LLM response steps).
///
/// `steps` is optional: replay cases script every LLM round-trip, while live
/// cases must omit them (the real provider produces the responses).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceTurn {
    pub user_input: String,
    #[serde(default)]
    pub steps: Option<Vec<TraceStep>>,
}

/// A single LLM response step within a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceStep {
    pub response: TraceResponse,
}

/// The response content for one step — either plain text or tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum TraceResponse {
    #[serde(rename = "text")]
    Text {
        content: String,
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
    },
    #[serde(rename = "tool_calls")]
    ToolCalls {
        tool_calls: Vec<TraceToolCall>,
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
    },
}

/// A tool call within a trace response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Declarative expectations for grading a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceExpects {
    /// Substrings the final response must contain.
    #[serde(default)]
    pub response_contains: Vec<String>,
    /// Substrings the final response must NOT contain.
    #[serde(default)]
    pub response_not_contains: Vec<String>,
    /// Tool names that must have been called.
    #[serde(default)]
    pub tools_used: Vec<String>,
    /// Tool names that must NOT have been called.
    #[serde(default)]
    pub tools_not_used: Vec<String>,
    /// Upper bound on the number of tool calls.
    #[serde(default)]
    pub max_tool_calls: Option<usize>,
    /// If set, whether every tool call must have succeeded.
    #[serde(default)]
    pub all_tools_succeeded: Option<bool>,
    /// Regex patterns the final response must match.
    #[serde(default)]
    pub response_matches: Vec<String>,
}

impl TraceExpects {
    /// True when nothing in this block can ever produce a [`GradeResult`]:
    /// every list is empty and every optional bound is unset, so
    /// `evaluate_expects` returns `vec![]` no matter what the run did.
    ///
    /// [`GradeResult`]: crate::grader::GradeResult
    pub fn is_no_op(&self) -> bool {
        self.response_contains.is_empty()
            && self.response_not_contains.is_empty()
            && self.tools_used.is_empty()
            && self.tools_not_used.is_empty()
            && self.response_matches.is_empty()
            && self.max_tool_calls.is_none()
            && self.all_tools_succeeded.is_none()
    }
}

impl LlmTrace {
    /// The identity used in reports and receipts: the explicit `id` when set,
    /// otherwise `model_name`.
    pub fn display_id(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.model_name)
    }

    /// Reject a fixture whose expectations can never produce a grade, unless it
    /// opts in with `allow_no_expectations`.
    ///
    /// This is the second half of the no-op-fixture guard: `deny_unknown_fields`
    /// catches misspelled keys, and this catches a fixture that spelled
    /// everything correctly but declared nothing (`"expects": {}`, or `expects`
    /// omitted entirely). Without it such a case runs, grades nothing, and
    /// aggregates to green — a CI gate reporting success precisely where it
    /// should be catching a regression.
    fn ensure_effective_expectations(&self, path: &Path) -> anyhow::Result<()> {
        if self.expects.is_no_op() && !self.allow_no_expectations {
            anyhow::bail!(
                "eval fixture {} declares no effective expectation; \
                 add an assertion or set \"allow_no_expectations\": true",
                path.display()
            );
        }
        Ok(())
    }

    /// Load a trace from a JSON file.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading trace fixture {}", path.display()))?;
        let trace: LlmTrace = serde_json::from_str(&content)
            .with_context(|| format!("parsing trace fixture {}", path.display()))?;
        trace.ensure_effective_expectations(path)?;
        Ok(trace)
    }
}

/// Validate that `path` is a safe workspace-relative path: non-empty, not absolute,
/// and free of any `..` component. Used before writing setup files or grading
/// workspace paths, so a case cannot read or write outside its sandbox.
pub fn validate_workspace_rel_path(path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        anyhow::bail!("workspace path must not be empty");
    }
    for component in Path::new(path).components() {
        match component {
            std::path::Component::ParentDir => {
                anyhow::bail!("workspace path {path:?} must not contain a `..` component");
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                anyhow::bail!("workspace path {path:?} must be relative, not absolute");
            }
            std::path::Component::CurDir | std::path::Component::Normal(_) => {}
        }
    }
    Ok(())
}

/// Load every `*.json` trace fixture in `dir`, sorted by path for stable ordering.
pub fn load_suite(dir: &Path) -> anyhow::Result<Vec<(PathBuf, LlmTrace)>> {
    let read = std::fs::read_dir(dir)
        .with_context(|| format!("reading eval suite directory {}", dir.display()))?;

    let mut paths: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let trace = LlmTrace::from_file(&path)?;
        out.push((path, trace));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_response_text_variant_defaults_tokens_to_zero() {
        let r: TraceResponse = serde_json::from_str(r#"{"type":"text","content":"hi"}"#).unwrap();
        match r {
            TraceResponse::Text {
                content,
                input_tokens,
                output_tokens,
            } => {
                assert_eq!(content, "hi");
                assert_eq!(input_tokens, 0);
                assert_eq!(output_tokens, 0);
            }
            _ => panic!("expected Text variant"),
        }
    }

    #[test]
    fn trace_response_tool_calls_variant_parses() {
        let j = r#"{"type":"tool_calls","tool_calls":[{"id":"1","name":"search","arguments":{"q":"x"}}],"input_tokens":5}"#;
        let r: TraceResponse = serde_json::from_str(j).unwrap();
        match r {
            TraceResponse::ToolCalls {
                tool_calls,
                input_tokens,
                output_tokens,
            } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "1");
                assert_eq!(tool_calls[0].name, "search");
                assert_eq!(input_tokens, 5);
                assert_eq!(output_tokens, 0);
            }
            _ => panic!("expected ToolCalls variant"),
        }
    }

    #[test]
    fn llm_trace_uses_default_expects_when_omitted() {
        let t: LlmTrace = serde_json::from_str(r#"{"model_name":"m","turns":[]}"#).unwrap();
        assert_eq!(t.model_name, "m");
        assert!(t.turns.is_empty());
        assert!(t.expects.response_contains.is_empty());
        assert!(t.expects.max_tool_calls.is_none());
        // Deserialization stays permissive — the no-op guard lives in
        // `from_file`, so in-memory construction (tests, generators) is
        // unaffected while every on-disk fixture is checked.
        assert!(t.expects.is_no_op());
    }

    /// Write `json` to a uniquely named temp fixture and load it through the
    /// real loader, so these tests exercise `from_file`'s guard rather than a
    /// bare `serde_json::from_str`.
    fn load_fixture(name: &str, json: &str) -> anyhow::Result<LlmTrace> {
        let path = std::env::temp_dir().join(format!("zeroclaw_eval_case_{name}.json"));
        std::fs::write(&path, json).unwrap();
        let out = LlmTrace::from_file(&path);
        let _ = std::fs::remove_file(&path);
        out
    }

    #[test]
    fn from_file_reads_and_parses_trace() {
        let t = load_fixture(
            "from_file_ok",
            r#"{"model_name":"demo","turns":[],"expects":{"response_contains":["hi"]}}"#,
        )
        .unwrap();
        assert_eq!(t.model_name, "demo");
    }

    #[test]
    fn fixture_with_misspelled_expectation_key_is_rejected() {
        // `respose_contains` is a typo for `response_contains`. Before
        // `deny_unknown_fields` this parsed into an empty expectation block and
        // the case scored green while asserting nothing.
        let err = load_fixture(
            "typo_key",
            r#"{"model_name":"m","turns":[],"expects":{"respose_contains":["hi"]}}"#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("respose_contains"),
            "error must name the offending key: {msg}"
        );
    }

    #[test]
    fn fixture_with_misspelled_top_level_key_is_rejected() {
        let err = load_fixture(
            "typo_top_level",
            r#"{"model_name":"m","turns":[],"expct":{"response_contains":["hi"]}}"#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("expct"), "error must name the typo: {msg}");
    }

    #[test]
    fn fixture_with_empty_expectations_is_rejected() {
        // Both spellings of "asserts nothing" must fail: an explicit empty block…
        let err = load_fixture(
            "empty_expects",
            r#"{"model_name":"m","turns":[],"expects":{}}"#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("no effective expectation"),
            "unexpected error: {err}"
        );
        // …and an omitted `expects` key entirely.
        let err = load_fixture("omitted_expects", r#"{"model_name":"m","turns":[]}"#).unwrap_err();
        assert!(
            err.to_string().contains("no effective expectation"),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains("allow_no_expectations"),
            "error must name the opt-out so the fix is obvious: {err}"
        );
    }

    #[test]
    fn explicit_smoke_case_is_accepted() {
        // A case that genuinely asserts nothing is allowed, but only when it
        // says so — the contract is declared, not stumbled into.
        let t = load_fixture(
            "smoke_optin",
            r#"{"model_name":"m","turns":[],"expects":{},"allow_no_expectations":true}"#,
        )
        .unwrap();
        assert!(t.allow_no_expectations);
        assert!(t.expects.is_no_op());
    }

    #[test]
    fn any_single_expectation_makes_a_block_effective() {
        // `is_no_op` must be false as soon as *any* field can produce a grade,
        // so the guard never rejects a fixture that does assert something.
        let cases = [
            r#"{"response_contains":["x"]}"#,
            r#"{"response_not_contains":["x"]}"#,
            r#"{"tools_used":["echo"]}"#,
            r#"{"tools_not_used":["echo"]}"#,
            r#"{"response_matches":["^x$"]}"#,
            r#"{"max_tool_calls":0}"#,
            r#"{"all_tools_succeeded":true}"#,
        ];
        for c in cases {
            let e: TraceExpects = serde_json::from_str(c).unwrap();
            assert!(!e.is_no_op(), "must be effective: {c}");
        }
        assert!(TraceExpects::default().is_no_op());
    }

    #[test]
    fn shipped_regression_fixtures_load_under_the_stricter_loader() {
        // Every fixture the CI gate runs must survive `deny_unknown_fields` and
        // the no-op check; otherwise this PR's own gate cannot start.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/regression");
        let suite = load_suite(&dir).expect("shipped regression fixtures must load");
        assert!(!suite.is_empty(), "regression suite must not be empty");
        for (path, trace) in &suite {
            assert!(
                !trace.expects.is_no_op() || trace.allow_no_expectations,
                "{} asserts nothing without opting in",
                path.display()
            );
        }
    }

    #[test]
    fn display_id_prefers_id_then_falls_back_to_model_name() {
        let with_id: LlmTrace =
            serde_json::from_str(r#"{"model_name":"m","id":"case-7","turns":[]}"#).unwrap();
        assert_eq!(with_id.display_id(), "case-7");
        let without_id: LlmTrace =
            serde_json::from_str(r#"{"model_name":"m","turns":[]}"#).unwrap();
        assert_eq!(without_id.display_id(), "m");
    }

    #[test]
    fn turn_steps_default_to_none_when_omitted() {
        let t: LlmTrace =
            serde_json::from_str(r#"{"model_name":"m","turns":[{"user_input":"hi"}]}"#).unwrap();
        assert!(t.turns[0].steps.is_none());
    }

    #[test]
    fn validate_workspace_rel_path_rejects_absolute() {
        assert!(validate_workspace_rel_path("/etc/passwd").is_err());
    }

    #[test]
    fn validate_workspace_rel_path_rejects_empty() {
        assert!(validate_workspace_rel_path("").is_err());
    }

    #[test]
    fn validate_workspace_rel_path_rejects_parent_component() {
        assert!(validate_workspace_rel_path("../secret").is_err());
        assert!(validate_workspace_rel_path("sub/../../secret").is_err());
    }

    #[test]
    fn validate_workspace_rel_path_accepts_nested_relative() {
        assert!(validate_workspace_rel_path("sub/dir/file.txt").is_ok());
    }

    #[test]
    fn load_suite_filters_json_and_sorts_by_path() {
        let dir = std::env::temp_dir().join("zeroclaw_eval_case_suite_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let effective = r#"{"model_name":"NAME","turns":[],"expects":{"max_tool_calls":0}}"#;
        std::fs::write(dir.join("b.json"), effective.replace("NAME", "b")).unwrap();
        std::fs::write(dir.join("a.json"), effective.replace("NAME", "a")).unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        let suite = load_suite(&dir).unwrap();
        assert_eq!(suite.len(), 2); // the .txt file is ignored
        assert_eq!(suite[0].1.model_name, "a"); // sorted by path
        assert_eq!(suite[1].1.model_name, "b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_suite_fails_closed_on_a_single_no_op_fixture() {
        // One bad fixture fails the whole load rather than being skipped: a
        // suite that silently drops a case is the same silent-green failure
        // mode the loader guard exists to prevent.
        let dir = std::env::temp_dir().join("zeroclaw_eval_case_suite_noop_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("good.json"),
            r#"{"model_name":"good","turns":[],"expects":{"max_tool_calls":0}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("bad.json"), r#"{"model_name":"bad","turns":[]}"#).unwrap();
        let err = load_suite(&dir).unwrap_err();
        assert!(
            err.to_string().contains("no effective expectation"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
