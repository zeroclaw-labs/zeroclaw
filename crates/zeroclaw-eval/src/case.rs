//! The evaluation case format — JSON trace fixtures for deterministic replay.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A complete LLM conversation trace loaded from a JSON fixture.
///
/// Unknown keys are rejected. A required, 100%-pass regression gate must not
/// be able to certify a fixture whose expectation was silently dropped
/// because its key was misspelled.
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
///
/// Unknown keys are rejected so a typoed expectation cannot degrade a case
/// into a silent no-op. See [`TraceExpects::is_empty`] for the companion
/// guard against a fixture that declares no expectation at all.
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
    /// Per-dimension LLM-judge rubrics. Judge grades are diagnostic and must be
    /// accompanied by at least one deterministic expectation.
    #[serde(default)]
    pub judge: Vec<JudgeRubric>,
}

fn default_judge_threshold() -> f64 {
    0.7
}

/// One judged dimension of a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeRubric {
    /// Short dimension name, e.g. "helpfulness" — one dimension per entry.
    pub name: String,
    /// The rubric for THIS dimension only.
    pub rubric: String,
    /// Pass threshold on the judge's 0.0–1.0 score. Uncalibrated default.
    #[serde(default = "default_judge_threshold")]
    pub threshold: f64,
    /// Include a rendered transcript (tool calls + results), not just the final
    /// response, so state-dependent rubrics can't be gamed by prose.
    #[serde(default)]
    pub include_transcript: bool,
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
    /// True when the fixture declares no effective assertion.
    ///
    /// A case in this state exercises the agent but certifies nothing, so a
    /// required gate must reject it at load time rather than report it green.
    pub fn is_empty(&self) -> bool {
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
                .is_none_or(WorkspaceExpects::is_empty)
            && self.budget.as_ref().is_none_or(BudgetExpects::is_empty)
    }

    /// Reject nested expectation declarations that would produce no useful grade.
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(workspace) = &self.workspace {
            if workspace.is_empty() {
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
                if needles.iter().any(String::is_empty) {
                    anyhow::bail!(
                        "expects.workspace.file_contains[{rel:?}] contains an empty needle, \
                         which every file trivially satisfies"
                    );
                }
            }
        }
        if self.budget.as_ref().is_some_and(BudgetExpects::is_empty) {
            anyhow::bail!(
                "expects.budget is present but declares no bounds; \
                 remove the block or set at least one max_* field"
            );
        }
        for (index, judge) in self.judge.iter().enumerate() {
            if judge.name.trim().is_empty() {
                anyhow::bail!("expects.judge[{index}].name must not be empty");
            }
            if judge.rubric.trim().is_empty() {
                anyhow::bail!("expects.judge[{index}].rubric must not be empty");
            }
            if !judge.threshold.is_finite() || !(0.0..=1.0).contains(&judge.threshold) {
                anyhow::bail!(
                    "expects.judge[{index}].threshold must be a finite number between 0.0 and 1.0"
                );
            }
        }
        Ok(())
    }

    /// The name of the first string-backed family holding a zero-length entry.
    ///
    /// A zero-length entry is admitted by [`is_empty`](Self::is_empty) because
    /// the vector is non-empty, yet it asserts nothing: every response contains
    /// the empty substring, the empty regex matches every response, and no tool
    /// is ever recorded under an empty name. The negative families are rejected
    /// on the same rule for a consistent schema.
    fn empty_entry_family(&self) -> Option<&'static str> {
        [
            ("response_contains", &self.response_contains),
            ("response_not_contains", &self.response_not_contains),
            ("tools_used", &self.tools_used),
            ("tools_not_used", &self.tools_not_used),
            ("response_matches", &self.response_matches),
        ]
        .into_iter()
        .find(|(_, values)| values.iter().any(String::is_empty))
        .map(|(name, _)| name)
    }
}

impl WorkspaceExpects {
    fn is_empty(&self) -> bool {
        self.file_exists.is_empty() && self.file_absent.is_empty() && self.file_contains.is_empty()
    }
}

impl BudgetExpects {
    fn is_empty(&self) -> bool {
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
        if trace.expects.is_empty() {
            anyhow::bail!(
                "trace fixture {} declares no effective expectation; a case that asserts \
                 nothing would pass vacuously and cannot certify the required regression gate",
                path.display()
            );
        }
        if let Some(family) = trace.expects.empty_entry_family() {
            anyhow::bail!(
                "trace fixture {} declares a zero-length entry in `{}`; an empty value asserts \
                 nothing and cannot certify the required regression gate",
                path.display(),
                family
            );
        }
        trace
            .expects
            .validate()
            .with_context(|| format!("validating trace fixture {}", path.display()))?;
        Ok(trace)
    }
}

