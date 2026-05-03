pub use zeroclaw_api::provider::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolSpec;
    use async_trait::async_trait;
    use futures_util::StreamExt;
    use futures_util::stream::{self, BoxStream};

    /// Representative non-zero temperature for default-path chat tests;
    /// mocks ignore it, so any plausible in-range value is fine — this
    /// matches the historical default used across the codebase.
    const TEST_DEFAULT_TEMPERATURE: f64 = 0.7;

    /// Zero = greedy sampling; used by streaming tests where we want
    /// deterministic replays from the mock stream.
    const TEST_GREEDY_TEMPERATURE: f64 = 0.0;

    struct CapabilityMockProvider;

    #[async_trait]
    impl Provider for CapabilityMockProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                vision: true,
                prompt_caching: false,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".into())
        }
    }

    #[test]
    fn chat_message_constructors() {
        let sys = ChatMessage::system("Be helpful");
        assert_eq!(sys.role, "system");
        assert_eq!(sys.content, "Be helpful");

        let user = ChatMessage::user("Hello");
        assert_eq!(user.role, "user");

        let asst = ChatMessage::assistant("Hi there");
        assert_eq!(asst.role, "assistant");

        let tool = ChatMessage::tool("{}");
        assert_eq!(tool.role, "tool");
    }

    #[test]
    fn chat_response_helpers() {
        let empty = ChatResponse {
            text: None,
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        };
        assert!(!empty.has_tool_calls());
        assert_eq!(empty.text_or_empty(), "");

        let with_tools = ChatResponse {
            text: Some("Let me check".into()),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            usage: None,
            reasoning_content: None,
        };
        assert!(with_tools.has_tool_calls());
        assert_eq!(with_tools.text_or_empty(), "Let me check");
    }

    #[test]
    fn token_usage_default_is_none() {
        let usage = TokenUsage::default();
        assert!(usage.input_tokens.is_none());
        assert!(usage.output_tokens.is_none());
    }

    #[test]
    fn chat_response_with_usage() {
        let resp = ChatResponse {
            text: Some("Hello".into()),
            tool_calls: vec![],
            usage: Some(TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(50),
                cached_input_tokens: None,
            }),
            reasoning_content: None,
        };
        assert_eq!(resp.usage.as_ref().unwrap().input_tokens, Some(100));
        assert_eq!(resp.usage.as_ref().unwrap().output_tokens, Some(50));
    }

    #[test]
    fn tool_call_serialization() {
        let tc = ToolCall {
            id: "call_123".into(),
            name: "file_read".into(),
            arguments: r#"{"path":"test.txt"}"#.into(),
            extra_content: None,
        };
        let json = serde_json::to_string(&tc).unwrap();
        assert!(json.contains("call_123"));
        assert!(json.contains("file_read"));
    }

    #[test]
    fn conversation_message_variants() {
        let chat = ConversationMessage::Chat(ChatMessage::user("hi"));
        let json = serde_json::to_string(&chat).unwrap();
        assert!(json.contains("\"type\":\"Chat\""));

        let tool_result = ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "1".into(),
            content: "done".into(),
        }]);
        let json = serde_json::to_string(&tool_result).unwrap();
        assert!(json.contains("\"type\":\"ToolResults\""));
    }

    #[test]
    fn provider_capabilities_default() {
        let caps = ProviderCapabilities::default();
        assert!(!caps.native_tool_calling);
        assert!(!caps.vision);
    }

    #[test]
    fn provider_capabilities_equality() {
        let caps1 = ProviderCapabilities {
            native_tool_calling: true,
            vision: false,
            prompt_caching: false,
        };
        let caps2 = ProviderCapabilities {
            native_tool_calling: true,
            vision: false,
            prompt_caching: false,
        };
        let caps3 = ProviderCapabilities {
            native_tool_calling: false,
            vision: false,
            prompt_caching: false,
        };

        assert_eq!(caps1, caps2);
        assert_ne!(caps1, caps3);
    }

    #[test]
    fn supports_native_tools_reflects_capabilities_default_mapping() {
        let provider = CapabilityMockProvider;
        assert!(provider.supports_native_tools());
    }

    #[test]
    fn supports_vision_reflects_capabilities_default_mapping() {
        let provider = CapabilityMockProvider;
        assert!(provider.supports_vision());
    }

    #[test]
    fn tools_payload_variants() {
        let gemini = ToolsPayload::Gemini {
            function_declarations: vec![serde_json::json!({"name": "test"})],
        };
        assert!(matches!(gemini, ToolsPayload::Gemini { .. }));

        let anthropic = ToolsPayload::Anthropic {
            tools: vec![serde_json::json!({"name": "test"})],
        };
        assert!(matches!(anthropic, ToolsPayload::Anthropic { .. }));

        let openai = ToolsPayload::OpenAI {
            tools: vec![serde_json::json!({"type": "function"})],
        };
        assert!(matches!(openai, ToolsPayload::OpenAI { .. }));

        let prompt_guided = ToolsPayload::PromptGuided {
            instructions: "Use tools...".to_string(),
        };
        assert!(matches!(prompt_guided, ToolsPayload::PromptGuided { .. }));
    }

    #[test]
    fn build_tool_instructions_text_format() {
        let tools = vec![
            ToolSpec {
                name: "shell".to_string(),
                description: "Execute commands".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"}
                    }
                }),
            },
            ToolSpec {
                name: "file_read".to_string(),
                description: "Read files".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    }
                }),
            },
        ];

        let instructions = build_tool_instructions_text(&tools);

        assert!(instructions.contains("Tool Use Protocol"));
        assert!(instructions.contains("<tool_call>"));
        assert!(instructions.contains("</tool_call>"));
        assert!(instructions.contains("**shell**"));
        assert!(instructions.contains("Execute commands"));
        assert!(instructions.contains("**file_read**"));
        assert!(instructions.contains("Read files"));
        assert!(instructions.contains("Parameters:"));
        assert!(instructions.contains(r#""type":"object""#));
    }

    #[test]
    fn build_tool_instructions_text_empty() {
        let instructions = build_tool_instructions_text(&[]);
        assert!(instructions.contains("Tool Use Protocol"));
        assert!(instructions.contains("Available Tools"));
    }

    struct MockProvider {
        supports_native: bool,
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn supports_native_tools(&self) -> bool {
            self.supports_native
        }

        async fn chat_with_system(
            &self,
            _system: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("response".to_string())
        }
    }

    #[test]
    fn provider_convert_tools_default() {
        let provider = MockProvider {
            supports_native: false,
        };

        let tools = vec![ToolSpec {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];

        let payload = provider.convert_tools(&tools);
        assert!(matches!(payload, ToolsPayload::PromptGuided { .. }));

        if let ToolsPayload::PromptGuided { instructions } = payload {
            assert!(instructions.contains("test_tool"));
            assert!(instructions.contains("A test tool"));
        }
    }

    #[tokio::test]
    async fn provider_chat_prompt_guided_fallback() {
        let provider = MockProvider {
            supports_native: false,
        };

        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run commands".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];

        let request = ChatRequest {
            messages: &[ChatMessage::user("Hello")],
            tools: Some(&tools),
        };

        let response = provider
            .chat(request, "model", Some(TEST_DEFAULT_TEMPERATURE))
            .await
            .unwrap();
        assert!(response.text.is_some());
    }

    #[tokio::test]
    async fn provider_chat_without_tools() {
        let provider = MockProvider {
            supports_native: true,
        };

        let request = ChatRequest {
            messages: &[ChatMessage::user("Hello")],
            tools: None,
        };

        let response = provider
            .chat(request, "model", Some(TEST_DEFAULT_TEMPERATURE))
            .await
            .unwrap();
        assert!(response.text.is_some());
    }

    struct EchoSystemProvider {
        supports_native: bool,
    }

    #[async_trait]
    impl Provider for EchoSystemProvider {
        fn supports_native_tools(&self) -> bool {
            self.supports_native
        }

        async fn chat_with_system(
            &self,
            system: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(system.unwrap_or_default().to_string())
        }
    }

    struct CustomConvertProvider;

    #[async_trait]
    impl Provider for CustomConvertProvider {
        fn supports_native_tools(&self) -> bool {
            false
        }

        fn convert_tools(&self, _tools: &[ToolSpec]) -> ToolsPayload {
            ToolsPayload::PromptGuided {
                instructions: "CUSTOM_TOOL_INSTRUCTIONS".to_string(),
            }
        }

        async fn chat_with_system(
            &self,
            system: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(system.unwrap_or_default().to_string())
        }
    }

    struct InvalidConvertProvider;

    #[async_trait]
    impl Provider for InvalidConvertProvider {
        fn supports_native_tools(&self) -> bool {
            false
        }

        fn convert_tools(&self, _tools: &[ToolSpec]) -> ToolsPayload {
            ToolsPayload::OpenAI {
                tools: vec![serde_json::json!({"type": "function"})],
            }
        }

        async fn chat_with_system(
            &self,
            _system: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("should_not_reach".to_string())
        }
    }

    #[tokio::test]
    async fn provider_chat_prompt_guided_preserves_existing_system_not_first() {
        let provider = EchoSystemProvider {
            supports_native: false,
        };

        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run commands".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];

        let request = ChatRequest {
            messages: &[
                ChatMessage::user("Hello"),
                ChatMessage::system("BASE_SYSTEM_PROMPT"),
            ],
            tools: Some(&tools),
        };

        let response = provider
            .chat(request, "model", Some(TEST_DEFAULT_TEMPERATURE))
            .await
            .unwrap();
        let text = response.text.unwrap_or_default();

        assert!(text.contains("BASE_SYSTEM_PROMPT"));
        assert!(text.contains("Tool Use Protocol"));
    }

    #[tokio::test]
    async fn provider_chat_prompt_guided_uses_convert_tools_override() {
        let provider = CustomConvertProvider;

        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run commands".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];

        let request = ChatRequest {
            messages: &[ChatMessage::system("BASE"), ChatMessage::user("Hello")],
            tools: Some(&tools),
        };

        let response = provider
            .chat(request, "model", Some(TEST_DEFAULT_TEMPERATURE))
            .await
            .unwrap();
        let text = response.text.unwrap_or_default();

        assert!(text.contains("BASE"));
        assert!(text.contains("CUSTOM_TOOL_INSTRUCTIONS"));
    }

    #[tokio::test]
    async fn provider_chat_prompt_guided_rejects_non_prompt_payload() {
        let provider = InvalidConvertProvider;

        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "Run commands".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];

        let request = ChatRequest {
            messages: &[ChatMessage::user("Hello")],
            tools: Some(&tools),
        };

        let err = provider
            .chat(request, "model", Some(TEST_DEFAULT_TEMPERATURE))
            .await
            .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("non-prompt-guided"));
    }

    struct StreamingChunkOnlyProvider;

    #[async_trait]
    impl Provider for StreamingChunkOnlyProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamChunk>> {
            stream::iter(vec![
                Ok(StreamChunk::delta("hello")),
                Ok(StreamChunk::final_chunk()),
            ])
            .boxed()
        }
    }

    #[tokio::test]
    async fn provider_stream_chat_default_maps_legacy_chunks_to_events() {
        let provider = StreamingChunkOnlyProvider;
        let mut stream = provider.stream_chat(
            ChatRequest {
                messages: &[ChatMessage::user("hi")],
                tools: None,
            },
            "model",
            Some(TEST_GREEDY_TEMPERATURE),
            StreamOptions::new(true),
        );

        let first = stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();
        assert!(stream.next().await.is_none());

        match first {
            StreamEvent::TextDelta(chunk) => assert_eq!(chunk.delta, "hello"),
            other => panic!("expected text delta event, got {other:?}"),
        }
        assert!(matches!(second, StreamEvent::Final));
    }
}
