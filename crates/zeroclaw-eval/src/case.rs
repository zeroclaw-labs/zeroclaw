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
    /// Conversation turns, replayed in order.
    pub turns: Vec<TraceTurn>,
    /// Declarative expectations graded against the run.
    #[serde(default)]
    pub expects: TraceExpects,
}

/// A single conversation turn (user input + scripted LLM response steps).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceTurn {
    pub user_input: String,
    pub steps: Vec<TraceStep>,
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
}

impl TraceExpects {
    /// True when the fixture declares no effective assertion.
    ///
    /// `evaluate_expects` produces one grade per declared expectation, and
    /// `CaseReport::passed()` is vacuously true over zero grades. A case in
    /// this state exercises the agent but certifies nothing, so a required
    /// gate must reject it at load time rather than report it green.
    pub fn is_empty(&self) -> bool {
        self.response_contains.is_empty()
            && self.response_not_contains.is_empty()
            && self.tools_used.is_empty()
            && self.tools_not_used.is_empty()
            && self.max_tool_calls.is_none()
            && self.all_tools_succeeded.is_none()
            && self.response_matches.is_empty()
    }

    /// The name of the first string-backed family holding a zero-length entry.
    ///
    /// A zero-length entry is admitted by [`is_empty`](Self::is_empty) because
    /// the vector is non-empty, yet it asserts nothing: every response contains
    /// the empty substring, the empty regex matches every response, and no tool
    /// is ever recorded under an empty name. In the positive families that
    /// yields a tautological pass and in `tools_not_used` a degenerate one, so
    /// such an entry can certify the required gate green without testing any
    /// behavior. The negative families are rejected on the same rule for a
    /// consistent schema, even though they fail rather than falsely certify.
    fn empty_entry_family(&self) -> Option<&'static str> {
        [
            ("response_contains", &self.response_contains),
            ("response_not_contains", &self.response_not_contains),
            ("tools_used", &self.tools_used),
            ("tools_not_used", &self.tools_not_used),
            ("response_matches", &self.response_matches),
        ]
        .into_iter()
        .find(|(_, values)| values.iter().any(|value| value.is_empty()))
        .map(|(name, _)| name)
    }
}

impl LlmTrace {
    /// Load a trace from a JSON file.
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

/// Load every `*.json` trace fixture in `dir`, sorted by path for stable ordering.
pub fn load_suite(dir: &Path) -> anyhow::Result<Vec<(PathBuf, LlmTrace)>> {
    let read = std::fs::read_dir(dir)
        .with_context(|| format!("reading eval suite directory {}", dir.display()))?;

    let paths = collect_fixture_paths(read.map(|entry| entry.map(|e| e.path())), dir)?;

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