/// Collect the `*.json` fixture paths from an iterator of directory entries,
/// sorted by path for stable ordering.
///
/// Every entry error aborts the collection. A suite that silently shrank
/// because one entry could not be read would let the regression gate report
/// success while certifying fewer fixtures than it claims, so a partial
/// listing is never treated as a valid suite.
fn collect_fixture_paths(
    entries: impl Iterator<Item = std::io::Result<PathBuf>>,
    dir: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.with_context(|| {
            format!("reading an entry in eval suite directory {}", dir.display())
        })?;
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
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

    let paths = collect_fixture_paths(read.map(|entry| entry.map(|e| e.path())), dir)?;

    let mut out = Vec::with_capacity(paths.len());
    let mut identities = std::collections::BTreeMap::new();
    for path in paths {
        let trace = LlmTrace::from_file(&path)?;
        let identity = trace.display_id().to_string();
        if let Some(first_path) = identities.insert(identity.clone(), path.clone()) {
            anyhow::bail!(
                "eval suite {} declares duplicate case identity {:?} in {} and {}; \
                 reports, receipts, and baseline joins require unique identities",
                dir.display(),
                identity,
                first_path.display(),
                path.display()
            );
        }
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
        // Deserialization still tolerates the omitted field; `from_file` is
        // the layer that refuses to admit the resulting no-op case.
        assert!(t.expects.is_empty());
    }

    #[test]
    fn from_file_reads_and_parses_trace() {
        let path = std::env::temp_dir().join("zeroclaw_eval_case_from_file_test.json");
        std::fs::write(
            &path,
            r#"{"model_name":"demo","turns":[],"expects":{"response_contains":["ok"]}}"#,
        )
        .unwrap();
        let t = LlmTrace::from_file(&path).unwrap();
        assert_eq!(t.model_name, "demo");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_file_rejects_a_fixture_with_omitted_expectations() {
        let path = std::env::temp_dir().join("zeroclaw_eval_case_omitted_expects_test.json");
        std::fs::write(&path, r#"{"model_name":"demo","turns":[]}"#).unwrap();

        let err = LlmTrace::from_file(&path)
            .expect_err("a fixture that asserts nothing must not load into a required gate");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("declares no effective expectation"),
            "error must explain the vacuous-pass rejection, got: {rendered}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_file_rejects_a_fixture_with_an_empty_expects_block() {
        let path = std::env::temp_dir().join("zeroclaw_eval_case_empty_expects_test.json");
        std::fs::write(&path, r#"{"model_name":"demo","turns":[],"expects":{}}"#).unwrap();

        let err = LlmTrace::from_file(&path)
            .expect_err("an explicitly empty expects block asserts nothing either");

        assert!(
            format!("{err:#}").contains("declares no effective expectation"),
            "an empty `expects` object must be rejected like an omitted one"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_file_rejects_a_typoed_expectation_key() {
        // Without `deny_unknown_fields` a misspelled key is dropped in
        // silence, leaving a case that runs the agent and grades nothing.
        let path = std::env::temp_dir().join("zeroclaw_eval_case_typo_expects_test.json");
        std::fs::write(
            &path,
            r#"{"model_name":"demo","turns":[],"expects":{"response_contain":["ok"]}}"#,
        )
        .unwrap();

        let err = LlmTrace::from_file(&path)
            .expect_err("a typoed expectation key must fail loudly, not degrade to a no-op");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("response_contain"),
            "error must name the offending key, got: {rendered}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_file_rejects_an_unknown_top_level_key() {
        let path = std::env::temp_dir().join("zeroclaw_eval_case_typo_toplevel_test.json");
        std::fs::write(
            &path,
            r#"{"model_name":"demo","turns":[],"expect":{"response_contains":["ok"]}}"#,
        )
        .unwrap();

        let err = LlmTrace::from_file(&path)
            .expect_err("a misspelled top-level key silently discards every expectation");

        assert!(
            format!("{err:#}").contains("expect"),
            "error must name the offending top-level key"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn trace_expects_is_empty_is_false_for_each_individual_expectation() {
        // Each field alone must be enough to admit the fixture, otherwise a
        // legitimate single-assertion case would be rejected as vacuous.
        let cases = [
            r#"{"response_contains":["a"]}"#,
            r#"{"response_not_contains":["a"]}"#,
            r#"{"tools_used":["echo"]}"#,
            r#"{"tools_not_used":["echo"]}"#,
            r#"{"max_tool_calls":0}"#,
            r#"{"all_tools_succeeded":false}"#,
            r#"{"response_matches":["^a"]}"#,
        ];
        for raw in cases {
            let expects: TraceExpects = serde_json::from_str(raw).unwrap();
            assert!(
                !expects.is_empty(),
                "{raw} declares a real assertion and must not count as empty"
            );
        }
        // `max_tool_calls: 0` is a genuine assertion (no tool may run), so it
        // must not be confused with an absent bound.
        let zero: TraceExpects = serde_json::from_str(r#"{"max_tool_calls":0}"#).unwrap();
        assert_eq!(zero.max_tool_calls, Some(0));
    }

    #[test]
    fn from_file_rejects_a_zero_length_entry_in_every_string_backed_family() {
        // Each of these loads today because the vector is non-empty, yet the
        // entry asserts nothing. `response_contains`/`response_matches`/
        // `tools_not_used` would additionally report a passing grade, which is
        // a false green rather than the vacuous zero-grade case above.
        let cases = [
            ("response_contains", r#"{"response_contains":[""]}"#),
            ("response_matches", r#"{"response_matches":[""]}"#),
            ("tools_not_used", r#"{"tools_not_used":[""]}"#),
            ("response_not_contains", r#"{"response_not_contains":[""]}"#),
            ("tools_used", r#"{"tools_used":[""]}"#),
        ];

        for (family, expects) in cases {
            let path = std::env::temp_dir()
                .join(format!("zeroclaw_eval_case_empty_entry_{family}_test.json"));
            std::fs::write(
                &path,
                format!(r#"{{"model_name":"demo","turns":[],"expects":{expects}}}"#),
            )
            .unwrap();

            let err = LlmTrace::from_file(&path)
                .expect_err("a zero-length entry must not load into a required gate");
            let rendered = format!("{err:#}");
            assert!(
                rendered.contains("declares a zero-length entry"),
                "error must explain the zero-length rejection, got: {rendered}"
            );
            assert!(
                rendered.contains(family),
                "error must name the offending family {family}, got: {rendered}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn trace_expects_keeps_non_empty_entries_admissible() {
        let expects: TraceExpects = serde_json::from_str(
            r#"{"response_contains":["ok"],"tools_not_used":["shell"],"response_matches":["^o"]}"#,
        )
        .unwrap();
        assert!(expects.empty_entry_family().is_none());
    }

    #[test]
    fn from_file_rejects_judge_only_fixture() {
        let err = load_fixture(
            "judge_only",
            r#"{"model_name":"demo","turns":[],"expects":{"judge":[{"name":"quality","rubric":"be correct"}]}}"#,
        )
        .expect_err("a diagnostic judge cannot be the fixture's only expectation");
        assert!(format!("{err:#}").contains("no effective expectation"));
    }

    #[test]
    fn judge_rubric_rejects_unknown_fields() {
        let err = serde_json::from_str::<TraceExpects>(
            r#"{"max_tool_calls":0,"judge":[{"name":"quality","rubric":"be correct","threshhold":0.9}]}"#,
        )
        .expect_err("a misspelled judge key must not be ignored");
        assert!(err.to_string().contains("threshhold"));
    }

    #[test]
    fn judge_rubric_validation_rejects_invalid_content_and_thresholds() {
        for (name, rubric, threshold) in [
            (" ", "be correct", 0.7),
            ("quality", "\t", 0.7),
            ("quality", "be correct", -0.1),
            ("quality", "be correct", 1.1),
            ("quality", "be correct", f64::NAN),
        ] {
            let expects = TraceExpects {
                max_tool_calls: Some(0),
                judge: vec![JudgeRubric {
                    name: name.to_string(),
                    rubric: rubric.to_string(),
                    threshold,
                    include_transcript: false,
                }],
                ..TraceExpects::default()
            };
            assert!(expects.validate().is_err());
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
        std::fs::write(
            dir.join("b.json"),
            r#"{"model_name":"b","turns":[],"expects":{"response_contains":["b"]}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("a.json"),
            r#"{"model_name":"a","turns":[],"expects":{"response_contains":["a"]}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        let suite = load_suite(&dir).unwrap();
        assert_eq!(suite.len(), 2); // the .txt file is ignored
        assert_eq!(suite[0].1.model_name, "a"); // sorted by path
        assert_eq!(suite[1].1.model_name, "b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_suite_rejects_duplicate_display_ids() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.json"),
            r#"{"model_name":"first","id":"same","turns":[],"expects":{"max_tool_calls":0}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.json"),
            r#"{"model_name":"second","id":"same","turns":[],"expects":{"max_tool_calls":0}}"#,
        )
        .unwrap();

        let err = load_suite(dir.path()).expect_err("duplicate receipt identities must fail");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("duplicate case identity \"same\""));
        assert!(rendered.contains("a.json"));
        assert!(rendered.contains("b.json"));
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
            chain.contains("no effective expectation"),
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
        assert!(!trace.expects.is_empty());
    }

    #[test]
    fn collect_fixture_paths_propagates_entry_errors() {
        let dir = Path::new("/eval/suite");
        let entries = vec![
            Ok(dir.join("a.json")),
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "entry unreadable",
            )),
            Ok(dir.join("b.json")),
        ];

        let err = collect_fixture_paths(entries.into_iter(), dir)
            .expect_err("an unreadable entry must abort the suite, not shrink it");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("reading an entry in eval suite directory /eval/suite"),
            "error must name the suite directory, got: {rendered}"
        );
        assert!(
            rendered.contains("entry unreadable"),
            "error must preserve the underlying io cause, got: {rendered}"
        );
    }

    #[test]
    fn collect_fixture_paths_keeps_only_sorted_json_on_the_happy_path() {
        let dir = Path::new("/eval/suite");
        let entries = vec![
            Ok(dir.join("b.json")),
            Ok(dir.join("note.txt")),
            Ok(dir.join("a.json")),
        ];

        let paths = collect_fixture_paths(entries.into_iter(), dir).unwrap();

        assert_eq!(paths, vec![dir.join("a.json"), dir.join("b.json")]);
    }
}
