//! The evaluation case format — JSON trace fixtures for deterministic replay.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A complete LLM conversation trace loaded from a JSON fixture.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Number of isolated live runs for this case (clamped to 1..=50). In replay
    /// `repeat > 1` runs once (deterministic). A live case counts as PASSED for
    /// gating/baselines iff every run passes (pass^k).
    #[serde(default = "default_repeat")]
    pub repeat: u32,
    /// Optional cluster label. Correlated case families sharing a label are
    /// averaged together before the suite error bar, so resamples of one family do
    /// not fake precision. Omitting it asserts independence.
    #[serde(default)]
    pub cluster: Option<String>,
}

fn default_repeat() -> u32 {
    1
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
///
/// `deny_unknown_fields`: a misspelled expectation such as `response_contians`
/// would otherwise deserialize into the default (empty) block and the case would
/// pass without asserting anything.
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
    /// Number of effective assertions this block declares — i.e. how many
    /// [`GradeResult`](crate::grader::GradeResult)s grading it will produce.
    /// Zero means the case asserts nothing and cannot produce a meaningful
    /// green.
    pub fn assertion_count(&self) -> usize {
        let mut n = self.response_contains.len()
            + self.response_not_contains.len()
            + self.tools_used.len()
            + self.tools_not_used.len()
            + self.response_matches.len()
            + self.response_json.len()
            + usize::from(self.max_tool_calls.is_some())
            + usize::from(self.all_tools_succeeded.is_some());
        if let Some(ws) = &self.workspace {
            n += ws.assertion_count();
        }
        if let Some(budget) = &self.budget {
            n += budget.assertion_count();
        }
        n
    }

    /// Reject expectation shapes that parse but can never assert anything:
    /// empty needles (which match every response vacuously) and declared-but-
    /// empty `workspace` / `budget` blocks.
    fn validate(&self) -> anyhow::Result<()> {
        for (field, needles) in [
            ("response_contains", &self.response_contains),
            ("response_not_contains", &self.response_not_contains),
            ("response_matches", &self.response_matches),
        ] {
            if needles.iter().any(|n| n.is_empty()) {
                anyhow::bail!("expects.{field} must not contain an empty string");
            }
        }
        for (field, tools) in [
            ("tools_used", &self.tools_used),
            ("tools_not_used", &self.tools_not_used),
        ] {
            if tools.iter().any(|t| t.trim().is_empty()) {
                anyhow::bail!("expects.{field} must not contain an empty tool name");
            }
        }
        if let Some(ws) = &self.workspace {
            ws.validate()?;
        }
        if let Some(budget) = &self.budget
            && budget.assertion_count() == 0
        {
            anyhow::bail!("expects.budget is declared but sets no bound");
        }
        Ok(())
    }
}

impl WorkspaceExpects {
    fn assertion_count(&self) -> usize {
        self.file_exists.len()
            + self.file_absent.len()
            + self.file_contains.values().map(Vec::len).sum::<usize>()
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.assertion_count() == 0 {
            anyhow::bail!("expects.workspace is declared but asserts nothing");
        }
        for (path, needles) in &self.file_contains {
            if needles.is_empty() {
                anyhow::bail!("expects.workspace.file_contains[{path:?}] lists no substring");
            }
            if needles.iter().any(|n| n.is_empty()) {
                // An empty needle matches every file, so it is an assertion in
                // name only.
                anyhow::bail!(
                    "expects.workspace.file_contains[{path:?}] must not contain an empty substring"
                );
            }
        }
        Ok(())
    }
}

impl BudgetExpects {
    fn assertion_count(&self) -> usize {
        usize::from(self.max_input_tokens.is_some())
            + usize::from(self.max_output_tokens.is_some())
            + usize::from(self.max_total_tokens.is_some())
            + usize::from(self.max_duration_ms.is_some())
            + usize::from(self.max_llm_calls.is_some())
    }
}

impl LlmTrace {
    /// The identity used in reports and receipts: the explicit `id` when set,
    /// otherwise `model_name`.
    pub fn display_id(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.model_name)
    }

    /// Fail closed on a case that can never assert anything.
    ///
    /// Three permissive layers used to compose into a vacuous green: the
    /// expectation structs deserialized permissively, `grade_run` returned no
    /// grades for an empty expectation block, and `CaseReport::passed()` treated
    /// an empty grade list as a pass. A fixture in the required
    /// `evals/regression` gate could therefore pass without making a single
    /// assertion, or even calling a provider. A green result must mean at least
    /// one declared assertion actually ran.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.expects.validate()?;
        if self.expects.assertion_count() == 0 {
            anyhow::bail!(
                "eval case {:?} declares no effective assertion; a case that asserts nothing cannot pass",
                self.display_id()
            );
        }
        Ok(())
    }

    /// Load a trace from a JSON file, without the assertion-contract check.
    fn parse_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading trace fixture {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("parsing trace fixture {}", path.display()))
    }

    /// Load a trace from a JSON file and enforce its assertion contract.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let trace = Self::parse_file(path)?;
        trace
            .validate()
            .with_context(|| format!("validating trace fixture {}", path.display()))?;
        Ok(trace)
    }
}

