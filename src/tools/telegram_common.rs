use crate::security::SecurityPolicy;
use crate::tools::traits::ToolResult;
use std::sync::Arc;

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
const TELEGRAM_REQUEST_TIMEOUT_SECS: u64 = 15;

pub struct TelegramApiClient {
    security: Arc<SecurityPolicy>,
    bot_token: String,
    api_base: String,
}

impl TelegramApiClient {
    pub fn new(security: Arc<SecurityPolicy>, bot_token: String) -> Self {
        Self {
            security,
            bot_token,
            api_base: TELEGRAM_API_BASE.to_string(),
        }
    }

    #[cfg(test)]
    pub fn with_api_base(mut self, api_base: String) -> Self {
        self.api_base = api_base;
        self
    }

    pub fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.api_base, self.bot_token)
    }

    pub fn file_url(&self, file_path: &str) -> String {
        format!("{}/file/bot{}/{file_path}", self.api_base, self.bot_token)
    }

    /// Check security gate (autonomy + rate limit). Returns Ok(()) if allowed,
    /// or Err(ToolResult) with the rejection reason.
    pub fn enforce_act(&self) -> Result<(), ToolResult> {
        if !self.security.can_act() {
            return Err(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }
        if !self.security.record_action() {
            return Err(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }
        Ok(())
    }

    /// POST a JSON body to a Telegram API method. Returns the parsed JSON response.
    /// On failure, returns a ToolResult with a redacted error (no bot token).
    pub async fn post_json(
        &self,
        service_key: &str,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ToolResult> {
        let url = self.api_url(method);
        let client = crate::config::build_runtime_proxy_client_with_timeouts(
            service_key,
            TELEGRAM_REQUEST_TIMEOUT_SECS,
            10,
        );

        let response = client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| self.redact_error(e.to_string()))?;

        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();

        let json: serde_json::Value = serde_json::from_str(&response_body).unwrap_or_else(|_| {
            serde_json::json!({"ok": false, "description": format!("Telegram API returned status {status}")})
        });

        let ok = json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if !ok {
            let desc = json
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("Telegram API returned ok=false");
            return Err(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.redact_token(desc)),
            });
        }

        Ok(json)
    }

    /// POST a multipart form to a Telegram API method. Returns the parsed JSON response.
    pub async fn post_multipart(
        &self,
        service_key: &str,
        method: &str,
        form: reqwest::multipart::Form,
    ) -> Result<serde_json::Value, ToolResult> {
        let url = self.api_url(method);
        let client = crate::config::build_runtime_proxy_client_with_timeouts(
            service_key,
            TELEGRAM_REQUEST_TIMEOUT_SECS,
            10,
        );

        let response = client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| self.redact_error(e.to_string()))?;

        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();

        let json: serde_json::Value = serde_json::from_str(&response_body).unwrap_or_else(|_| {
            serde_json::json!({"ok": false, "description": format!("Telegram API returned status {status}")})
        });

        let ok = json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if !ok {
            let desc = json
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("Telegram API returned ok=false");
            return Err(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.redact_token(desc)),
            });
        }

        Ok(json)
    }

    /// Download a file from the Telegram CDN. Returns the file bytes.
    pub async fn download_file(
        &self,
        service_key: &str,
        file_path: &str,
    ) -> Result<Vec<u8>, ToolResult> {
        let url = self.file_url(file_path);
        let client = crate::config::build_runtime_proxy_client_with_timeouts(
            service_key,
            TELEGRAM_REQUEST_TIMEOUT_SECS,
            10,
        );

        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| self.redact_error(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to download file: HTTP {}",
                    response.status()
                )),
            });
        }

        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| self.redact_error(e.to_string()))
    }

    /// Get the workspace directory from security policy.
    pub fn workspace_dir(&self) -> &std::path::Path {
        &self.security.workspace_dir
    }

    /// Validate that a path is within the workspace directory.
    pub fn validate_workspace_path(
        &self,
        path: &std::path::Path,
    ) -> Result<std::path::PathBuf, ToolResult> {
        let workspace = self.workspace_dir().canonicalize().map_err(|e| ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Cannot resolve workspace directory: {e}")),
        })?;

        // For existing files, canonicalize the full path
        // For new files, canonicalize the parent and join the filename
        let canonical = if path.exists() {
            path.canonicalize().map_err(|e| ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Cannot resolve path: {e}")),
            })?
        } else {
            let parent = path.parent().ok_or_else(|| ToolResult {
                success: false,
                output: String::new(),
                error: Some("Invalid file path: no parent directory".into()),
            })?;
            let parent_canonical = parent.canonicalize().map_err(|e| ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Cannot resolve parent directory: {e}")),
            })?;
            let filename = path.file_name().ok_or_else(|| ToolResult {
                success: false,
                output: String::new(),
                error: Some("Invalid file path: no filename".into()),
            })?;
            parent_canonical.join(filename)
        };

        if !canonical.starts_with(&workspace) {
            return Err(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Path is outside the workspace directory".into()),
            });
        }

        Ok(canonical)
    }

    fn redact_error(&self, error: String) -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some(self.redact_token(&error)),
        }
    }

    fn redact_token(&self, text: &str) -> String {
        text.replace(&self.bot_token, "[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn test_security(level: AutonomyLevel, max_actions: u32) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour: max_actions,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_client() -> TelegramApiClient {
        TelegramApiClient::new(
            test_security(AutonomyLevel::Full, 100),
            "test-bot-token-123".to_string(),
        )
    }

    #[test]
    fn api_url_builds_correctly() {
        let client = test_client();
        assert_eq!(
            client.api_url("sendMessage"),
            "https://api.telegram.org/bottest-bot-token-123/sendMessage"
        );
    }

    #[test]
    fn api_url_with_custom_base() {
        let client = test_client().with_api_base("https://custom.api".to_string());
        assert_eq!(
            client.api_url("getChat"),
            "https://custom.api/bottest-bot-token-123/getChat"
        );
    }

    #[test]
    fn file_url_builds_correctly() {
        let client = test_client();
        assert_eq!(
            client.file_url("documents/file_123.pdf"),
            "https://api.telegram.org/file/bottest-bot-token-123/documents/file_123.pdf"
        );
    }

    #[test]
    fn enforce_act_allows_full_autonomy() {
        let client = test_client();
        assert!(client.enforce_act().is_ok());
    }

    #[test]
    fn enforce_act_blocks_readonly() {
        let client = TelegramApiClient::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            "token".to_string(),
        );
        let err = client.enforce_act().unwrap_err();
        assert!(!err.success);
        assert!(err.error.unwrap().contains("read-only"));
    }

    #[test]
    fn enforce_act_blocks_rate_limit() {
        let client = TelegramApiClient::new(
            test_security(AutonomyLevel::Full, 0),
            "token".to_string(),
        );
        let err = client.enforce_act().unwrap_err();
        assert!(!err.success);
        assert!(err.error.unwrap().contains("rate limit"));
    }

    #[test]
    fn redact_token_removes_bot_token() {
        let client = test_client();
        let redacted = client.redact_token("Error with token test-bot-token-123 in url");
        assert!(!redacted.contains("test-bot-token-123"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn redact_token_no_token_unchanged() {
        let client = test_client();
        let text = "Some error without token";
        assert_eq!(client.redact_token(text), text);
    }

    #[test]
    fn redact_error_creates_tool_result() {
        let client = test_client();
        let result = client.redact_error("Failed with token test-bot-token-123".to_string());
        assert!(!result.success);
        assert!(result.output.is_empty());
        let err = result.error.unwrap();
        assert!(!err.contains("test-bot-token-123"));
    }

    #[test]
    fn workspace_dir_returns_security_workspace() {
        let client = test_client();
        assert_eq!(client.workspace_dir(), std::env::temp_dir());
    }
}
