//! The evaluation case format — JSON trace fixtures for deterministic replay.
//!
//! Phase 0 cases are [`LlmTrace`] fixtures: a sequence of conversation turns where
//! each turn lists the scripted LLM response steps, plus declarative [`TraceExpects`]
//! that the run is graded against. The format is intentionally a superset target —
//! later phases extend it with setup (seeded workspace/memory) and richer graders.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A complete LLM conversation trace loaded from a JSON fixture.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl LlmTrace {
    /// Load a trace from a JSON file.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading trace fixture {}", path.display()))?;
        let trace: LlmTrace = serde_json::from_str(&content)
            .with_context(|| format!("parsing trace fixture {}", path.display()))?;
        Ok(trace)
    }
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