/// SHA-256 hex of the case's canonical JSON, used as the receipt's comparability
/// key. `serde_json` emits object keys in sorted (BTreeMap) order because nothing
/// in this workspace enables `preserve_order`, so the hash is stable across
/// re-serialization (guarded by `canonical_json_is_key_sorted`).
pub fn case_hash(trace: &LlmTrace) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::to_string(&serde_json::to_value(trace)?)?;
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
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

/// Collect the `*.json` fixture paths from an eval-suite directory listing,
/// sorted by path for stable ordering.
///
/// Fails closed: an entry-level I/O error (an unreadable entry, or one that
/// disappears while the directory is being enumerated) is propagated with the
/// suite path as context rather than skipped. Dropping such an entry would
/// silently shrink the certified suite, and the regression-suite CI gate would
/// report green for an incomplete `evals/regression` directory.
///
/// Takes the listing as an iterator so the failure path is directly testable
/// without depending on a platform-specific way of provoking a `readdir` error.
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

/// Reject empty and colliding case identities in a loaded suite.
///
/// [`LlmTrace::display_id`] permits any `id`, including an empty string, and
/// nothing else in the load path enforces uniqueness. Two downstream maps are
/// keyed by that identity — the baseline comparison's `cur_map` and the live
/// retry's `rerun_passed[id]` — so colliding ids collapse last-writer-wins: a
/// failing fixture and a passing fixture sharing an id can have the real
/// failure overwritten by the passing retry and downgraded to flaky. That is a
/// gate bypass, so identities are validated once at load, before any suite
/// execution, baseline comparison, or retry.
fn validate_suite_identities(suite: &[(PathBuf, LlmTrace)]) -> anyhow::Result<()> {
    let mut seen: std::collections::BTreeMap<&str, &PathBuf> = std::collections::BTreeMap::new();
    for (path, trace) in suite {
        let id = trace.display_id();
        if id.trim().is_empty() {
            anyhow::bail!("eval fixture {} has an empty case id", path.display());
        }
        if let Some(prev) = seen.insert(id, path) {
            anyhow::bail!(
                "duplicate eval case id {id:?}: {} and {}",
                prev.display(),
                path.display()
            );
        }
    }
    Ok(())
}

