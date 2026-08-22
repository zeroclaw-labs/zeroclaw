//! The evaluation case format — JSON trace fixtures for deterministic replay.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A complete LLM conversation trace loaded from a JSON fixture.
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
pub struct TraceStep {
    pub response: TraceResponse,
}

/// The response content for one step — either plain text or tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
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
    /// End-state checks against the case workspace after the run.
    #[serde(default)]
    pub workspace: Option<WorkspaceExpects>,
    /// Resource ceilings for the run.
    #[serde(default)]
    pub budget: Option<BudgetExpects>,
    /// JSON-pointer checks against the final response parsed as JSON.
    #[serde(default)]
    pub response_json: std::collections::BTreeMap<String, serde_json::Value>,
}

/// End-state checks against the case workspace after the run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceExpects {
    /// Workspace-relative paths that must exist as a regular file after the run
    /// (a directory at the path does not satisfy the check).
    #[serde(default)]
    pub file_exists: Vec<String>,
    /// Workspace-relative paths at which nothing (file or directory) may exist
    /// after the run.
    #[serde(default)]
    pub file_absent: Vec<String>,
    /// Path -> substrings that must appear in that file.
    #[serde(default)]
    pub file_contains: std::collections::BTreeMap<String, Vec<String>>,
}

/// Resource ceilings for the run (all optional; each present bound is one
/// inclusive check, `actual <= max`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetExpects {
    /// Max accumulated input tokens reported by the provider.
    #[serde(default)]
    pub max_input_tokens: Option<u64>,
    /// Max accumulated output tokens reported by the provider.
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// Max total tokens (input + output).
    #[serde(default)]
    pub max_total_tokens: Option<u64>,
    /// Max wall-clock duration of the turns loop, in milliseconds.
    #[serde(default)]
    pub max_duration_ms: Option<u64>,
    /// Max number of LLM responses (model round-trips) during the run.
    #[serde(default)]
    pub max_llm_calls: Option<u32>,
}

impl TraceExpects {
    /// Reject expectation blocks that declare nothing enforceable.
    ///
    /// Fail closed: a present-but-empty `workspace`/`budget` block, an entirely
    /// empty `expects`, or an empty `file_contains` needle all grade green while
    /// asserting nothing, which silently converts a required CI gate into
    /// permanent success.
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(workspace) = &self.workspace {
            if workspace.is_vacuous() {
                anyhow::bail!(
                    "expects.workspace is present but declares no checks; \
                     remove the block or add file_exists / file_absent / file_contains entries"
                );
            }
            for (rel, needles) in &workspace.file_contains {
                if needles.is_empty() {
                    anyhow::bail!(
                        "expects.workspace.file_contains[{rel:?}] is an empty list; \
                         remove the entry or add at least one needle"
                    );
                }
                if needles.iter().any(|n| n.is_empty()) {
                    anyhow::bail!(
                        "expects.workspace.file_contains[{rel:?}] contains an empty needle, \
                         which every file trivially satisfies"
                    );
                }
            }
        }
        if let Some(budget) = &self.budget
            && budget.is_vacuous()
        {
            anyhow::bail!(
                "expects.budget is present but declares no bounds; \
                 remove the block or set at least one max_* field"
            );
        }
        if self.is_vacuous() {
            anyhow::bail!(
                "expects declares no effective checks; a case that asserts nothing \
                 always passes and cannot detect a regression"
            );
        }
        Ok(())
    }

    /// True when nothing in this block produces a grade.
    pub fn is_vacuous(&self) -> bool {
        self.response_contains.is_empty()
            && self.response_not_contains.is_empty()
            && self.tools_used.is_empty()
            && self.tools_not_used.is_empty()
            && self.max_tool_calls.is_none()
            && self.all_tools_succeeded.is_none()
            && self.response_matches.is_empty()
            && self.response_json.is_empty()
            && self
                .workspace
                .as_ref()
                .is_none_or(WorkspaceExpects::is_vacuous)
            && self.budget.as_ref().is_none_or(BudgetExpects::is_vacuous)
    }
}

