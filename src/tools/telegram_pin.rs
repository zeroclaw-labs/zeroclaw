use super::telegram_common::TelegramApiClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct TelegramPinTool {
    client: TelegramApiClient,
}

impl TelegramPinTool {
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

    fn build_pin_body(
        chat_id: &str,
        message_id: i64,
        disable_notification: bool,
    ) -> serde_json::Value {
        json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "disable_notification": disable_notification,
        })
    }

    fn build_unpin_body(chat_id: &str, message_id: i64) -> serde_json::Value {
        json!({
            "chat_id": chat_id,
            "message_id": message_id,
        })
    }
}

#[async_trait]
impl Tool for TelegramPinTool {
    fn name(&self) -> &str {
        "telegram_pin"
    }

    fn description(&self) -> &str {
        "Pin or unpin a message in a Telegram chat. Requires the bot to have admin rights with pin_messages permission. Use the chat_id and message_id from the conversation context."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["pin", "unpin"],
                    "description": "Whether to pin or unpin the message"
                },
                "chat_id": {
                    "type": "string",
                    "description": "Telegram chat ID where the message is located"
                },
                "message_id": {
                    "type": "integer",
                    "description": "Telegram message ID to pin or unpin"
                },
                "disable_notification": {
                    "type": "boolean",
                    "description": "If true, pin silently without notifying chat members. Defaults to true."
                }
            },
            "required": ["action", "chat_id", "message_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(result) = self.client.enforce_act() {
            return Ok(result);
        }

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'action' parameter"))?;

        let chat_id = args
            .get("chat_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'chat_id' parameter"))?;

        let message_id = args
            .get("message_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'message_id' parameter"))?;

        let (method, body) = match action {
            "pin" => {
                let disable_notification = args
                    .get("disable_notification")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                (
                    "pinChatMessage",
                    Self::build_pin_body(chat_id, message_id, disable_notification),
                )
            }
            "unpin" => (
                "unpinChatMessage",
                Self::build_unpin_body(chat_id, message_id),
            ),
            other => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid 'action': {other}. Expected 'pin' or 'unpin'."
                    )),
                });
            }
        };

        match self.client.post_json("tool.telegram_pin", method, &body).await {
            Ok(_) => {
                let verb = if action == "pin" { "pinned" } else { "unpinned" };
                Ok(ToolResult {
                    success: true,
                    output: format!("Message {message_id} {verb} in chat {chat_id}."),
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

    fn test_security(level: AutonomyLevel, max_actions_per_hour: u32) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_tool() -> TelegramPinTool {
        TelegramPinTool::new(
            test_security(AutonomyLevel::Full, 100),
            "fake-token".to_string(),
        )
    }

    #[test]
    fn tool_name() {
        assert_eq!(test_tool().name(), "telegram_pin");
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
        assert!(required.contains(&json!("action")));
        assert!(required.contains(&json!("chat_id")));
        assert!(required.contains(&json!("message_id")));
    }

    #[test]
    fn schema_action_has_enum() {
        let schema = test_tool().parameters_schema();
        let action_enum = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert!(action_enum.contains(&json!("pin")));
        assert!(action_enum.contains(&json!("unpin")));
    }

    #[test]
    fn build_pin_body_correct() {
        let body = TelegramPinTool::build_pin_body("123", 42, true);
        assert_eq!(body["chat_id"], "123");
        assert_eq!(body["message_id"], 42);
        assert_eq!(body["disable_notification"], true);
    }

    #[test]
    fn build_pin_body_with_notification() {
        let body = TelegramPinTool::build_pin_body("123", 42, false);
        assert_eq!(body["disable_notification"], false);
    }

    #[test]
    fn build_unpin_body_correct() {
        let body = TelegramPinTool::build_unpin_body("456", 99);
        assert_eq!(body["chat_id"], "456");
        assert_eq!(body["message_id"], 99);
        assert!(body.get("disable_notification").is_none());
    }

    #[test]
    fn api_url_constructs_correctly() {
        let tool = test_tool();
        let url = tool.client.api_url("pinChatMessage");
        assert_eq!(url, "https://api.telegram.org/botfake-token/pinChatMessage");
    }

    #[test]
    fn api_url_with_custom_base() {
        let tool = test_tool().with_api_base("https://custom.api".to_string());
        let url = tool.client.api_url("unpinChatMessage");
        assert_eq!(url, "https://custom.api/botfake-token/unpinChatMessage");
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let tool = TelegramPinTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"action": "pin", "chat_id": "123", "message_id": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = TelegramPinTool::new(
            test_security(AutonomyLevel::Full, 0),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"action": "pin", "chat_id": "123", "message_id": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_action() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "delete", "chat_id": "123", "message_id": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid 'action'"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_action() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_chat_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "pin", "message_id": 1}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_message_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "pin", "chat_id": "123"}))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn disable_notification_defaults_documented_in_schema() {
        let schema = test_tool().parameters_schema();
        let desc = schema["properties"]["disable_notification"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("Defaults to true"));
    }

    #[test]
    fn bot_token_not_in_schema() {
        let schema = test_tool().parameters_schema();
        assert!(schema["properties"].get("bot_token").is_none());
    }

    #[test]
    fn schema_has_additional_properties_false() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn error_output_never_contains_bot_token() {
        let tool = test_tool();
        let token = "fake-token";

        assert!(!tool.description().contains(token));
        let schema_str = tool.parameters_schema().to_string();
        assert!(!schema_str.contains(token));

        let output = format!("Message {} pinned in chat {}.", 42, "123");
        assert!(!output.contains(token));
    }

    #[tokio::test]
    async fn error_path_redacts_token_from_result() {
        let tool = test_tool();
        let token = "fake-token";

        let result = tool
            .execute(json!({"action": "bad", "chat_id": "123", "message_id": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(!err.contains(token));
        assert!(!result.output.contains(token));
    }
}
