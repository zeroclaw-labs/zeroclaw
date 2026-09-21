//! Stateless DTOs for the Ollama `/api/chat` wire shape.
//!
//! Providers own transport and policy. This module only shares the neutral
//! request/response representation used by servers that speak this protocol.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub(super) struct ChatRequest {
    pub(super) model: String,
    pub(super) messages: Vec<Message>,
    pub(super) stream: bool,
    pub(super) options: Options,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) think: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct Message {
    pub(super) role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) images: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<Vec<OutgoingToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct OutgoingToolCall {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) function: OutgoingFunction,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct OutgoingFunction {
    pub(super) name: String,
    pub(super) arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub(super) struct Options {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) num_ctx: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) num_predict: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ApiChatResponse {
    pub(super) message: ResponseMessage,
    #[serde(default)]
    pub(super) done: Option<bool>,
    #[serde(default)]
    pub(super) prompt_eval_count: Option<u64>,
    #[serde(default)]
    pub(super) eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ResponseMessage {
    #[serde(default)]
    pub(super) content: String,
    #[serde(default)]
    pub(super) tool_calls: Vec<OllamaToolCall>,
    /// Some models return a `thinking` field with internal reasoning.
    #[serde(default)]
    pub(super) thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OllamaToolCall {
    pub(super) id: Option<String>,
    pub(super) function: OllamaFunction,
}

#[derive(Debug, Deserialize)]
pub(super) struct OllamaFunction {
    pub(super) name: String,
    #[serde(default, deserialize_with = "deserialize_args")]
    pub(super) arguments: serde_json::Value,
}

fn deserialize_args<'de, D>(deserializer: D) -> Result<serde_json::Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;

    if let Some(serialized) = value.as_str() {
        Ok(serde_json::from_str(serialized).unwrap_or_else(|_| serde_json::json!({})))
    } else {
        Ok(value)
    }
}
