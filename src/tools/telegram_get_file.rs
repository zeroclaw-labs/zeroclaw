use super::telegram_common::TelegramApiClient;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct TelegramGetFileTool {
    client: TelegramApiClient,
}

impl TelegramGetFileTool {
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

    /// Extract a usable filename from the Telegram file_path, or fall back to file_id.
    fn filename_from_path(file_path: &str, file_id: &str) -> String {
        std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or(file_id)
            .to_string()
    }
}

#[async_trait]
impl Tool for TelegramGetFileTool {
    fn name(&self) -> &str {
        "telegram_get_file"
    }

    fn description(&self) -> &str {
        "Download a file from Telegram by its file_id. Calls the getFile API to resolve the CDN path, downloads the file, and saves it to the workspace downloads directory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_id": {
                    "type": "string",
                    "description": "Telegram file_id to download"
                },
                "save_as": {
                    "type": "string",
                    "description": "Filename to save as. Defaults to the original filename from the Telegram file path, or the file_id if none."
                }
            },
            "required": ["file_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(result) = self.client.enforce_act() {
            return Ok(result);
        }

        let file_id = args
            .get("file_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'file_id' parameter"))?;

        // Step 1: Call getFile to resolve the CDN file_path
        let body = json!({ "file_id": file_id });
        let response = match self
            .client
            .post_json("tool.telegram_get_file", "getFile", &body)
            .await
        {
            Ok(json) => json,
            Err(result) => return Ok(result),
        };

        let remote_file_path = response
            .get("result")
            .and_then(|r| r.get("file_path"))
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow::anyhow!("Telegram getFile response missing result.file_path"))?;

        // Step 2: Download the file from CDN
        let bytes = match self
            .client
            .download_file("tool.telegram_get_file", remote_file_path)
            .await
        {
            Ok(b) => b,
            Err(result) => return Ok(result),
        };

        // Step 3: Save to workspace downloads directory
        let save_as = args
            .get("save_as")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Self::filename_from_path(remote_file_path, file_id));

        let downloads_dir = self.client.workspace_dir().join("downloads");
        tokio::fs::create_dir_all(&downloads_dir).await.map_err(|e| {
            anyhow::anyhow!("Failed to create downloads directory: {e}")
        })?;

        let local_path = downloads_dir.join(&save_as);

        // Validate the final path is within workspace (after dir creation so canonicalize works)
        let validated_path = match self.client.validate_workspace_path(&local_path) {
            Ok(p) => p,
            Err(result) => return Ok(result),
        };

        tokio::fs::write(&validated_path, &bytes).await.map_err(|e| {
            anyhow::anyhow!("Failed to write file: {e}")
        })?;

        Ok(ToolResult {
            success: true,
            output: format!("File downloaded to {}", validated_path.display()),
            error: None,
        })
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

    fn test_tool() -> TelegramGetFileTool {
        TelegramGetFileTool::new(
            test_security(AutonomyLevel::Full, 100),
            "fake-token".to_string(),
        )
    }

    #[test]
    fn tool_name() {
        assert_eq!(test_tool().name(), "telegram_get_file");
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
        assert!(required.contains(&json!("file_id")));
    }

    #[test]
    fn schema_has_additional_properties_false() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn execute_blocks_readonly() {
        let tool = TelegramGetFileTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"file_id": "abc123"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = TelegramGetFileTool::new(
            test_security(AutonomyLevel::Full, 0),
            "fake-token".to_string(),
        );

        let result = tool
            .execute(json!({"file_id": "abc123"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_file_id() {
        let tool = test_tool();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn bot_token_not_in_schema() {
        let schema = test_tool().parameters_schema();
        assert!(schema["properties"].get("bot_token").is_none());
    }

    #[test]
    fn filename_from_path_extracts_basename() {
        assert_eq!(
            TelegramGetFileTool::filename_from_path("documents/file_123.pdf", "fallback_id"),
            "file_123.pdf"
        );
    }

    #[test]
    fn filename_from_path_uses_fallback() {
        assert_eq!(
            TelegramGetFileTool::filename_from_path("", "fallback_id"),
            "fallback_id"
        );
    }

    #[test]
    fn filename_from_path_handles_nested() {
        assert_eq!(
            TelegramGetFileTool::filename_from_path("photos/some/deep/image.jpg", "fid"),
            "image.jpg"
        );
    }

    #[test]
    fn with_api_base_overrides_url() {
        let tool = test_tool().with_api_base("https://custom.test".to_string());
        let url = tool.client.api_url("getFile");
        assert_eq!(url, "https://custom.test/botfake-token/getFile");
    }
}