impl WorkspaceExpects {
    /// True when the block declares no workspace check at all.
    pub fn is_vacuous(&self) -> bool {
        self.file_exists.is_empty() && self.file_absent.is_empty() && self.file_contains.is_empty()
    }
}

impl BudgetExpects {
    /// True when the block sets no bound at all.
    pub fn is_vacuous(&self) -> bool {
        self.max_input_tokens.is_none()
            && self.max_output_tokens.is_none()
            && self.max_total_tokens.is_none()
            && self.max_duration_ms.is_none()
            && self.max_llm_calls.is_none()
    }
}

impl LlmTrace {
    /// The identity used in reports and receipts: the explicit `id` when set,
    /// otherwise `model_name`.
    pub fn display_id(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.model_name)
    }

    /// Load a trace from a JSON file.
    ///
    /// Fixture validation is part of loading: unknown keys are rejected by
    /// `deny_unknown_fields`, and [`TraceExpects::validate`] rejects declarations
    /// that would grade green without asserting anything.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading trace fixture {}", path.display()))?;
        let trace: LlmTrace = serde_json::from_str(&content)
            .with_context(|| format!("parsing trace fixture {}", path.display()))?;
        trace
            .expects
            .validate()
            .with_context(|| format!("validating trace fixture {}", path.display()))?;
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

