use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use anyhow::Context;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_FACEBOOK_GRAPH_API_BASE: &str = "https://graph.facebook.com/v22.0";
const FACEBOOK_REQUEST_TIMEOUT_SECS: u64 = 20;

const FB_APP_ID_ENV_KEYS: &[&str] = &[
    "ZEROCLAW_FB_APP_ID",
    "FB_APP_ID",
    "FACEBOOK_APP_ID",
    "META_APP_ID",
];
const FB_APP_SECRET_ENV_KEYS: &[&str] = &[
    "ZEROCLAW_FB_APP_SECRET",
    "FB_APP_SECRET",
    "FACEBOOK_APP_SECRET",
    "META_APP_SECRET",
];
const FB_ACCESS_TOKEN_ENV_KEYS: &[&str] = &[
    "ZEROCLAW_FB_ACCESS_TOKEN",
    "FB_ACCESS_TOKEN",
    "FACEBOOK_ACCESS_TOKEN",
    "META_ACCESS_TOKEN",
];
const FB_GRAPH_API_BASE_ENV_KEYS: &[&str] = &[
    "ZEROCLAW_FACEBOOK_GRAPH_API_BASE",
    "FACEBOOK_GRAPH_API_BASE",
    "FB_GRAPH_API_BASE",
];

pub struct FacebookPagePostTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
struct FacebookPageAccount {
    id: String,
    name: String,
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct FacebookAccountsResponse {
    data: Vec<FacebookPageAccount>,
    paging: Option<FacebookPaging>,
}

#[derive(Debug, Deserialize)]
struct FacebookPaging {
    cursors: Option<FacebookPagingCursors>,
}

#[derive(Debug, Deserialize)]
struct FacebookPagingCursors {
    after: Option<String>,
}

impl FacebookPagePostTool {
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

        unquoted.split_once(" #").map_or_else(
            || unquoted.trim().to_string(),
            |(value, _)| value.trim().to_string(),
        )
    }

    fn read_non_empty_process_env(keys: &[&str]) -> Option<String> {
        keys.iter().find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    }

    fn read_non_empty_env_file_value(
        env_file_values: &HashMap<String, String>,
        keys: &[&str],
    ) -> Option<String> {
        keys.iter().find_map(|key| {
            env_file_values
                .get(*key)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    }

    fn require_env_value(
        label: &str,
        keys: &[&str],
        env_file_values: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        if let Some(value) = Self::read_non_empty_process_env(keys)
            .or_else(|| Self::read_non_empty_env_file_value(env_file_values, keys))
        {
            return Ok(value);
        }

        anyhow::bail!("Missing {label}. Set one of: {}", keys.join(", "))
    }

    async fn read_env_file_values(&self) -> anyhow::Result<HashMap<String, String>> {
        let env_path = self.workspace_dir.join(".env");
        let content = match tokio::fs::read_to_string(&env_path).await {
            Ok(content) => content,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to read {}", env_path.display()))
            }
        };

        let mut values = HashMap::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let line = line.strip_prefix("export ").map(str::trim).unwrap_or(line);
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                if key.is_empty() {
                    continue;
                }
                values.insert(key.to_string(), Self::parse_env_value(value));
            }
        }

