use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::path::PathBuf;

pub struct PushoverTool {
    client: Client,
    workspace_dir: PathBuf,
}

impl PushoverTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self {
            client: Client::new(),
            workspace_dir,
        }
    }

    fn get_credentials(&self) -> anyhow::Result<(String, String)> {
        let env_path = self.workspace_dir.join(".env");
        let content = std::fs::read_to_string(&env_path)
            .map_err(|e| anyhow::anyhow!("Failed to read .env: {}", e))?;

        let mut token = None;
        let mut user_key = None;

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                if key.eq_ignore_ascii_case("PUSHOVER_TOKEN") {
                    token = Some(value.to_string());
                } else if key.eq_ignore_ascii_case("PUSHOVER_USER_KEY") {
                    user_key = Some(value.to_string());
                }
            }
        }

        let token = token.ok_or_else(|| anyhow::anyhow!("PUSHOVER_TOKEN not found in .env"))?;
        let user_key =
            user_key.ok_or_else(|| anyhow::anyhow!("PUSHOVER_USER_KEY not found in .env"))?;

        Ok((token, user_key))
    }
}

#[async_trait]
impl Tool for PushoverTool {
    fn name(&self) -> &str {
        "pushover"
    }

    fn description(&self) -> &str {
        "Send a Pushover notification to your device. Requires PUSHOVER_TOKEN and PUSHOVER_USER_KEY in .env file."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The notification message to send"
                },
                "title": {
                    "type": "string",
                    "description": "Optional notification title"
                },
                "priority": {
                    "type": "integer",
                    "enum": [-2, -1, 0, 1, 2],
                    "description": "Message priority: -2 (lowest/silent), -1 (low/no sound), 0 (normal), 1 (high), 2 (emergency/repeating)"
                },
                "sound": {
                    "type": "string",
                    "description": "Notification sound override (e.g., 'pushover', 'bike', 'bugle', 'cashregister', etc.)"
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?
            .to_string();

        let title = args.get("title").and_then(|v| v.as_str()).map(String::from);

        let priority = args.get("priority").and_then(|v| v.as_i64());

        let sound = args.get("sound").and_then(|v| v.as_str()).map(String::from);

        let (token, user_key) = self.get_credentials()?;

        let mut form = reqwest::multipart::Form::new()
            .text("token", token)
            .text("user", user_key)
            .text("message", message);

        if let Some(title) = title {
            form = form.text("title", title);
        }

        if let Some(priority) = priority {
            if priority >= -2 && priority <= 2 {
                form = form.text("priority", priority.to_string());
            }
        }

        if let Some(sound) = sound {
            form = form.text("sound", sound);
        }

        let response = self
            .client
            .post("https://api.pushover.net/1/messages.json")
            .multipart(form)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if status.is_success() {
            Ok(ToolResult {
                success: true,
                output: format!(
                    "Pushover notification sent successfully. Response: {}",
                    body
                ),
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: body,
                error: Some(format!("Pushover API returned status {}", status)),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn pushover_tool_name() {
        let tool = PushoverTool::new(PathBuf::from("/tmp"));
        assert_eq!(tool.name(), "pushover");
    }

    #[test]
    fn pushover_tool_description() {
        let tool = PushoverTool::new(PathBuf::from("/tmp"));
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn pushover_tool_has_parameters_schema() {
        let tool = PushoverTool::new(PathBuf::from("/tmp"));
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].get("message").is_some());
    }

    #[test]
    fn pushover_tool_requires_message() {
        let tool = PushoverTool::new(PathBuf::from("/tmp"));
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("message".to_string())));
    }

    #[test]
    fn credentials_parsed_from_env_file() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "PUSHOVER_TOKEN=testtoken123\nPUSHOVER_USER_KEY=userkey456\n",
        )
        .unwrap();

        let tool = PushoverTool::new(tmp.path().to_path_buf());
        let result = tool.get_credentials();

        assert!(result.is_ok());
        let (token, user_key) = result.unwrap();
        assert_eq!(token, "testtoken123");
        assert_eq!(user_key, "userkey456");
    }

    #[test]
    fn credentials_fail_without_env_file() {
        let tmp = TempDir::new().unwrap();
        let tool = PushoverTool::new(tmp.path().to_path_buf());
        let result = tool.get_credentials();

        assert!(result.is_err());
    }

    #[test]
    fn credentials_fail_without_token() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(&env_path, "PUSHOVER_USER_KEY=userkey456\n").unwrap();

        let tool = PushoverTool::new(tmp.path().to_path_buf());
        let result = tool.get_credentials();

        assert!(result.is_err());
    }

    #[test]
    fn credentials_fail_without_user_key() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(&env_path, "PUSHOVER_TOKEN=testtoken123\n").unwrap();

        let tool = PushoverTool::new(tmp.path().to_path_buf());
        let result = tool.get_credentials();

        assert!(result.is_err());
    }

    #[test]
    fn credentials_ignore_comments() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(&env_path, "# This is a comment\nPUSHOVER_TOKEN=realtoken\n# Another comment\nPUSHOVER_USER_KEY=realuser\n").unwrap();

        let tool = PushoverTool::new(tmp.path().to_path_buf());
        let result = tool.get_credentials();

        assert!(result.is_ok());
        let (token, user_key) = result.unwrap();
        assert_eq!(token, "realtoken");
        assert_eq!(user_key, "realuser");
    }

    #[test]
    fn pushover_tool_supports_priority() {
        let tool = PushoverTool::new(PathBuf::from("/tmp"));
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("priority").is_some());
    }

    #[test]
    fn pushover_tool_supports_sound() {
        let tool = PushoverTool::new(PathBuf::from("/tmp"));
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("sound").is_some());
    }
}
