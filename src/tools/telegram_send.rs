use super::telegram_common::TelegramApiClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

pub struct TelegramSendTool {
    client: TelegramApiClient,
}

impl TelegramSendTool {
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
impl Tool for TelegramSendTool {
    fn name(&self) -> &str {
        "telegram_send"
    }

    fn description(&self) -> &str {
        "Send a text message or upload a file (document or photo) to a Telegram chat."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "string",
                    "description": "Telegram chat ID to send the message or file to"
                },
                "text": {
                    "type": "string",
                    "description": "Text content of the message. Mutually exclusive with file_path."
                },
                "file_path": {
                    "type": "string",
                    "description": "Local file path within the workspace to upload. Mutually exclusive with text."
                },
                "file_type": {
                    "type": "string",
                    "enum": ["document", "photo"],
                    "description": "How to send the file: as a document or as a photo. Defaults to 'document'."
                },
                "caption": {
                    "type": "string",
                    "description": "Caption for file messages"
                },
                "parse_mode": {
                    "type": "string",
                    "enum": ["HTML", "Markdown"],
                    "description": "Parse mode for text formatting"
                },
                "reply_to_message_id": {
                    "type": "integer",
                    "description": "Message ID to reply to"
                }
            },
            "required": ["chat_id"],
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

        let text = args.get("text").and_then(|v| v.as_str());
        let file_path = args.get("file_path").and_then(|v| v.as_str());

        // Exactly one of text or file_path must be provided.
        match (text, file_path) {
            (Some(_), Some(_)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Exactly one of 'text' or 'file_path' must be provided, not both."
                            .into(),
                    ),
                });
            }
            (None, None) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Exactly one of 'text' or 'file_path' must be provided.".into(),
                    ),
                });
            }
            _ => {}
        }

        let parse_mode = args.get("parse_mode").and_then(|v| v.as_str());
        let reply_to = args.get("reply_to_message_id").and_then(|v| v.as_i64());

        if let Some(msg_text) = text {
            // --- Text message path ---
            let mut body = json!({
                "chat_id": chat_id,
                "text": msg_text,
            });
            if let Some(pm) = parse_mode {
                body["parse_mode"] = json!(pm);
            }
            if let Some(reply_id) = reply_to {
                body["reply_to_message_id"] = json!(reply_id);
            }

            match self
                .client
                .post_json("tool.telegram_send", "sendMessage", &body)
                .await
            {
                Ok(resp) => {
                    let mid = resp
                        .get("result")
                        .and_then(|r| r.get("message_id"))
                        .and_then(|v| v.as_i64());
                    let output = match mid {
                        Some(id) => format!("Message sent to chat {chat_id}. message_id={id}"),
                        None => format!("Message sent to chat {chat_id}."),
                    };
                    Ok(ToolResult {
                        success: true,
                        output,
                        error: None,
                    })
                }
                Err(result) => Ok(result),
            }
        } else {
            // --- File upload path ---
            let raw_path = file_path.unwrap();
            let local_path = Path::new(raw_path);

            let canonical = match self.client.validate_workspace_path(local_path) {
                Ok(p) => p,
                Err(result) => return Ok(result),
            };

            let file_bytes = tokio::fs::read(&canonical).await.map_err(|e| {
                anyhow::anyhow!("Failed to read file '{}': {e}", canonical.display())
            })?;

            let file_name = canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "upload".to_string());

            let file_type = args
                .get("file_type")
                .and_then(|v| v.as_str())
                .unwrap_or("document");

            let (method, field_name) = match file_type {
                "photo" => ("sendPhoto", "photo"),
                _ => ("sendDocument", "document"),
            };

            let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);

            let mut form = reqwest::multipart::Form::new()
                .text("chat_id", chat_id.to_string())
                .part(field_name, part);

            if let Some(caption) = args.get("caption").and_then(|v| v.as_str()) {
                form = form.text("caption", caption.to_string());
            }
            if let Some(pm) = parse_mode {
                form = form.text("parse_mode", pm.to_string());
            }
            if let Some(reply_id) = reply_to {
                form = form.text("reply_to_message_id", reply_id.to_string());
            }

            match self
                .client
                .post_multipart("tool.telegram_send", method, form)
                .await
            {
                Ok(resp) => {
                    let mid = resp
                        .get("result")
                        .and_then(|r| r.get("message_id"))
                        .and_then(|v| v.as_i64());
                    let output = match mid {
                        Some(id) => format!("File sent to chat {chat_id}. message_id={id}"),
                        None => format!("File sent to chat {chat_id}."),
                    };
                    Ok(ToolResult {
                        success: true,
                        output,
                        error: None,
                    })
                }
                Err(result) => Ok(result),
            }
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

    fn test_tool() -> TelegramSendTool {
        TelegramSendTool::new(
            test_security(AutonomyLevel::Full, 100),
            "fake-token".to_string(),
        )
    }

    #[test]
    fn tool_name() {
        assert_eq!(test_tool().name(), "telegram_send");
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
    }

    #[test]
    fn schema_has_additional_properties_false() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn execute_blocks_readonly() {
        let tool = TelegramSendTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "text": "hello"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = TelegramSendTool::new(
            test_security(AutonomyLevel::Full, 0),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"chat_id": "123", "text": "hello"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_chat_id() {
        let tool = test_tool();
        let result = tool.execute(json!({"text": "hello"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_both_text_and_file() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"chat_id": "123", "text": "hello", "file_path": "/tmp/f.txt"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not both"));
    }

    #[tokio::test]
    async fn execute_rejects_neither_text_nor_file() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"chat_id": "123"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("Exactly one"));
    }

    #[test]
    fn bot_token_not_in_schema() {
        let schema = test_tool().parameters_schema();
        assert!(schema["properties"].get("bot_token").is_none());
    }
}
