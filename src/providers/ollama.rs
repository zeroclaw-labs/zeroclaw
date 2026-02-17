use crate::providers::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OllamaProvider {
    base_url: String,
    client: Client,
}

// ─── Request Structures ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    options: Options,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct Options {
    temperature: f64,
}

// ─── Response Structures ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiChatResponse {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCall>,
    /// Some models return a "thinking" field with internal reasoning
    #[serde(default)]
    thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    id: Option<String>,
    function: OllamaFunction,
}

#[derive(Debug, Deserialize)]
struct OllamaFunction {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

// ─── Implementation ───────────────────────────────────────────────────────────

impl OllamaProvider {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            base_url: base_url
                .unwrap_or("http://localhost:11434")
                .trim_end_matches('/')
                .to_string(),
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Send a request to Ollama and get the parsed response
    async fn send_request(
        &self,
        messages: Vec<Message>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ApiChatResponse> {
        let request = ChatRequest {
            model: model.to_string(),
            messages,
            stream: false,
            options: Options { temperature },
        };

        let url = format!("{}/api/chat", self.base_url);

        tracing::debug!(
            "Ollama request: url={} model={} message_count={} temperature={}",
            url,
            model,
            request.messages.len(),
            temperature
        );

        let response = self.client.post(&url).json(&request).send().await?;
        let status = response.status();
        tracing::debug!("Ollama response status: {}", status);

        let body = response.bytes().await?;
        tracing::debug!("Ollama response body length: {} bytes", body.len());

        if !status.is_success() {
            let raw = String::from_utf8_lossy(&body);
            let sanitized = super::sanitize_api_error(&raw);
            tracing::error!(
                "Ollama error response: status={} body_excerpt={}",
                status,
                sanitized
            );
            anyhow::bail!(
                "Ollama API error ({}): {}. Is Ollama running? (brew install ollama && ollama serve)",
                status,
                sanitized
            );
        }

        let chat_response: ApiChatResponse = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                let raw = String::from_utf8_lossy(&body);
                let sanitized = super::sanitize_api_error(&raw);
                tracing::error!(
                    "Ollama response deserialization failed: {e}. body_excerpt={}",
                    sanitized
                );
                anyhow::bail!("Failed to parse Ollama response: {e}");
            }
        };

        Ok(chat_response)
    }

