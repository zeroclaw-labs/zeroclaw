use crate::providers::traits::{
    ChatCompletionResponse, ChatContent, ChatMessage, ContentBlock, Provider, ToolDefinition,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct AnthropicProvider {
    api_key: Option<String>,
    client: Client,
}

// ── Simple chat types (legacy) ─────────────────────────────────

#[derive(Debug, Serialize)]
struct SimpleChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<SimpleMessage>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct SimpleMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct SimpleChatResponse {
    content: Vec<SimpleContentBlock>,
}

#[derive(Debug, Deserialize)]
struct SimpleContentBlock {
    text: String,
}

// ── Structured chat types (tool-use) ───────────────────────────

#[derive(Debug, Serialize)]
struct ToolChatRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
    temperature: f64,
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ToolChatResponse {
    content: Vec<AnthropicContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

impl AnthropicProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self {
            api_key: api_key.map(ToString::to_string),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn get_key(&self) -> anyhow::Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!("Anthropic API key not set. Set ANTHROPIC_API_KEY or edit config.toml.")
        })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_key = self.get_key()?;

        let request = SimpleChatRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system: system_prompt.map(ToString::to_string),
            messages: vec![SimpleMessage {
                role: "user".to_string(),
                content: message.to_string(),
            }],
            temperature,
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("Anthropic API error: {error}");
        }

        let chat_response: SimpleChatResponse = response.json().await?;

        chat_response
            .content
            .into_iter()
            .next()
            .map(|c| c.text)
            .ok_or_else(|| anyhow::anyhow!("No response from Anthropic"))
    }

    async fn chat_completion(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        max_tokens: u32,
    ) -> anyhow::Result<ChatCompletionResponse> {
        let api_key = self.get_key()?;

        // Convert messages to Anthropic format
        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|msg| {
                let content = match &msg.content {
                    ChatContent::Text(t) => serde_json::json!(t),
                    ChatContent::Blocks(blocks) => {
                        let api_blocks: Vec<serde_json::Value> = blocks
                            .iter()
                            .map(|b| match b {
                                ContentBlock::Text { text } => {
                                    serde_json::json!({"type": "text", "text": text})
                                }
                                ContentBlock::ToolUse { id, name, input } => {
                                    serde_json::json!({"type": "tool_use", "id": id, "name": name, "input": input})
                                }
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                } => {
                                    serde_json::json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error})
                                }
                            })
                            .collect();
                        serde_json::json!(api_blocks)
                    }
                };
                serde_json::json!({"role": msg.role, "content": content})
            })
            .collect();

        // Convert tools to Anthropic format
        let api_tools: Vec<AnthropicTool> = tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect();

        let request = ToolChatRequest {
            model: model.to_string(),
            max_tokens,
            system: system_prompt.map(ToString::to_string),
            messages: api_messages,
            tools: api_tools,
            temperature,
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error = response.text().await?;
            anyhow::bail!("Anthropic API error: {error}");
        }

        let api_response: ToolChatResponse = response.json().await?;

        // Convert Anthropic content blocks to our ContentBlock type
        let content = api_response
            .content
            .into_iter()
            .map(|b| match b {
                AnthropicContentBlock::Text { text } => ContentBlock::Text { text },
                AnthropicContentBlock::ToolUse { id, name, input } => {
                    ContentBlock::ToolUse { id, name, input }
                }
            })
            .collect();

        Ok(ChatCompletionResponse {
            content,
            stop_reason: api_response.stop_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_with_key() {
        let p = AnthropicProvider::new(Some("sk-ant-test123"));
        assert!(p.api_key.is_some());
        assert_eq!(p.api_key.as_deref(), Some("sk-ant-test123"));
    }

    #[test]
    fn creates_without_key() {
        let p = AnthropicProvider::new(None);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn creates_with_empty_key() {
        let p = AnthropicProvider::new(Some(""));
        assert!(p.api_key.is_some());
        assert_eq!(p.api_key.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn chat_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_with_system(None, "hello", "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("API key not set"),
            "Expected key error, got: {err}"
        );
    }

    #[tokio::test]
    async fn chat_with_system_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_with_system(Some("You are Aria"), "hello", "claude-3-opus", 0.7)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn chat_completion_fails_without_key() {
        let p = AnthropicProvider::new(None);
        let result = p
            .chat_completion(
                Some("You are Aria"),
                &[ChatMessage {
                    role: "user".into(),
                    content: ChatContent::Text("hello".into()),
                }],
                &[],
                "claude-3-opus",
                0.7,
                4096,
            )
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn simple_request_serializes_without_system() {
        let req = SimpleChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![SimpleMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("system"),
            "system field should be skipped when None"
        );
        assert!(json.contains("claude-3-opus"));
        assert!(json.contains("hello"));
    }

    #[test]
    fn simple_request_serializes_with_system() {
        let req = SimpleChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: Some("You are Aria".to_string()),
            messages: vec![SimpleMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"system\":\"You are Aria\""));
    }

    #[test]
    fn simple_response_deserializes() {
        let json = r#"{"content":[{"type":"text","text":"Hello there!"}]}"#;
        let resp: SimpleChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0].text, "Hello there!");
    }

    #[test]
    fn tool_response_deserializes_text() {
        let json = r#"{"content":[{"type":"text","text":"Hello"}],"stop_reason":"end_turn"}"#;
        let resp: ToolChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 1);
        assert!(matches!(
            &resp.content[0],
            AnthropicContentBlock::Text { text } if text == "Hello"
        ));
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn tool_response_deserializes_tool_use() {
        let json = r#"{"content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"call_1","name":"shell","input":{"command":"ls"}}],"stop_reason":"tool_use"}"#;
        let resp: ToolChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert!(matches!(
            &resp.content[0],
            AnthropicContentBlock::Text { .. }
        ));
        assert!(matches!(
            &resp.content[1],
            AnthropicContentBlock::ToolUse { name, .. } if name == "shell"
        ));
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn tool_request_serializes_with_tools() {
        let req = ToolChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: Some("You are Aria".to_string()),
            messages: vec![serde_json::json!({"role": "user", "content": "list files"})],
            tools: vec![AnthropicTool {
                name: "shell".to_string(),
                description: "Run commands".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"]
                }),
            }],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"tools\""));
        assert!(json.contains("\"name\":\"shell\""));
        assert!(json.contains("\"input_schema\""));
    }

    #[test]
    fn tool_request_serializes_without_tools() {
        let req = ToolChatRequest {
            model: "claude-3-opus".to_string(),
            max_tokens: 4096,
            system: None,
            messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
            tools: vec![],
            temperature: 0.7,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("\"tools\""),
            "tools should be skipped when empty"
        );
    }

    #[test]
    fn temperature_range_serializes() {
        for temp in [0.0, 0.5, 1.0, 2.0] {
            let req = SimpleChatRequest {
                model: "claude-3-opus".to_string(),
                max_tokens: 4096,
                system: None,
                messages: vec![],
                temperature: temp,
            };
            let json = serde_json::to_string(&req).unwrap();
            assert!(json.contains(&format!("{temp}")));
        }
    }
}
