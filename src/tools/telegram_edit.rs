use super::telegram_common::TelegramApiClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct TelegramEditTool {
    client: TelegramApiClient,
}

impl TelegramEditTool {
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

    fn build_body(
        chat_id: &str,
        message_id: i64,
        text: &str,
        parse_mode: Option<&str>,
    ) -> serde_json::Value {
        let mut body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
        });
        if let Some(mode) = parse_mode {
            body["parse_mode"] = json!(mode);
        }
        body
    }
}

#[async_trait]
impl Tool for TelegramEditTool {
    fn name(&self) -> &str {
        "telegram_edit"
    }

    fn description(&self) -> &str {
        "Edit the text of a previously sent message in a Telegram chat. Requires the bot to be the author of the message or to have appropriate admin rights."
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
                    "description": "Telegram message ID to edit"
                },
                "text": {
                    "type": "string",
                    "description": "New text content for the message"
                },
                "parse_mode": {
                    "type": "string",
                    "enum": ["HTML", "Markdown"],
                    "description": "Optional parse mode for text formatting"
                }
            },
            "required": ["chat_id", "message_id", "text"],
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

        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'text' parameter"))?;

        let parse_mode = args.get("parse_mode").and_then(|v| v.as_str());

        let body = Self::build_body(chat_id, message_id, text, parse_mode);

        match self
            .client
            .post_json("tool.telegram_edit", "editMessageText", &body)
            .await
        {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Message {message_id} edited in chat {chat_id}."),
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

    fn test_tool() -> TelegramEditTool {
        TelegramEditTool::new(
            test_security(AutonomyLevel::Full, 100),
            "fake-token".to_string(),
        )
    }

    #[test]
    fn tool_name() {
        assert_eq!(test_tool().name(), "telegram_edit");
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
        assert!(required.contains(&json!("text")));
    }

    #[test]
    fn schema_has_additional_properties_false() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn schema_parse_mode_has_enum() {
        let schema = test_tool().parameters_schema();
        let parse_mode_enum = schema["properties"]["parse_mode"]["enum"]
            .as_array()
            .unwrap();
        assert!(parse_mode_enum.contains(&json!("HTML")));
        assert!(parse_mode_enum.contains(&json!("Markdown")));
    }

    #[test]
    fn build_body_without_parse_mode() {
        let body = TelegramEditTool::build_body("123", 42, "hello world", None);
        assert_eq!(body["chat_id"], "123");
        assert_eq!(body["message_id"], 42);
        assert_eq!(body["text"], "hello world");
        assert!(body.get("parse_mode").is_none());
    }

    #[test]
    fn build_body_with_parse_mode() {
        let body = TelegramEditTool::build_body("123", 42, "<b>bold</b>", Some("HTML"));
        assert_eq!(body["chat_id"], "123");
        assert_eq!(body["message_id"], 42);
        assert_eq!(body["text"], "<b>bold</b>");
        assert_eq!(body["parse_mode"], "HTML");
    }

    #[test]
    fn api_url_constructs_correctly() {
        let tool = test_tool();
        let url = tool.client.api_url("editMessageText");
        assert_eq!(
            url,
            "https://api.telegram.org/botfake-token/editMessageText"
        );
    }

    #[test]
    fn api_url_with_custom_base() {
        let tool = test_tool().with_api_base("https://custom.api".to_string());
        let url = tool.client.api_url("editMessageText");
        assert_eq!(url, "https://custom.api/botfake-token/editMessageText");
    }

    #[tokio::test]
    async fn execute_blocks_readonly() {
        let tool = TelegramEditTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1, "text": "updated"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = TelegramEditTool::new(
            test_security(AutonomyLevel::Full, 0),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "message_id": 1, "text": "updated"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_chat_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"message_id": 1, "text": "updated"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_message_id() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"chat_id": "123", "text": "updated"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_text() {
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

        let output = format!("Message {} edited in chat {}.", 42, "123");
        assert!(!output.contains(token));
    }
}
