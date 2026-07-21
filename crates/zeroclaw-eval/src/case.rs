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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

impl LlmTrace {
    /// The identity used in reports and receipts: the explicit `id` when set,
    /// otherwise `model_name`.
    pub fn display_id(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.model_name)
    }

    /// Load a trace from a JSON file.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading trace fixture {}", path.display()))?;
        let trace: LlmTrace = serde_json::from_str(&content)
            .with_context(|| format!("parsing trace fixture {}", path.display()))?;
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
    }

    #[test]
    fn from_file_reads_and_parses_trace() {
        let path = std::env::temp_dir().join("zeroclaw_eval_case_from_file_test.json");
        std::fs::write(&path, r#"{"model_name":"demo","turns":[]}"#).unwrap();
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
        std::fs::write(dir.join("b.json"), r#"{"model_name":"b","turns":[]}"#).unwrap();
        std::fs::write(dir.join("a.json"), r#"{"model_name":"a","turns":[]}"#).unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        let suite = load_suite(&dir).unwrap();
        assert_eq!(suite.len(), 2); // the .txt file is ignored
        assert_eq!(suite[0].1.model_name, "a"); // sorted by path
        assert_eq!(suite[1].1.model_name, "b");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