/// Collect the sorted `*.json` paths from an already-opened directory listing.
///
/// Split out of [`load_suite`] so the fail-closed entry handling can be driven
/// directly by a test with an injected entry-level I/O error: a real mid-iteration
/// `readdir(3)` failure cannot be provoked portably.
///
/// Fail closed: a dropped entry silently shrinks the suite that CI then
/// certifies as green, so any entry error aborts discovery with suite context.
fn collect_suite_paths<I>(dir: &Path, entries: I) -> anyhow::Result<Vec<PathBuf>>
where
    I: IntoIterator<Item = std::io::Result<PathBuf>>,
{
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let path = entry.with_context(|| {
            format!("reading an entry of eval suite directory {}", dir.display())
        })?;
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Load every `*.json` trace fixture in `dir`, sorted by path for stable ordering.
pub fn load_suite(dir: &Path) -> anyhow::Result<Vec<(PathBuf, LlmTrace)>> {
    let read = std::fs::read_dir(dir)
        .with_context(|| format!("reading eval suite directory {}", dir.display()))?;

    let paths = collect_suite_paths(dir, read.map(|entry| entry.map(|entry| entry.path())))?;

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
    }

    #[test]
    fn from_file_reads_and_parses_trace() {
        let path = std::env::temp_dir().join("zeroclaw_eval_case_from_file_test.json");
        std::fs::write(
            &path,
            r#"{"model_name":"demo","turns":[],"expects":{"response_contains":["hi"]}}"#,
        )
        .unwrap();
        let t = LlmTrace::from_file(&path).unwrap();
        assert_eq!(t.model_name, "demo");
        let _ = std::fs::remove_file(&path);
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
        let body = |name: &str| {
            format!(
                r#"{{"model_name":"{name}","turns":[],"expects":{{"response_contains":["x"]}}}}"#
            )
        };
        std::fs::write(dir.join("b.json"), body("b")).unwrap();
        std::fs::write(dir.join("a.json"), body("a")).unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        let suite = load_suite(&dir).unwrap();
        assert_eq!(suite.len(), 2); // the .txt file is ignored
        assert_eq!(suite[0].1.model_name, "a"); // sorted by path
        assert_eq!(suite[1].1.model_name, "b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Write `body` to a fixture file and load it, returning the loader result.
    fn load_fixture(name: &str, body: &str) -> anyhow::Result<LlmTrace> {
        let dir = std::env::temp_dir().join("zeroclaw_eval_fixture_validation");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.json"));
        std::fs::write(&path, body).unwrap();
        let out = LlmTrace::from_file(&path);
        let _ = std::fs::remove_file(&path);
        out
    }

    #[test]
    fn unknown_expectation_key_is_rejected() {
        // A one-character typo (`workspce`) used to be silently ignored, turning
        // a real regression check into permanent green.
        let err = load_fixture(
            "unknown_key",
            r#"{"model_name":"m","turns":[],"expects":{"response_contains":["hi"],"workspce":{"file_exists":["out.txt"]}}}"#,
        )
        .expect_err("an unknown expects key must be a load error");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("workspce"),
            "error must name the unknown key: {chain}"
        );
        assert!(
            chain.contains("unknown_key.json"),
            "error must name the fixture path: {chain}"
        );
    }

    #[test]
    fn empty_workspace_block_is_rejected() {
        let err = load_fixture(
            "empty_workspace",
            r#"{"model_name":"m","turns":[],"expects":{"response_contains":["hi"],"workspace":{}}}"#,
        )
        .expect_err("a present-but-empty workspace block must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("expects.workspace"),
            "error must name the vacuous block: {chain}"
        );
    }

    #[test]
    fn empty_budget_block_is_rejected() {
        let err = load_fixture(
            "empty_budget",
            r#"{"model_name":"m","turns":[],"expects":{"response_contains":["hi"],"budget":{}}}"#,
        )
        .expect_err("a present-but-empty budget block must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("expects.budget"),
            "error must name the vacuous block: {chain}"
        );
    }

    #[test]
    fn empty_expectation_list_is_rejected() {
        let err = load_fixture(
            "empty_list",
            r#"{"model_name":"m","turns":[],"expects":{"workspace":{"file_contains":{"out.txt":[]}}}}"#,
        )
        .expect_err("an empty file_contains list must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("empty list"),
            "error must explain the empty list: {chain}"
        );
    }

    #[test]
    fn empty_file_contains_needle_is_rejected() {
        // `String::contains("")` is always true, so an empty needle always passes.
        let err = load_fixture(
            "empty_needle",
            r#"{"model_name":"m","turns":[],"expects":{"workspace":{"file_contains":{"out.txt":[""]}}}}"#,
        )
        .expect_err("an empty file_contains needle must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("empty needle"),
            "error must explain the empty needle: {chain}"
        );
    }

    #[test]
    fn expects_declaring_no_effective_checks_is_rejected() {
        let err = load_fixture("no_checks", r#"{"model_name":"m","turns":[],"expects":{}}"#)
            .expect_err("a case that asserts nothing must be rejected at load time");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("no effective checks"),
            "error must explain the vacuous case: {chain}"
        );
    }

    #[test]
    fn a_single_real_check_is_enough_to_load() {
        // Anti-vacuity for the rejections above: the validator is not simply
        // refusing every fixture.
        let trace = load_fixture(
            "one_check",
            r#"{"model_name":"m","turns":[],"expects":{"max_tool_calls":0}}"#,
        )
        .expect("a case with one real check must load");
        assert_eq!(trace.expects.max_tool_calls, Some(0));
        assert!(!trace.expects.is_vacuous());
    }

    #[test]
    fn load_suite_propagates_entry_error_instead_of_shrinking() {
        // Two readable fixtures plus one entry that fails at the iterator level:
        // discovery must abort, not certify the readable subset.
        let dir = std::path::Path::new("/eval/suite");
        let entries: Vec<std::io::Result<PathBuf>> = vec![
            Ok(dir.join("a.json")),
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "permission denied",
            )),
            Ok(dir.join("b.json")),
        ];
        let err = collect_suite_paths(dir, entries)
            .expect_err("an entry-level error must abort suite discovery");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("/eval/suite"),
            "error must name the suite directory, got: {chain}"
        );
        assert!(
            chain.contains("permission denied"),
            "error must retain the underlying cause, got: {chain}"
        );
    }

    #[test]
    fn collect_suite_paths_filters_and_sorts_on_the_healthy_path() {
        let dir = std::path::Path::new("/eval/suite");
        let entries: Vec<std::io::Result<PathBuf>> = vec![
            Ok(dir.join("b.json")),
            Ok(dir.join("note.txt")),
            Ok(dir.join("a.json")),
        ];
        let paths = collect_suite_paths(dir, entries).unwrap();
        assert_eq!(paths, vec![dir.join("a.json"), dir.join("b.json")]);
    }
}