    /// Convert Ollama tool calls to the JSON format expected by parse_tool_calls in loop_.rs
    ///
    /// Handles quirky model behavior where tool calls are wrapped:
    /// - `{"name": "tool_call", "arguments": {"name": "shell", "arguments": {...}}}`
    /// - `{"name": "tool.shell", "arguments": {...}}`
    fn format_tool_calls_for_loop(&self, tool_calls: &[OllamaToolCall]) -> String {
        let formatted_calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .map(|tc| {
                let (tool_name, tool_args) = self.extract_tool_name_and_args(tc);

                // Arguments must be a JSON string for parse_tool_calls compatibility
                let args_str =
                    serde_json::to_string(&tool_args).unwrap_or_else(|_| "{}".to_string());

                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": args_str
                    }
                })
            })
            .collect();

        serde_json::json!({
            "content": "",
            "tool_calls": formatted_calls
        })
        .to_string()
    }

    /// Extract the actual tool name and arguments from potentially nested structures
    fn extract_tool_name_and_args(&self, tc: &OllamaToolCall) -> (String, serde_json::Value) {
        let name = &tc.function.name;
        let args = &tc.function.arguments;

        // Pattern 1: Nested tool_call wrapper (various malformed versions)
        // {"name": "tool_call", "arguments": {"name": "shell", "arguments": {"command": "date"}}}
        // {"name": "tool_call><json", "arguments": {"name": "shell", ...}}
        // {"name": "tool.call", "arguments": {"name": "shell", ...}}
        if name == "tool_call"
            || name == "tool.call"
            || name.starts_with("tool_call>")
            || name.starts_with("tool_call<")
        {
            if let Some(nested_name) = args.get("name").and_then(|v| v.as_str()) {
                let nested_args = args
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                tracing::debug!(
                    "Unwrapped nested tool call: {} -> {} with args {:?}",
                    name,
                    nested_name,
                    nested_args
                );
                return (nested_name.to_string(), nested_args);
            }
        }

        // Pattern 2: Prefixed tool name (tool.shell, tool.file_read, etc.)
        if let Some(stripped) = name.strip_prefix("tool.") {
            return (stripped.to_string(), args.clone());
        }

        // Pattern 3: Normal tool call
        (name.clone(), args.clone())
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: sys.to_string(),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        let response = self.send_request(messages, model, temperature).await?;

        // If model returned tool calls, format them for loop_.rs's parse_tool_calls
        if !response.message.tool_calls.is_empty() {
            tracing::debug!(
                "Ollama returned {} tool call(s), formatting for loop parser",
                response.message.tool_calls.len()
            );
            return Ok(self.format_tool_calls_for_loop(&response.message.tool_calls));
        }

        // Plain text response
        let content = response.message.content;

        // Handle edge case: model returned only "thinking" with no content or tool calls
        if content.is_empty() {
            if let Some(thinking) = &response.message.thinking {
                tracing::warn!(
                    "Ollama returned empty content with only thinking: '{}'. Model may have stopped prematurely.",
                    if thinking.len() > 100 { &thinking[..100] } else { thinking }
                );
                return Ok(format!(
                    "I was thinking about this: {}... but I didn't complete my response. Could you try asking again?",
                    if thinking.len() > 200 { &thinking[..200] } else { thinking }
                ));
            }
            tracing::warn!("Ollama returned empty content with no tool calls");
        }

        Ok(content)
    }

    async fn chat_with_history(
        &self,
        messages: &[crate::providers::ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let api_messages: Vec<Message> = messages
            .iter()
            .map(|m| Message {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        let response = self.send_request(api_messages, model, temperature).await?;

        // If model returned tool calls, format them for loop_.rs's parse_tool_calls
        if !response.message.tool_calls.is_empty() {
            tracing::debug!(
                "Ollama returned {} tool call(s), formatting for loop parser",
                response.message.tool_calls.len()
            );
            return Ok(self.format_tool_calls_for_loop(&response.message.tool_calls));
        }

        // Plain text response
        let content = response.message.content;

        // Handle edge case: model returned only "thinking" with no content or tool calls
        // This is a model quirk - it stopped after reasoning without producing output
        if content.is_empty() {
            if let Some(thinking) = &response.message.thinking {
                tracing::warn!(
                    "Ollama returned empty content with only thinking: '{}'. Model may have stopped prematurely.",
                    if thinking.len() > 100 { &thinking[..100] } else { thinking }
                );
                // Return a message indicating the model's thought process but no action
                return Ok(format!(
                    "I was thinking about this: {}... but I didn't complete my response. Could you try asking again?",
                    if thinking.len() > 200 { &thinking[..200] } else { thinking }
                ));
            }
            tracing::warn!("Ollama returned empty content with no tool calls");
        }

        Ok(content)
    }

    fn supports_native_tools(&self) -> bool {
        // Return false since loop_.rs uses XML-style tool parsing via system prompt
        // The model may return native tool_calls but we convert them to JSON format
        // that parse_tool_calls() understands
        false
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url() {
        let p = OllamaProvider::new(None);
        assert_eq!(p.base_url, "http://localhost:11434");
    }

    #[test]
    fn custom_url_trailing_slash() {
        let p = OllamaProvider::new(Some("http://192.168.1.100:11434/"));
        assert_eq!(p.base_url, "http://192.168.1.100:11434");
    }

    #[test]
    fn custom_url_no_trailing_slash() {
        let p = OllamaProvider::new(Some("http://myserver:11434"));
        assert_eq!(p.base_url, "http://myserver:11434");
    }

    #[test]
    fn empty_url_uses_empty() {
        let p = OllamaProvider::new(Some(""));
        assert_eq!(p.base_url, "");
    }

    #[test]
    fn response_deserializes() {
        let json = r#"{"message":{"role":"assistant","content":"Hello from Ollama!"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content, "Hello from Ollama!");
    }

    #[test]
    fn response_with_empty_content() {
        let json = r#"{"message":{"role":"assistant","content":""}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
    }

    #[test]
    fn response_with_missing_content_defaults_to_empty() {
        let json = r#"{"message":{"role":"assistant"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
    }

    #[test]
    fn response_with_thinking_field_extracts_content() {
        let json =
            r#"{"message":{"role":"assistant","content":"hello","thinking":"internal reasoning"}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content, "hello");
    }

    #[test]
    fn response_with_tool_calls_parses_correctly() {
        let json = r#"{"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_123","function":{"name":"shell","arguments":{"command":"date"}}}]}}"#;
        let resp: ApiChatResponse = serde_json::from_str(json).unwrap();
        assert!(resp.message.content.is_empty());
        assert_eq!(resp.message.tool_calls.len(), 1);
        assert_eq!(resp.message.tool_calls[0].function.name, "shell");
    }

    #[test]
    fn extract_tool_name_handles_nested_tool_call() {
        let provider = OllamaProvider::new(None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "tool_call".into(),
                arguments: serde_json::json!({
                    "name": "shell",
                    "arguments": {"command": "date"}
                }),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "shell");
        assert_eq!(args.get("command").unwrap(), "date");
    }

    #[test]
    fn extract_tool_name_handles_prefixed_name() {
        let provider = OllamaProvider::new(None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "tool.shell".into(),
                arguments: serde_json::json!({"command": "ls"}),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "shell");
        assert_eq!(args.get("command").unwrap(), "ls");
    }

    #[test]
    fn extract_tool_name_handles_normal_call() {
        let provider = OllamaProvider::new(None);
        let tc = OllamaToolCall {
            id: Some("call_123".into()),
            function: OllamaFunction {
                name: "file_read".into(),
                arguments: serde_json::json!({"path": "/tmp/test"}),
            },
        };
        let (name, args) = provider.extract_tool_name_and_args(&tc);
        assert_eq!(name, "file_read");
        assert_eq!(args.get("path").unwrap(), "/tmp/test");
    }

    #[test]
    fn format_tool_calls_produces_valid_json() {
        let provider = OllamaProvider::new(None);
        let tool_calls = vec![OllamaToolCall {
            id: Some("call_abc".into()),
            function: OllamaFunction {
                name: "shell".into(),
                arguments: serde_json::json!({"command": "date"}),
            },
        }];

        let formatted = provider.format_tool_calls_for_loop(&tool_calls);
        let parsed: serde_json::Value = serde_json::from_str(&formatted).unwrap();

        assert!(parsed.get("tool_calls").is_some());
        let calls = parsed.get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(calls.len(), 1);

        let func = calls[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap(), "shell");
        // arguments should be a string (JSON-encoded)
        assert!(func.get("arguments").unwrap().is_string());
    }
}
