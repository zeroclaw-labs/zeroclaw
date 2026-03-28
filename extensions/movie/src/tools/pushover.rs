use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

const PUSHOVER_API_URL: &str = "https://api.pushover.net/1/messages.json";
const PUSHOVER_REQUEST_TIMEOUT_SECS: u64 = 15;
const PUSHOVER_TOKEN_ENV: &str = "PUSHOVER_TOKEN";
const PUSHOVER_USER_KEY_ENV: &str = "PUSHOVER_USER_KEY";

pub struct PushoverTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
}

impl PushoverTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }

    fn parse_env_value(raw: &str) -> String {
        let raw = raw.trim();

        let unquoted = if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };

        // Keep support for inline comments in unquoted values:
        // KEY=value # comment
        unquoted.split_once(" #").map_or_else(
            || unquoted.trim().to_string(),
            |(value, _)| value.trim().to_string(),
        )
    }

    fn looks_like_secret_reference(value: &str) -> bool {
        let trimmed = value.trim();
        trimmed.starts_with("en://") || trimmed.starts_with("ev://")
    }

    fn parse_process_env_credentials() -> anyhow::Result<Option<(String, String)>> {
        let token = std::env::var(PUSHOVER_TOKEN_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let user_key = std::env::var(PUSHOVER_USER_KEY_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        match (token, user_key) {
            (Some(token), Some(user_key)) => Ok(Some((token, user_key))),
            (Some(_), None) | (None, Some(_)) => Err(anyhow::anyhow!(
                "Process environment has only one Pushover credential. Set both {PUSHOVER_TOKEN_ENV} and {PUSHOVER_USER_KEY_ENV}."
            )),
            (None, None) => Ok(None),
        }
    }

    async fn get_credentials(&self) -> anyhow::Result<(String, String)> {
        if let Some(credentials) = Self::parse_process_env_credentials()? {
            return Ok(credentials);
        }

        let env_path = self.workspace_dir.join(".env");
        let content = tokio::fs::read_to_string(&env_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", env_path.display(), e))?;

        let mut token = None;
        let mut user_key = None;

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let line = line.strip_prefix("export ").map(str::trim).unwrap_or(line);
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = Self::parse_env_value(value);

                if Self::looks_like_secret_reference(&value) {
                    return Err(anyhow::anyhow!(
                        "{} uses secret references ({value}) for {key}. \
Provide resolved credentials via process env vars ({PUSHOVER_TOKEN_ENV}/{PUSHOVER_USER_KEY_ENV}), \
for example by launching ZeroClaw with enject injection.",
                        env_path.display()
                    ));
                }

                if key.eq_ignore_ascii_case(PUSHOVER_TOKEN_ENV) {
                    token = Some(value);
                } else if key.eq_ignore_ascii_case(PUSHOVER_USER_KEY_ENV) {
                    user_key = Some(value);
                }
            }
        }

        let token =
            token.ok_or_else(|| anyhow::anyhow!("{PUSHOVER_TOKEN_ENV} not found in .env"))?;
        let user_key =
            user_key.ok_or_else(|| anyhow::anyhow!("{PUSHOVER_USER_KEY_ENV} not found in .env"))?;

        Ok((token, user_key))
    }
}

#[async_trait]
impl Tool for PushoverTool {
    fn name(&self) -> &str {
        "pushover"
    }

