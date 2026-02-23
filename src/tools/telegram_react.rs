use super::telegram_common::TelegramApiClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct TelegramReactTool {
    client: TelegramApiClient,
}

impl TelegramReactTool {
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

    fn build_reaction_body(
        chat_id: &str,
        message_id: i64,
        emoji: &str,
    ) -> serde_json::Value {
        json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "reaction": [{"type": "emoji", "emoji": emoji}],
        })
    }
}

#[async_trait]
impl Tool for TelegramReactTool {
    fn name(&self) -> &str {
        "telegram_react"
    }

    fn description(&self) -> &str {
        "Set an emoji reaction on a message in a Telegram chat. Requires the bot to have permission to set reactions. Use the chat_id, message_id, and desired emoji from the conversation context."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "string",
                    "description": "Telegram chat ID where the message is located"
                },
                "message_id": {
                    "type": "integer",
                    "description": "Telegram message ID to react to"
                },
                "emoji": {
                    "type": "string",
                    "description": "Emoji to set as the reaction (e.g. \"\u{1f44d}\", \"\u{2764}\", \"\u{1f525}\")"
                }
            },
            "required": ["chat_id", "message_id", "emoji"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(result) = self.client.enforce_act() {
            return Ok(result);
        }

        let chat_id = args
            .get("chat_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'chat_id' parameter"))?;

        let message_id = args
            .get("message_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'message_id' parameter"))?;

        let emoji = args
            .get("emoji")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'emoji' parameter"))?;

        let body = Self::build_reaction_body(chat_id, message_id, emoji);

        match self
            .client
            .post_json("tool.telegram_react", "setMessageReaction", &body)
            .await
        {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Reaction {emoji} set on message {message_id} in chat {chat_id}."
                ),
                error: None,
            }),
            Err(result) => Ok(result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn test_security(level: AutonomyLevel, max_actions_per_hour: u32) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_tool() -> TelegramReactTool {
        TelegramReactTool::new(
            test_security(AutonomyLevel::Full, 100),
            "fake-token".to_string(),
        )
    }

    #[test]
    fn tool_name() {
        assert_eq!(test_tool().name(), "telegram_react");
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
        assert!(required.contains(&json!("chat_id")));
        assert!(required.contains(&json!("message_id")));
        assert!(required.contains(&json!("emoji")));
    }

    #[test]
    fn schema_has_additional_properties_false() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn schema_chat_id_is_string() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["properties"]["chat_id"]["type"], "string");
    }

    #[test]
    fn schema_message_id_is_integer() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["properties"]["message_id"]["type"], "integer");
    }

    #[test]
    fn schema_emoji_is_string() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["properties"]["emoji"]["type"], "string");
    }

    #[test]
    fn build_reaction_body_correct() {
        let body = TelegramReactTool::build_reaction_body("123", 42, "\u{1f44d}");
        assert_eq!(body["chat_id"], "123");
        assert_eq!(body["message_id"], 42);
        let reaction = body["reaction"].as_array().unwrap();
        assert_eq!(reaction.len(), 1);
        assert_eq!(reaction[0]["type"], "emoji");
        assert_eq!(reaction[0]["emoji"], "\u{1f44d}");
    }

    #[test]
    fn build_reaction_body_preserves_emoji() {
        let body = TelegramReactTool::build_reaction_body("456", 99, "\u{2764}");
        assert_eq!(body["reaction"][0]["emoji"], "\u{2764}");
    }

    #[test]
    fn build_reaction_body_different_chat() {
        let body = TelegramReactTool::build_reaction_body("-100999", 7, "\u{1f525}");
        assert_eq!(body["chat_id"], "-100999");
        assert_eq!(body["message_id"], 7);
        assert_eq!(body["reaction"][0]["emoji"], "\u{1f525}");
    }

    #[test]
    fn api_url_constructs_correctly() {
        let tool = test_tool();
        let url = tool.client.api_url("setMessageReaction");
        assert_eq!(
            url,
            "https://api.telegram.org/botfake-token/setMessageReaction"
        );
    }

    #[test]
    fn api_url_with_custom_base() {
        let tool = test_tool().with_api_base("https://custom.api".to_string());
        let url = tool.client.api_url("setMessageReaction");
        assert_eq!(
            url,
            "https://custom.api/botfake-token/setMessageReaction"
        );
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let tool = TelegramReactTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1, "emoji": "\u{1f44d}"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = TelegramReactTool::new(
            test_security(AutonomyLevel::Full, 0),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1, "emoji": "\u{1f44d}"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_chat_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"message_id": 1, "emoji": "\u{1f44d}"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_message_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"chat_id": "123", "emoji": "\u{1f44d}"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_emoji() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1}))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn bot_token_not_in_schema() {
        let schema = test_tool().parameters_schema();
        assert!(schema["properties"].get("bot_token").is_none());
    }

    #[test]
    fn error_output_never_contains_bot_token() {
        let tool = test_tool();
        let token = "fake-token";

        assert!(!tool.description().contains(token));
        let schema_str = tool.parameters_schema().to_string();
        assert!(!schema_str.contains(token));

        let output = format!(
            "Reaction {} set on message {} in chat {}.",
            "\u{1f44d}", 42, "123"
        );
        assert!(!output.contains(token));
    }

    #[test]
    fn schema_has_exactly_three_properties() {
        let schema = test_tool().parameters_schema();
        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props.len(), 3);
        assert!(props.contains_key("chat_id"));
        assert!(props.contains_key("message_id"));
        assert!(props.contains_key("emoji"));
    }

    #[test]
    fn schema_required_has_exactly_three_entries() {
        let schema = test_tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 3);
    }

    #[test]
    fn reaction_body_has_single_element_array() {
        let body = TelegramReactTool::build_reaction_body("chat", 1, "\u{1f44d}");
        let reaction = body["reaction"].as_array().unwrap();
        assert_eq!(reaction.len(), 1);
    }

    #[test]
    fn reaction_body_type_field_is_emoji() {
        let body = TelegramReactTool::build_reaction_body("chat", 1, "\u{1f44d}");
        assert_eq!(body["reaction"][0]["type"], "emoji");
    }
}
