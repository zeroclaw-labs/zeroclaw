use async_trait::async_trait;

use super::traits::{
    build_tool_instructions_text, ChatMessage, ChatResponse, ProviderCapabilities, ToolsPayload,
};
use crate::tools::ToolSpec;

#[async_trait]
pub trait ProviderToolCalling: Send + Sync {
    fn tool_capabilities(&self) -> ProviderCapabilities;

    fn supports_native_tools(&self) -> bool {
        self.tool_capabilities().native_tool_calling
    }

    fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
        ToolsPayload::PromptGuided {
            instructions: build_tool_instructions_text(tools),
        }
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockToolProvider;

    #[async_trait]
    impl ProviderToolCalling for MockToolProvider {
        fn tool_capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
            }
        }

        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("tool response".into()),
                tool_calls: vec![],
                usage: None,
            })
        }
    }

    struct NoToolProvider;

    #[async_trait]
    impl ProviderToolCalling for NoToolProvider {
        fn tool_capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }

        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("native tool calling not supported")
        }
    }

    #[test]
    fn mock_tool_provider_supports_native() {
        let p = MockToolProvider;
        assert!(p.supports_native_tools());
    }

    #[test]
    fn no_tool_provider_does_not_support_native() {
        let p = NoToolProvider;
        assert!(!p.supports_native_tools());
    }

    #[test]
    fn default_convert_tools_returns_prompt_guided() {
        let p = NoToolProvider;
        let tools = vec![ToolSpec {
            name: "test".into(),
            description: "A test tool".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let payload = p.convert_tools(&tools);
        assert!(matches!(payload, ToolsPayload::PromptGuided { .. }));
    }

    #[tokio::test]
    async fn mock_tool_provider_returns_response() {
        let p = MockToolProvider;
        let messages = vec![ChatMessage::user("test")];
        let tools = vec![serde_json::json!({"type": "function"})];
        let resp = p
            .chat_with_tools(&messages, &tools, "model", 0.7)
            .await
            .unwrap();
        assert_eq!(resp.text.as_deref(), Some("tool response"));
    }
}
