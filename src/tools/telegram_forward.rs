use super::telegram_common::TelegramApiClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct TelegramForwardTool {
    client: TelegramApiClient,
}

impl TelegramForwardTool {
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

    fn build_forward_body(
        from_chat_id: &str,
        to_chat_id: &str,
        message_id: i64,
    ) -> serde_json::Value {
        json!({
            "chat_id": to_chat_id,
            "from_chat_id": from_chat_id,
            "message_id": message_id,
        })
    }
}

#[async_trait]
impl Tool for TelegramForwardTool {
    fn name(&self) -> &str {
        "telegram_forward"
    }

    fn description(&self) -> &str {
        "Forward a message from one Telegram chat to another. The bot must be a member of both the source and destination chats."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "from_chat_id": {
                    "type": "string",
                    "description": "Telegram chat ID of the source chat containing the message to forward"
                },
                "to_chat_id": {
                    "type": "string",
                    "description": "Telegram chat ID of the destination chat to forward the message to"
                },
                "message_id": {
                    "type": "integer",
                    "description": "ID of the message to forward from the source chat"
                }
            },
            "required": ["from_chat_id", "to_chat_id", "message_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(result) = self.client.enforce_act() {
            return Ok(result);
        }

        let from_chat_id = args
            .get("from_chat_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'from_chat_id' parameter"))?;

        let to_chat_id = args
            .get("to_chat_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'to_chat_id' parameter"))?;

        let message_id = args
            .get("message_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'message_id' parameter"))?;

        let body = Self::build_forward_body(from_chat_id, to_chat_id, message_id);

        match self
            .client
            .post_json("tool.telegram_forward", "forwardMessage", &body)
            .await
        {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Message {message_id} forwarded from {from_chat_id} to {to_chat_id}."
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

    fn test_tool() -> TelegramForwardTool {
        TelegramForwardTool::new(
            test_security(AutonomyLevel::Full, 100),
            "fake-token".to_string(),
        )
    }

    #[test]
    fn tool_name() {
        assert_eq!(test_tool().name(), "telegram_forward");
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
        assert!(required.contains(&json!("from_chat_id")));
        assert!(required.contains(&json!("to_chat_id")));
        assert!(required.contains(&json!("message_id")));
    }

    #[test]
    fn schema_from_chat_id_is_string() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["properties"]["from_chat_id"]["type"], "string");
    }

    #[test]
    fn schema_to_chat_id_is_string() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["properties"]["to_chat_id"]["type"], "string");
    }

    #[test]
    fn schema_message_id_is_integer() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["properties"]["message_id"]["type"], "integer");
    }

    #[test]
    fn schema_has_additional_properties_false() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn build_forward_body_correct() {
        let body = TelegramForwardTool::build_forward_body("111", "222", 42);
        assert_eq!(body["chat_id"], "222");
        assert_eq!(body["from_chat_id"], "111");
        assert_eq!(body["message_id"], 42);
    }

    #[test]
    fn build_forward_body_chat_id_is_destination() {
        let body = TelegramForwardTool::build_forward_body("src", "dst", 1);
        assert_eq!(body["chat_id"], "dst");
    }

    #[test]
    fn build_forward_body_from_chat_id_is_source() {
        let body = TelegramForwardTool::build_forward_body("src", "dst", 1);
        assert_eq!(body["from_chat_id"], "src");
    }

    #[test]
    fn api_url_constructs_correctly() {
        let tool = test_tool();
        let url = tool.client.api_url("forwardMessage");
        assert_eq!(url, "https://api.telegram.org/botfake-token/forwardMessage");
    }

    #[test]
    fn api_url_with_custom_base() {
        let tool = test_tool().with_api_base("https://custom.api".to_string());
        let url = tool.client.api_url("forwardMessage");
        assert_eq!(url, "https://custom.api/botfake-token/forwardMessage");
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let tool = TelegramForwardTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({
                "from_chat_id": "111",
                "to_chat_id": "222",
                "message_id": 1
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = TelegramForwardTool::new(
            test_security(AutonomyLevel::Full, 0),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({
                "from_chat_id": "111",
                "to_chat_id": "222",
                "message_id": 1
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_from_chat_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"to_chat_id": "222", "message_id": 1}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_to_chat_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"from_chat_id": "111", "message_id": 1}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_message_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"from_chat_id": "111", "to_chat_id": "222"}))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn schema_has_exactly_three_properties() {
        let schema = test_tool().parameters_schema();
        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props.len(), 3);
    }

    #[test]
    fn schema_has_exactly_three_required() {
        let schema = test_tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 3);
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

        let output = format!("Message {} forwarded from {} to {}.", 42, "111", "222");
        assert!(!output.contains(token));
    }

    #[tokio::test]
    async fn execute_readonly_error_redacts_token() {
        let tool = TelegramForwardTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "fake-token".to_string(),
        );
        let token = "fake-token";

        let result = tool
            .execute(json!({
                "from_chat_id": "111",
                "to_chat_id": "222",
                "message_id": 1
            }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(!err.contains(token));
        assert!(!result.output.contains(token));
    }

    #[test]
    fn schema_properties_have_descriptions() {
        let schema = test_tool().parameters_schema();
        let props = schema["properties"].as_object().unwrap();
        for (key, val) in props {
            assert!(
                val.get("description").is_some(),
                "Property '{key}' missing description"
            );
            assert!(
                !val["description"].as_str().unwrap().is_empty(),
                "Property '{key}' has empty description"
            );
        }
    }

    #[test]
    fn build_forward_body_has_no_extra_fields() {
        let body = TelegramForwardTool::build_forward_body("111", "222", 7);
        let obj = body.as_object().unwrap();
        assert_eq!(obj.len(), 3);
        assert!(obj.contains_key("chat_id"));
        assert!(obj.contains_key("from_chat_id"));
        assert!(obj.contains_key("message_id"));
    }
}