        Ok(values)
    }

    async fn get_credentials(&self) -> anyhow::Result<(String, String, String)> {
        let env_file_values = self.read_env_file_values().await?;

        let app_id =
            Self::require_env_value("Facebook app ID", FB_APP_ID_ENV_KEYS, &env_file_values)?;
        let app_secret = Self::require_env_value(
            "Facebook app secret",
            FB_APP_SECRET_ENV_KEYS,
            &env_file_values,
        )?;
        let access_token = Self::require_env_value(
            "Facebook long-lived user access token",
            FB_ACCESS_TOKEN_ENV_KEYS,
            &env_file_values,
        )?;

        Ok((app_id, app_secret, access_token))
    }

    fn compute_appsecret_proof(app_secret: &str, access_token: &str) -> anyhow::Result<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes())
            .context("Invalid Facebook app secret for appsecret_proof")?;
        mac.update(access_token.as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    async fn get_graph_api_base(&self) -> anyhow::Result<String> {
        let env_file_values = self.read_env_file_values().await?;
        let raw = Self::read_non_empty_process_env(FB_GRAPH_API_BASE_ENV_KEYS)
            .or_else(|| {
                Self::read_non_empty_env_file_value(&env_file_values, FB_GRAPH_API_BASE_ENV_KEYS)
            })
            .unwrap_or_else(|| DEFAULT_FACEBOOK_GRAPH_API_BASE.to_string());

        let normalized = raw.trim().trim_end_matches('/').to_string();
        if !normalized.starts_with("https://") && !normalized.starts_with("http://") {
            anyhow::bail!(
                "Invalid Facebook Graph API base URL: must start with http:// or https://"
            );
        }
        Ok(normalized)
    }

    async fn fetch_page_accounts(
        &self,
        client: &reqwest::Client,
        graph_api_base: &str,
        user_access_token: &str,
        user_appsecret_proof: &str,
    ) -> anyhow::Result<Vec<FacebookPageAccount>> {
        let mut accounts = Vec::new();
        let mut after: Option<String> = None;

        loop {
            let endpoint = format!("{graph_api_base}/me/accounts");
            let mut query = vec![
                ("fields", "id,name,access_token".to_string()),
                ("limit", "100".to_string()),
                ("access_token", user_access_token.to_string()),
                ("appsecret_proof", user_appsecret_proof.to_string()),
            ];
            if let Some(cursor) = &after {
                query.push(("after", cursor.clone()));
            }

            let response = client.get(&endpoint).query(&query).send().await?;
            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            if !status.is_success() {
                anyhow::bail!(
                    "Facebook Graph API returned status {status} while listing connected pages: {body}"
                );
            }

            let payload: FacebookAccountsResponse = serde_json::from_str(&body)
                .with_context(|| format!("Failed to parse /me/accounts response: {body}"))?;
            accounts.extend(payload.data);

            after = payload
                .paging
                .and_then(|paging| paging.cursors)
                .and_then(|cursors| cursors.after);
            if after.is_none() {
                break;
            }
        }

        Ok(accounts)
    }
}

#[async_trait]
impl Tool for FacebookPagePostTool {
    fn name(&self) -> &str {
        "facebook_page_post"
    }