/// Load every `*.json` trace fixture in `dir`, sorted by path for stable ordering.
pub fn load_suite(dir: &Path) -> anyhow::Result<Vec<(PathBuf, LlmTrace)>> {
    let read = std::fs::read_dir(dir)
        .with_context(|| format!("reading eval suite directory {}", dir.display()))?;

    let paths = collect_suite_paths(dir, read.map(|entry| entry.map(|e| e.path())))?;

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let trace = LlmTrace::parse_file(&path)?;
        out.push((path, trace));
    }
    // Identity first: a collision or blank id is a suite-level defect and the
    // clearest thing to report. Then the per-case assertion contract.
    validate_suite_identities(&out)?;
    for (path, trace) in &out {
        trace
            .validate()
            .with_context(|| format!("validating trace fixture {}", path.display()))?;
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
            r#"{"model_name":"demo","turns":[],"expects":{"response_contains":["x"]}}"#,
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
    fn canonical_json_is_key_sorted() {
        // Guard: if anyone enables serde_json's `preserve_order`, this fails,
        // alerting that case_hash would stop being canonical.
        let v = serde_json::json!({ "b": 1, "a": 2 });
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn case_hash_stable_across_reserialization() {
        let trace: LlmTrace =
            serde_json::from_str(r#"{"model_name":"m","turns":[{"user_input":"hi"}]}"#).unwrap();
        // Re-parse from a re-serialized form; the hash must be identical.
        let reserialized: LlmTrace =
            serde_json::from_str(&serde_json::to_string(&trace).unwrap()).unwrap();
        assert_eq!(
            case_hash(&trace).unwrap(),
            case_hash(&reserialized).unwrap()
        );
    }

    #[test]
    fn case_hash_changes_on_case_edit() {
        let a: LlmTrace = serde_json::from_str(r#"{"model_name":"m","turns":[]}"#).unwrap();
        let b: LlmTrace = serde_json::from_str(r#"{"model_name":"m2","turns":[]}"#).unwrap();
        assert_ne!(case_hash(&a).unwrap(), case_hash(&b).unwrap());
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
        std::fs::write(dir.join("b.json"), asserting_fixture("b")).unwrap();
        std::fs::write(dir.join("a.json"), asserting_fixture("a")).unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        let suite = load_suite(&dir).unwrap();
        assert_eq!(suite.len(), 2); // the .txt file is ignored
        assert_eq!(suite[0].1.model_name, "a"); // sorted by path
        assert_eq!(suite[1].1.model_name, "b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fail closed: `load_suite`'s directory-listing pass must propagate an
    /// entry-level I/O error instead of dropping the entry. A dropped entry
    /// would silently shrink the suite that the regression CI gate certifies.
    ///
    /// The error is injected through `collect_suite_paths`, the exact iterator
    /// pass `load_suite` runs its `read_dir` results through, because provoking
    /// a genuine per-entry `readdir` failure is platform-specific and racy.
    #[test]
    fn load_suite_propagates_entry_errors() {
        let dir = Path::new("/some/eval/suite");
        let entries = vec![
            Ok(dir.join("a.json")),
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "entry vanished mid-enumeration",
            )),
            Ok(dir.join("b.json")),
        ];
        let err = collect_suite_paths(dir, entries)
            .expect_err("an unreadable directory entry must not be skipped");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("/some/eval/suite"),
            "error must name the suite directory, got: {rendered}"
        );
        assert!(
            rendered.contains("entry vanished mid-enumeration"),
            "error must preserve the underlying I/O cause, got: {rendered}"
        );
    }

    /// The injectable collector is the same filter/sort the directory walk uses,
    /// so the failure test above covers the real `load_suite` path.
    #[test]
    fn collect_suite_paths_filters_json_and_sorts() {
        let dir = Path::new("/some/eval/suite");
        let entries = vec![
            Ok(dir.join("b.json")),
            Ok(dir.join("note.txt")),
            Ok(dir.join("a.json")),
        ];
        let paths = collect_suite_paths(dir, entries).unwrap();
        assert_eq!(paths, vec![dir.join("a.json"), dir.join("b.json")]);
    }

    /// A minimal fixture that satisfies the assertion contract, so identity
    /// tests exercise identity validation rather than the contract check.
    fn asserting_fixture(id: &str) -> String {
        format!(
            r#"{{"model_name":"{id}","id":"{id}","turns":[],"expects":{{"response_contains":["x"]}}}}"#
        )
    }

    fn suite_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Colliding ids collapse the baseline `cur_map` and the live retry's
    /// `rerun_passed[id]`, so a passing sibling can overwrite a real failure.
    /// The error must name BOTH source paths so the collision is actionable.
    #[test]
    fn load_suite_rejects_duplicate_case_ids() {
        let dir = suite_dir("zeroclaw_eval_case_dup_id_test");
        std::fs::write(dir.join("a_failing.json"), asserting_fixture("duplicate")).unwrap();
        std::fs::write(dir.join("b_passing.json"), asserting_fixture("duplicate")).unwrap();
        let err = load_suite(&dir).expect_err("duplicate case ids must not load");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("duplicate"),
            "error must name the colliding id, got: {rendered}"
        );
        assert!(
            rendered.contains("a_failing.json") && rendered.contains("b_passing.json"),
            "error must name both source paths, got: {rendered}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty (or whitespace-only) effective id is not a usable map key for
    /// baseline comparison or retry bookkeeping, so it is rejected at load.
    #[test]
    fn load_suite_rejects_empty_case_id() {
        let dir = suite_dir("zeroclaw_eval_case_empty_id_test");
        std::fs::write(dir.join("blank.json"), asserting_fixture("")).unwrap();
        let err = load_suite(&dir).expect_err("an empty case id must not load");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("blank.json") && rendered.contains("empty case id"),
            "error must name the offending fixture, got: {rendered}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `display_id()` falls back to `model_name`, so a blank `model_name` with
    /// no `id` is the same hazard by another route.
    #[test]
    fn load_suite_rejects_whitespace_only_effective_id() {
        let dir = suite_dir("zeroclaw_eval_case_ws_id_test");
        std::fs::write(dir.join("ws.json"), asserting_fixture("   ")).unwrap();
        let err = load_suite(&dir).expect_err("a whitespace-only case id must not load");
        assert!(format!("{err:#}").contains("empty case id"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Distinct ids across fixtures remain loadable — the guard is not blanket.
    #[test]
    fn load_suite_accepts_distinct_case_ids() {
        let dir = suite_dir("zeroclaw_eval_case_ok_id_test");
        std::fs::write(dir.join("a.json"), asserting_fixture("a")).unwrap();
        std::fs::write(dir.join("b.json"), asserting_fixture("b")).unwrap();
        assert_eq!(load_suite(&dir).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A typo such as `response_contians` silently deserialized into the default
    /// (empty) expectations block, so the case asserted nothing and passed.
    /// `deny_unknown_fields` makes it a load error naming the field.
    #[test]
    fn rejects_unknown_expectation_field() {
        let err = serde_json::from_str::<LlmTrace>(
            r#"{"model_name":"typo","turns":[],"expects":{"response_contians":["hi"]}}"#,
        )
        .expect_err("a misspelled expectation must not deserialize");
        assert!(
            err.to_string().contains("response_contians"),
            "error must name the offending field, got: {err}"
        );
    }

    /// The same guard on the nested blocks.
    #[test]
    fn rejects_unknown_nested_expectation_field() {
        assert!(
            serde_json::from_str::<LlmTrace>(
                r#"{"model_name":"t","turns":[],"expects":{"workspace":{"file_exsits":["a"]}}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<LlmTrace>(
                r#"{"model_name":"t","turns":[],"expects":{"budget":{"max_tokens":5}}}"#
            )
            .is_err()
        );
    }

    /// No turns and no effective expectation: the case cannot assert anything,
    /// so it must not load into a gate that treats loading as certification.
    #[test]
    fn rejects_assertion_free_case() {
        let trace: LlmTrace = serde_json::from_str(r#"{"model_name":"empty","turns":[]}"#).unwrap();
        let err = trace
            .validate()
            .expect_err("an assertion-free case must be rejected");
        assert!(
            err.to_string().contains("no effective assertion"),
            "got: {err}"
        );
    }

    /// A declared-but-empty `workspace` / `budget` block asserts nothing.
    #[test]
    fn rejects_empty_expectation_blocks() {
        for json in [
            r#"{"model_name":"t","turns":[],"expects":{"workspace":{}}}"#,
            r#"{"model_name":"t","turns":[],"expects":{"budget":{}}}"#,
        ] {
            let trace: LlmTrace = serde_json::from_str(json).unwrap();
            assert!(
                trace.validate().is_err(),
                "an empty expectation block must be rejected: {json}"
            );
        }
    }

    /// An empty `file_contains` needle matches every file, so it is an
    /// assertion in name only.
    #[test]
    fn rejects_empty_file_contains_needle() {
        let trace: LlmTrace = serde_json::from_str(
            r#"{"model_name":"t","turns":[],"expects":{"workspace":{"file_contains":{"out.txt":[""]}}}}"#,
        )
        .unwrap();
        let err = trace
            .validate()
            .expect_err("an empty file_contains needle must be rejected");
        assert!(err.to_string().contains("empty substring"), "got: {err}");

        let empty_list: LlmTrace = serde_json::from_str(
            r#"{"model_name":"t","turns":[],"expects":{"workspace":{"file_contains":{"out.txt":[]}}}}"#,
        )
        .unwrap();
        assert!(empty_list.validate().is_err());
    }

    /// The same vacuity applies to an empty response needle.
    #[test]
    fn rejects_empty_response_needle() {
        let trace: LlmTrace = serde_json::from_str(
            r#"{"model_name":"t","turns":[],"expects":{"response_contains":[""]}}"#,
        )
        .unwrap();
        assert!(trace.validate().is_err());
    }

    /// The contract counts every expectation shape, so a case asserting only a
    /// budget bound or only a workspace check is still valid.
    #[test]
    fn accepts_cases_whose_only_assertion_is_a_block() {
        for json in [
            r#"{"model_name":"t","turns":[],"expects":{"budget":{"max_llm_calls":2}}}"#,
            r#"{"model_name":"t","turns":[],"expects":{"workspace":{"file_exists":["out.txt"]}}}"#,
            r#"{"model_name":"t","turns":[],"expects":{"max_tool_calls":0}}"#,
        ] {
            let trace: LlmTrace = serde_json::from_str(json).unwrap();
            assert!(trace.validate().is_ok(), "must accept: {json}");
            assert!(trace.expects.assertion_count() >= 1);
        }
    }

    /// The four shipped regression fixtures must satisfy the new contract; the
    /// gate they back is the reason the contract exists.
    #[test]
    fn shipped_regression_fixtures_satisfy_the_assertion_contract() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/regression");
        let suite = load_suite(&dir).expect("shipped regression suite must load");
        assert!(!suite.is_empty());
        for (path, trace) in &suite {
            assert!(
                trace.expects.assertion_count() > 0,
                "{} asserts nothing",
                path.display()
            );
        }
    }
}
