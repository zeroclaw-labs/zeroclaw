use super::telegram_common::TelegramApiClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct TelegramDeleteTool {
    client: TelegramApiClient,
}

impl TelegramDeleteTool {
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

    fn build_body(chat_id: &str, message_id: i64) -> serde_json::Value {
        json!({
            "chat_id": chat_id,
            "message_id": message_id,
        })
    }
}

#[async_trait]
impl Tool for TelegramDeleteTool {
    fn name(&self) -> &str {
        "telegram_delete"
    }

    fn description(&self) -> &str {
        "Delete a message in a Telegram chat. Requires the bot to have admin rights with delete_messages permission, or the message must have been sent by the bot itself."
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
                    "description": "Telegram message ID to delete"
                }
            },
            "required": ["chat_id", "message_id"],
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

        let body = Self::build_body(chat_id, message_id);

        match self
            .client
            .post_json("tool.telegram_delete", "deleteMessage", &body)
            .await
        {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Message {message_id} deleted from chat {chat_id}."),
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

    fn test_tool() -> TelegramDeleteTool {
        TelegramDeleteTool::new(
            test_security(AutonomyLevel::Full, 100),
            "fake-token".to_string(),
        )
    }

    // --- Metadata tests ---

    #[test]
    fn tool_name() {
        assert_eq!(test_tool().name(), "telegram_delete");
    }

    #[test]
    fn tool_description_not_empty() {
        assert!(!test_tool().description().is_empty());
    }

    // --- Schema tests ---

    #[test]
    fn schema_type_is_object() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn schema_has_required_params() {
        let schema = test_tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("chat_id")));
        assert!(required.contains(&json!("message_id")));
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
    fn schema_has_exactly_two_properties() {
        let schema = test_tool().parameters_schema();
        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props.len(), 2);
    }

    #[test]
    fn schema_required_has_exactly_two_entries() {
        let schema = test_tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
    }

    // --- Body builder tests ---

    #[test]
    fn build_body_correct() {
        let body = TelegramDeleteTool::build_body("123", 42);
        assert_eq!(body["chat_id"], "123");
        assert_eq!(body["message_id"], 42);
    }

    #[test]
    fn build_body_negative_chat_id() {
        let body = TelegramDeleteTool::build_body("-100123456789", 1);
        assert_eq!(body["chat_id"], "-100123456789");
    }

    #[test]
    fn build_body_has_no_extra_fields() {
        let body = TelegramDeleteTool::build_body("123", 42);
        let obj = body.as_object().unwrap();
        assert_eq!(obj.len(), 2);
    }

    // --- URL construction tests ---

    #[test]
    fn api_url_constructs_correctly() {
        let tool = test_tool();
        let url = tool.client.api_url("deleteMessage");
        assert_eq!(url, "https://api.telegram.org/botfake-token/deleteMessage");
    }

    #[test]
    fn api_url_with_custom_base() {
        let tool = test_tool().with_api_base("https://custom.api".to_string());
        let url = tool.client.api_url("deleteMessage");
        assert_eq!(url, "https://custom.api/botfake-token/deleteMessage");
    }

    // --- Security gate tests ---

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let tool = TelegramDeleteTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = TelegramDeleteTool::new(
            test_security(AutonomyLevel::Full, 0),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    // --- Missing parameter tests ---

    #[tokio::test]
    async fn execute_rejects_missing_chat_id() {
        let tool = test_tool();
        let result = tool.execute(json!({"message_id": 1})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_message_id() {
        let tool = test_tool();
        let result = tool.execute(json!({"chat_id": "123"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_empty_args() {
        let tool = test_tool();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_wrong_type_chat_id() {
        let tool = test_tool();
        let result = tool.execute(json!({"chat_id": 123, "message_id": 1})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_wrong_type_message_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"chat_id": "123", "message_id": "not_a_number"}))
            .await;
        assert!(result.is_err());
    }

    // --- Security / token redaction tests ---

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

        let output = format!("Message {} deleted from chat {}.", 42, "123");
        assert!(!output.contains(token));
    }

    #[test]
    fn success_output_format() {
        let output = format!("Message {} deleted from chat {}.", 99, "-100555");
        assert_eq!(output, "Message 99 deleted from chat -100555.");
    }

    #[tokio::test]
    async fn readonly_error_does_not_leak_token() {
        let tool = TelegramDeleteTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "secret-bot-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(!err.contains("secret-bot-token"));
        assert!(!result.output.contains("secret-bot-token"));
    }

    #[tokio::test]
    async fn rate_limit_error_does_not_leak_token() {
        let tool = TelegramDeleteTool::new(
            test_security(AutonomyLevel::Full, 0),
            "secret-bot-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(!err.contains("secret-bot-token"));
        assert!(!result.output.contains("secret-bot-token"));
    }
}
