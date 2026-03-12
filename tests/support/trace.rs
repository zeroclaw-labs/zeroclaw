//! JSON trace fixture types for deterministic LLM response replay.

use serde::Deserialize;
use std::path::Path;

/// A complete LLM conversation trace loaded from a JSON fixture.
#[derive(Debug, Deserialize)]
pub struct LlmTrace {
    pub model_name: String,
    pub turns: Vec<TraceTurn>,
    #[serde(default)]
    pub expects: TraceExpects,
}

/// A single conversation turn (user input + LLM response steps).
#[derive(Debug, Deserialize)]
pub struct TraceTurn {
    pub user_input: String,
    pub steps: Vec<TraceStep>,
}

/// A single LLM response step within a turn.
#[derive(Debug, Deserialize)]
pub struct TraceStep {
    pub response: TraceResponse,
}

/// The response content — either plain text or tool calls.
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
pub struct TraceToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Declarative expectations for trace verification.
#[derive(Debug, Default, Deserialize)]
pub struct TraceExpects {
    #[serde(default)]
    pub response_contains: Vec<String>,
    #[serde(default)]
    pub response_not_contains: Vec<String>,
    #[serde(default)]
    pub tools_used: Vec<String>,
    #[serde(default)]
    pub tools_not_used: Vec<String>,
    #[serde(default)]
    pub max_tool_calls: Option<usize>,
    #[serde(default)]
    pub all_tools_succeeded: Option<bool>,
    #[serde(default)]
    pub response_matches: Vec<String>,
}

impl LlmTrace {
    /// Load a trace from a JSON file.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let trace: LlmTrace = serde_json::from_str(&content)?;
        Ok(trace)
    }
}