    fn description(&self) -> &str {
        "Create a post on all Facebook Pages connected to the configured app user. \
        Credentials are read from env vars (preferred) or workspace .env: app_id, \
        app_secret, long-lived user access token. The tool discovers pages via \
        /me/accounts, then posts to each connected page using its page access token. \
        Supports text/link posts and single-image posts (image_url or image_path). \
        Optional API base override: FACEBOOK_GRAPH_API_BASE."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Post message text"
                },
                "link": {
                    "type": "string",
                    "description": "Optional HTTPS/HTTP URL to include with the post"
                },
                "image_url": {
                    "type": "string",
                    "description": "Optional HTTPS/HTTP image URL. If set, post is published as a page photo with message as caption."
                },
                "image_path": {
                    "type": "string",
                    "description": "Optional local image path (workspace-relative recommended). If set, image is uploaded as a page photo with message as caption."
                },
                "published": {
                    "type": "boolean",
                    "description": "Whether to publish immediately (default true)",
                    "default": true
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
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?
            .to_string();

        let link = match args.get("link").and_then(|value| value.as_str()) {
            Some(value) => {
                let value = value.trim();
                if value.is_empty() {
                    None
                } else if value.starts_with("https://") || value.starts_with("http://") {
                    Some(value.to_string())
                } else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Invalid 'link': must start with http:// or https://".into()),
                    });
                }
            }
            None => None,
        };

        let image_url = match args.get("image_url").and_then(|value| value.as_str()) {
            Some(value) => {
                let value = value.trim();
                if value.is_empty() {
                    None
                } else if value.starts_with("https://") || value.starts_with("http://") {
                    Some(value.to_string())
                } else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "Invalid 'image_url': must start with http:// or https://".into(),
                        ),
                    });
                }
            }
            None => None,
        };

        let image_path = args
            .get("image_path")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        if image_url.is_some() && image_path.is_some() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Use only one of 'image_url' or 'image_path'".into()),
            });
        }

        if (image_url.is_some() || image_path.is_some()) && link.is_some() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Do not combine 'link' with 'image_url'/'image_path'".into()),
            });
        }

        let published = args
            .get("published")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);

        let (_app_id, app_secret, user_access_token) = self.get_credentials().await?;
        let graph_api_base = self.get_graph_api_base().await?;
        let client = crate::config::build_runtime_proxy_client_with_timeouts(
            "tool.facebook_page_post",
            FACEBOOK_REQUEST_TIMEOUT_SECS,
            10,
        );
        let user_appsecret_proof =
            Self::compute_appsecret_proof(&app_secret, &user_access_token)?;
        let page_accounts = self
            .fetch_page_accounts(
                &client,
                &graph_api_base,
                &user_access_token,
                &user_appsecret_proof,
            )
            .await?;

        if page_accounts.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "No Facebook Pages were returned by /me/accounts for the configured user token"
                        .into(),
                ),
            });
        }

        let image_upload = if let Some(image_path) = image_path {
            if !self.security.is_path_allowed(&image_path) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Path not allowed by security policy: {image_path}")),
                });
            }

            let full_image_path = self.workspace_dir.join(&image_path);
            let image_bytes = match tokio::fs::read(&full_image_path).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Failed to read image_path '{}': {}",
                            full_image_path.display(),
                            error
                        )),
                    })
                }
            };

            let filename = full_image_path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("image.bin")
                .to_string();

            Some((image_bytes, filename))
        } else {
            None
        };

        let mut successes = Vec::new();
        let mut failures = Vec::new();

        for page in page_accounts {
            let page_appsecret_proof =
                Self::compute_appsecret_proof(&app_secret, &page.access_token)?;
            let response = if let Some(image_url) = &image_url {
                let endpoint = format!("{graph_api_base}/{}/photos", page.id);
                let mut form_data = vec![
                    ("caption".to_string(), message.clone()),
                    ("url".to_string(), image_url.clone()),
                    ("access_token".to_string(), page.access_token.clone()),
                    ("appsecret_proof".to_string(), page_appsecret_proof),
                ];
                if !published {
                    form_data.push(("published".to_string(), "false".to_string()));
                }
                client.post(endpoint).form(&form_data).send().await?
            } else if let Some((image_bytes, filename)) = &image_upload {
                let form = reqwest::multipart::Form::new()
                    .text("caption", message.clone())
                    .text("access_token", page.access_token.clone())
                    .text("appsecret_proof", page_appsecret_proof)
                    .text("published", if published { "true" } else { "false" })
                    .part(
                        "source",
                        reqwest::multipart::Part::bytes(image_bytes.clone())
                            .file_name(filename.clone()),
                    );

                let endpoint = format!("{graph_api_base}/{}/photos", page.id);
                client.post(endpoint).multipart(form).send().await?
            } else {
                let endpoint = format!("{graph_api_base}/{}/feed", page.id);
                let mut form_data = vec![
                    ("message".to_string(), message.clone()),
                    ("access_token".to_string(), page.access_token.clone()),
                    ("appsecret_proof".to_string(), page_appsecret_proof),
                ];
                if let Some(link) = &link {
                    form_data.push(("link".to_string(), link.clone()));
                }
                if !published {
                    form_data.push(("published".to_string(), "false".to_string()));
                }
                client.post(endpoint).form(&form_data).send().await?
            };

            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            if !status.is_success() {
                failures.push(format!(
                    "{} ({}) returned status {}: {}",
                    page.name, page.id, status, body
                ));
                continue;
            }

            match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(payload) if payload.get("error").is_some() => {
                    failures.push(format!(
                        "{} ({}) returned an application-level error: {}",
                        page.name, page.id, body
                    ));
                }
                Ok(payload) => {
                    let post_id = payload
                        .get("id")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown");
                    successes.push(format!("{} ({}) => {}", page.name, page.id, post_id));
                }
                Err(_) => {
                    successes.push(format!("{} ({}) => {}", page.name, page.id, body));
                }
            }
        }

        if failures.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!(
                    "Facebook posts created successfully on {} page(s): {}",
                    successes.len(),
                    successes.join("; ")
                ),
                error: None,
            });
        }

        Ok(ToolResult {
            success: false,
            output: if successes.is_empty() {
                String::new()
            } else {
                format!("Succeeded on: {}", successes.join("; "))
            },
            error: Some(format!("Failed on: {}", failures.join("; "))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;
    use std::fs;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn test_security(level: AutonomyLevel, max_actions_per_hour: u32) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    fn clear_facebook_envs_for_test() {
        for key in [
            "ZEROCLAW_FB_APP_ID",
            "FB_APP_ID",
            "FACEBOOK_APP_ID",
            "META_APP_ID",
            "ZEROCLAW_FB_APP_SECRET",
            "FB_APP_SECRET",
            "FACEBOOK_APP_SECRET",
            "META_APP_SECRET",
            "ZEROCLAW_FB_ACCESS_TOKEN",
            "FB_ACCESS_TOKEN",
            "FACEBOOK_ACCESS_TOKEN",
            "META_ACCESS_TOKEN",
            "ZEROCLAW_FACEBOOK_GRAPH_API_BASE",
            "FACEBOOK_GRAPH_API_BASE",
            "FB_GRAPH_API_BASE",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn facebook_tool_name() {
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        assert_eq!(tool.name(), "facebook_page_post");
    }

    #[test]
    fn facebook_tool_requires_message() {
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("message".to_string())));
    }

    #[tokio::test]
    async fn credentials_can_be_read_from_env_file() {
        let _guard = env_lock();
        clear_facebook_envs_for_test();
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".env"),
            "FB_APP_ID=1234\nFB_APP_SECRET=secret\nFB_ACCESS_TOKEN=token\n",
        )
        .unwrap();

        let tool =
            FacebookPagePostTool::new(test_security(AutonomyLevel::Full, 100), tmp.path().into());
        let creds = tool.get_credentials().await.unwrap();

        assert_eq!(creds.0, "1234");
        assert_eq!(creds.1, "secret");
        assert_eq!(creds.2, "token");
    }

    #[tokio::test]
    async fn credentials_prefer_process_env_over_env_file() {
        let _guard = env_lock();
        clear_facebook_envs_for_test();
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".env"),
            "FB_APP_ID=file-app\nFB_APP_SECRET=file-secret\nFB_ACCESS_TOKEN=file-token\n",
        )
        .unwrap();

        std::env::set_var("ZEROCLAW_FB_APP_ID", "env-app");
        std::env::set_var("ZEROCLAW_FB_APP_SECRET", "env-secret");
        std::env::set_var("ZEROCLAW_FB_ACCESS_TOKEN", "env-token");

        let tool =
            FacebookPagePostTool::new(test_security(AutonomyLevel::Full, 100), tmp.path().into());
        let creds = tool.get_credentials().await.unwrap();

        assert_eq!(creds.0, "env-app");
        assert_eq!(creds.1, "env-secret");
        assert_eq!(creds.2, "env-token");

        clear_facebook_envs_for_test();
    }

    #[test]
    fn parse_me_accounts_response_with_paging() {
        let payload = r#"{
            "data": [
                {"id":"1","name":"Page One","access_token":"page-token-1"},
                {"id":"2","name":"Page Two","access_token":"page-token-2"}
            ],
            "paging": {
                "cursors": {"after":"cursor-2"}
            }
        }"#;

        let parsed: FacebookAccountsResponse = serde_json::from_str(payload).unwrap();
        assert_eq!(parsed.data.len(), 2);
        assert_eq!(parsed.data[0].name, "Page One");
        assert_eq!(parsed.data[1].id, "2");
        assert_eq!(
            parsed
                .paging
                .and_then(|paging| paging.cursors)
                .and_then(|cursors| cursors.after)
                .as_deref(),
            Some("cursor-2")
        );
    }

    #[tokio::test]
    async fn graph_api_base_uses_default_when_unset() {
        let _guard = env_lock();
        clear_facebook_envs_for_test();
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        let value = tool.get_graph_api_base().await.unwrap();
        assert_eq!(value, DEFAULT_FACEBOOK_GRAPH_API_BASE);
    }

    #[tokio::test]
    async fn graph_api_base_can_be_read_from_env_file() {
        let _guard = env_lock();
        clear_facebook_envs_for_test();
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".env"),
            "FACEBOOK_GRAPH_API_BASE=https://graph.facebook.com/v21.0/\n",
        )
        .unwrap();

        let tool =
            FacebookPagePostTool::new(test_security(AutonomyLevel::Full, 100), tmp.path().into());
        let value = tool.get_graph_api_base().await.unwrap();
        assert_eq!(value, "https://graph.facebook.com/v21.0");
    }

    #[tokio::test]
    async fn graph_api_base_prefers_process_env() {
        let _guard = env_lock();
        clear_facebook_envs_for_test();
        std::env::set_var("FACEBOOK_GRAPH_API_BASE", "https://graph.facebook.com/v20.0");

        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );
        let value = tool.get_graph_api_base().await.unwrap();
        assert_eq!(value, "https://graph.facebook.com/v20.0");

        clear_facebook_envs_for_test();
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::ReadOnly, 100),
            PathBuf::from("/tmp"),
        );

        let result = tool.execute(json!({"message":"hello"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_rate_limit() {
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 0),
            PathBuf::from("/tmp"),
        );

        let result = tool.execute(json!({"message":"hello"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_link() {
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );

        let result = tool
            .execute(json!({"message":"hello","link":"ftp://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("http:// or https://"));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_image_url() {
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );

        let result = tool
            .execute(json!({"message":"hello","image_url":"file:///tmp/test.png"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("http:// or https://"));
    }

    #[tokio::test]
    async fn execute_rejects_both_image_inputs() {
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );

        let result = tool
            .execute(json!({"message":"hello","image_url":"https://example.com/a.png","image_path":"incoming/a.png"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Use only one"));
    }

    #[tokio::test]
    async fn execute_rejects_link_and_image_combination() {
        let tool = FacebookPagePostTool::new(
            test_security(AutonomyLevel::Full, 100),
            PathBuf::from("/tmp"),
        );

        let result = tool
            .execute(json!({"message":"hello","link":"https://example.com","image_url":"https://example.com/a.png"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Do not combine 'link'"));
    }
}
