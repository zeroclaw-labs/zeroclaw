use super::telegram_common::TelegramApiClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct TelegramGetChatTool {
    client: TelegramApiClient,
}

impl TelegramGetChatTool {
    pub fn new(security: Arc<SecurityPolicy>, bot_token: String) -> Self {
        Self {
            client: TelegramApiClient::new(security, bot_token),
        }
    }

    #[cfg(test)]
    fn with_api_base(mut self, api_base: String) -> Self {
        self.client = self.client.with_api_base(api_base);
        self
    }
}

#[async_trait]
impl Tool for TelegramGetChatTool {
    fn name(&self) -> &str {
        "telegram_get_chat"
    }

    fn description(&self) -> &str {
        "Retrieve information about a Telegram chat by its ID. Returns chat metadata including title, type, description, and other details."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "string",
                    "description": "Telegram chat ID to retrieve information for"
                }
            },
            "required": ["chat_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let chat_id = args
            .get("chat_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'chat_id' parameter"))?;

        let body = json!({ "chat_id": chat_id });

        match self
            .client
            .post_json("tool.telegram_get_chat", "getChat", &body)
            .await
        {
            Ok(response) => {
                let result = response
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let pretty =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                Ok(ToolResult {
                    success: true,
                    output: pretty,
                    error: None,
                })
            }
            Err(result) => Ok(result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 100,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_tool() -> TelegramGetChatTool {
        TelegramGetChatTool::new(test_security(), "fake-token".to_string())
    }

    #[test]
    fn tool_name() {
        assert_eq!(test_tool().name(), "telegram_get_chat");
    }

    #[test]
    fn tool_description_not_empty() {
        assert!(!test_tool().description().is_empty());
    }

    #[test]
    fn schema_has_required_params() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert!(required.contains(&json!("chat_id")));
    }

    #[test]
    fn schema_has_additional_properties_false() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn execute_rejects_missing_chat_id() {
        let tool = test_tool();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn bot_token_not_in_schema() {
        let schema = test_tool().parameters_schema();
        assert!(schema["properties"].get("bot_token").is_none());
    }
}
