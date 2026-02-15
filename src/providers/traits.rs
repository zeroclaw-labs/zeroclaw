use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Structured types for agentic tool-use conversations ──────────

/// A content block in a multi-turn conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

/// A message in a multi-turn agentic conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: ChatContent,
}

/// Content: either a plain string or structured content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// A tool definition for LLM function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Response from a structured chat completion (may include tool calls).
#[derive(Debug, Clone)]
pub struct ChatCompletionResponse {
    pub content: Vec<ContentBlock>,
    /// "`end_turn`", "`tool_use`", "`max_tokens`", or provider-specific values.
    pub stop_reason: Option<String>,
}

impl ChatCompletionResponse {
    /// Returns `true` if the response contains any `tool_use` blocks.
    pub fn has_tool_use(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }

    /// Extract all text content concatenated.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract all `tool_use` blocks.
    pub fn tool_uses(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.as_str(), name.as_str(), input))
                }
                _ => None,
            })
            .collect()
    }
}

// ── Provider trait ───────────────────────────────────────────────

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> anyhow::Result<String> {
        self.chat_with_system(None, message, model, temperature)
            .await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String>;

    /// Structured chat completion with tool-use support.
    ///
    /// Default implementation wraps `chat_with_system` — providers that
    /// support native tool calling should override this.
    async fn chat_completion(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        max_tokens: u32,
    ) -> anyhow::Result<ChatCompletionResponse> {
        // Fallback: inject tool descriptions into system prompt and use text-only chat.
        let mut sys = system_prompt.unwrap_or("").to_string();
        if !tools.is_empty() {
            sys.push_str("\n\n## Available Tools\n\n");
            sys.push_str("When you need to use a tool, respond with a JSON block:\n");
            sys.push_str("```tool_call\n{\"name\": \"tool_name\", \"input\": {...}}\n```\n\n");
            for tool in tools {
                sys.push_str(&format!("- **{}**: {}\n", tool.name, tool.description));
            }
        }

        // Extract the last user message as the prompt
        let last_msg = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| match &m.content {
                ChatContent::Text(t) => t.clone(),
                ChatContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            })
            .unwrap_or_default();

        let _ = max_tokens; // text-only fallback ignores this
        let text = self
            .chat_with_system(Some(&sys), &last_msg, model, temperature)
            .await?;

        Ok(ChatCompletionResponse {
            content: vec![ContentBlock::Text { text }],
            stop_reason: Some("end_turn".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completion_response_text_extraction() {
        let resp = ChatCompletionResponse {
            content: vec![
                ContentBlock::Text {
                    text: "Hello ".into(),
                },
                ContentBlock::Text {
                    text: "world".into(),
                },
            ],
            stop_reason: Some("end_turn".into()),
        };
        assert_eq!(resp.text(), "Hello world");
        assert!(!resp.has_tool_use());
    }

    #[test]
    fn chat_completion_response_tool_use_detection() {
        let resp = ChatCompletionResponse {
            content: vec![
                ContentBlock::Text {
                    text: "Let me check.".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "shell".into(),
                    input: serde_json::json!({"command": "ls"}),
                },
            ],
            stop_reason: Some("tool_use".into()),
        };
        assert!(resp.has_tool_use());
        let tools = resp.tool_uses();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "call_1");
        assert_eq!(tools[0].1, "shell");
    }

    #[test]
    fn content_block_serialization() {
        let block = ContentBlock::Text {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"hello\""));

        let tool = ContentBlock::ToolUse {
            id: "t1".into(),
            name: "shell".into(),
            input: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("\"name\":\"shell\""));
    }

    #[test]
    fn tool_definition_serialization() {
        let tool = ToolDefinition {
            name: "shell".into(),
            description: "Run commands".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
        };
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"name\":\"shell\""));
        let parsed: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "shell");
    }

    #[test]
    fn chat_message_text_content() {
        let msg = ChatMessage {
            role: "user".into(),
            content: ChatContent::Text("hello".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"hello\""));
    }

    #[test]
    fn chat_message_blocks_content() {
        let msg = ChatMessage {
            role: "assistant".into(),
            content: ChatContent::Blocks(vec![ContentBlock::Text {
                text: "thinking...".into(),
            }]),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"assistant\""));
        assert!(json.contains("\"type\":\"text\""));
    }
}