    fn description(&self) -> &str {
        "Send a Pushover notification to your device. Uses PUSHOVER_TOKEN/PUSHOVER_USER_KEY from process environment first, then falls back to .env."
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
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?
            .to_string();

        let title = args.get("title").and_then(|v| v.as_str()).map(String::from);

        let priority = match args.get("priority").and_then(|v| v.as_i64()) {
            Some(value) if (-2..=2).contains(&value) => Some(value),
            Some(value) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid 'priority': {value}. Expected integer in range -2..=2"
                    )),
                })
            }
            None => None,
        };

        let sound = args.get("sound").and_then(|v| v.as_str()).map(String::from);

        let (token, user_key) = self.get_credentials().await?;

        let mut form = reqwest::multipart::Form::new()
            .text("token", token)
            .text("user", user_key)
            .text("message", message);

        if let Some(title) = title {
            form = form.text("title", title);
        }

        if let Some(priority) = priority {
            form = form.text("priority", priority.to_string());
        }

        if let Some(sound) = sound {
            form = form.text("sound", sound);
        }

        let client = crate::config::build_runtime_proxy_client_with_timeouts(
            "tool.pushover",
            PUSHOVER_REQUEST_TIMEOUT_SECS,
            10,
        );
        let response = client.post(PUSHOVER_API_URL).multipart(form).send().await?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Ok(ToolResult {
                success: false,
                output: body,
                error: Some(format!("Pushover API returned status {}", status)),
            });
        }

        let api_status = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|json| json.get("status").and_then(|value| value.as_i64()));

        if api_status == Some(1) {
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
                error: Some("Pushover API returned an application-level error".into()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;
    use std::fs;
    use std::sync::{LazyLock, Mutex, MutexGuard};
    use tempfile::TempDir;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn test_security(level: AutonomyLevel, max_actions_per_hour: u32) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().expect("env lock poisoned")
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }

        fn unset(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn pushover_tool_name() {
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        assert_eq!(tool.name(), "pushover");
    }

    #[test]
    fn pushover_tool_description() {
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn pushover_tool_has_parameters_schema() {
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].get("message").is_some());
    }

    #[test]
    fn pushover_tool_requires_message() {
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("message".to_string())));
    }

    #[tokio::test]
    async fn credentials_parsed_from_env_file() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::unset(PUSHOVER_TOKEN_ENV);
        let _g2 = EnvGuard::unset(PUSHOVER_USER_KEY_ENV);
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "PUSHOVER_TOKEN=testtoken123\nPUSHOVER_USER_KEY=userkey456\n",
        )
        .unwrap();

        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_ok());
        let (token, user_key) = result.unwrap();
        assert_eq!(token, "testtoken123");
        assert_eq!(user_key, "userkey456");
    }

    #[tokio::test]
    async fn credentials_fail_without_env_file() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::unset(PUSHOVER_TOKEN_ENV);
        let _g2 = EnvGuard::unset(PUSHOVER_USER_KEY_ENV);
        let tmp = TempDir::new().unwrap();
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn credentials_fail_without_token() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::unset(PUSHOVER_TOKEN_ENV);
        let _g2 = EnvGuard::unset(PUSHOVER_USER_KEY_ENV);
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(&env_path, "PUSHOVER_USER_KEY=userkey456\n").unwrap();

        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn credentials_fail_without_user_key() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::unset(PUSHOVER_TOKEN_ENV);
        let _g2 = EnvGuard::unset(PUSHOVER_USER_KEY_ENV);
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(&env_path, "PUSHOVER_TOKEN=testtoken123\n").unwrap();

        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn credentials_ignore_comments() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::unset(PUSHOVER_TOKEN_ENV);
        let _g2 = EnvGuard::unset(PUSHOVER_USER_KEY_ENV);
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(&env_path, "# This is a comment\nPUSHOVER_TOKEN=realtoken\n# Another comment\nPUSHOVER_USER_KEY=realuser\n").unwrap();

        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_ok());
        let (token, user_key) = result.unwrap();
        assert_eq!(token, "realtoken");
        assert_eq!(user_key, "realuser");
    }

    #[test]
    fn pushover_tool_supports_priority() {
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("priority").is_some());
    }

    #[test]
    fn pushover_tool_supports_sound() {
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"].get("sound").is_some());
    }

    #[tokio::test]
    async fn credentials_support_export_and_quoted_values() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::unset(PUSHOVER_TOKEN_ENV);
        let _g2 = EnvGuard::unset(PUSHOVER_USER_KEY_ENV);
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "export PUSHOVER_TOKEN=\"quotedtoken\"\nPUSHOVER_USER_KEY='quoteduser'\n",
        )
        .unwrap();

        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_ok());
        let (token, user_key) = result.unwrap();
        assert_eq!(token, "quotedtoken");
        assert_eq!(user_key, "quoteduser");
    }

    #[tokio::test]
    async fn credentials_use_process_env_without_env_file() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::set(PUSHOVER_TOKEN_ENV, "env-token-123");
        let _g2 = EnvGuard::set(PUSHOVER_USER_KEY_ENV, "env-user-456");

        let tmp = TempDir::new().unwrap();
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_ok());
        let (token, user_key) = result.unwrap();
        assert_eq!(token, "env-token-123");
        assert_eq!(user_key, "env-user-456");
    }

    #[tokio::test]
    async fn credentials_fail_when_only_one_process_env_var_is_set() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::set(PUSHOVER_TOKEN_ENV, "only-token");
        let _g2 = EnvGuard::unset(PUSHOVER_USER_KEY_ENV);

        let tmp = TempDir::new().unwrap();
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("only one Pushover credential"));
    }

    #[tokio::test]
    async fn credentials_fail_on_secret_reference_values_in_dotenv() {
        let _env_lock = lock_env();
        let _g1 = EnvGuard::unset(PUSHOVER_TOKEN_ENV);
        let _g2 = EnvGuard::unset(PUSHOVER_USER_KEY_ENV);

        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join(".env");
        fs::write(
            &env_path,
            "PUSHOVER_TOKEN=en://pushover_token\nPUSHOVER_USER_KEY=en://pushover_user\n",
        )
        .unwrap();

        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            tmp.path().to_path_buf(),
        );
        let result = tool.get_credentials().await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("secret references"));
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            PathBuf::from("/tmp"),
        );

        let result = tool.execute(json!({"message": "hello"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = PushoverTool::new(test_security(AutonomyLevel::Full, 0), PathBuf::from("/tmp"));

        let result = tool.execute(json!({"message": "hello"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_rejects_priority_out_of_range() {
        let tool = PushoverTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );

        let result = tool
            .execute(json!({"message": "hello", "priority": 5}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("-2..=2"));
    }
}
