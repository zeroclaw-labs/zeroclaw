//! Google Workspace integration tool for Gmail, Calendar, Drive, and other services.
//!
//! This tool provides structured access to Google Workspace services using local OAuth2
//! credentials, without requiring external SaaS dependencies like Composio.
//!
//! # Configuration
//!
//! Requires Google OAuth2 credentials set as environment variables:
//! - `GOOGLE_CLIENT_ID`
//! - `GOOGLE_CLIENT_SECRET`
//! - `GOOGLE_REFRESH_TOKEN`
//!
//! # Example Usage
//!
//! ```json
//! {
//!   "service": "gmail",
//!   "resource": "messages",
//!   "method": "list",
//!   "params": {
//!     "q": "is:unread",
//!     "maxResults": 5
//!   }
//! }
//! ```

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Token expiry buffer — refresh 5 minutes before the 60-minute expiry.
const TOKEN_TTL: Duration = Duration::from_secs(55 * 60);

/// Google Workspace integration tool
pub struct GoogleWorkspaceTool {
    security: Arc<SecurityPolicy>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    /// Cached (access_token, acquired_at)
    token_cache: Arc<Mutex<Option<(String, Instant)>>>,
}

impl GoogleWorkspaceTool {
    /// Create a new GoogleWorkspaceTool instance
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            client_id: std::env::var("GOOGLE_CLIENT_ID").ok(),
            client_secret: std::env::var("GOOGLE_CLIENT_SECRET").ok(),
            refresh_token: std::env::var("GOOGLE_REFRESH_TOKEN").ok(),
            token_cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Validate credentials are configured
    fn validate_credentials(&self) -> anyhow::Result<()> {
        if self.client_id.is_none() || self.client_secret.is_none() || self.refresh_token.is_none()
        {
            return Err(anyhow::anyhow!(
                "Google Workspace credentials not configured. Set GOOGLE_CLIENT_ID, \
                 GOOGLE_CLIENT_SECRET, and GOOGLE_REFRESH_TOKEN environment variables."
            ));
        }
        Ok(())
    }

    /// Validate service/resource/method combination
    fn validate_operation(service: &str, resource: &str, method: &str) -> anyhow::Result<()> {
        let allowed_resources = match service {
            "gmail" => vec!["messages", "threads", "labels", "drafts"],
            "calendar" => vec!["events", "calendars", "calendarList"],
            "drive" => vec!["files"],
            "tasks" => vec!["tasklists", "tasks"],
            "docs" => vec!["documents"],
            _ => return Err(anyhow::anyhow!("Unknown service: {}", service)),
        };

        if !allowed_resources.contains(&resource) {
            return Err(anyhow::anyhow!(
                "Invalid resource '{}' for service '{}'. Allowed: {:?}",
                resource,
                service,
                allowed_resources
            ));
        }

        if method.is_empty() {
            return Err(anyhow::anyhow!("Method cannot be empty"));
        }

        if !method.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(anyhow::anyhow!("Invalid method name: {}", method));
        }

        Ok(())
    }

    /// Fetch a valid access token, refreshing via OAuth2 if the cached one has expired.
    async fn get_access_token(&self) -> anyhow::Result<String> {
        let mut cache = self.token_cache.lock().await;

        if let Some((ref token, acquired_at)) = *cache {
            if acquired_at.elapsed() < TOKEN_TTL {
                return Ok(token.clone());
            }
        }

        // Refresh the token
        let client_id = self.client_id.as_deref().unwrap();
        let client_secret = self.client_secret.as_deref().unwrap();
        let refresh_token = self.refresh_token.as_deref().unwrap();

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(10));
        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "tool.gws");
        let client = builder.build()?;

        let response = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "OAuth2 token refresh failed (HTTP {}): {}",
                status,
                body
            ));
        }

        let body: Value = response.json().await?;
        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("No access_token in OAuth2 response"))?
            .to_string();

        *cache = Some((access_token.clone(), Instant::now()));
        Ok(access_token)
    }

    /// Build the Google API base URL for a given service and resource.
    fn build_url(service: &str, resource: &str) -> String {
        match service {
            "gmail" => format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/{}",
                resource
            ),
            "calendar" => format!(
                "https://www.googleapis.com/calendar/v3/calendars/primary/{}",
                resource
            ),
            "drive" => format!("https://www.googleapis.com/drive/v3/{}", resource),
            "tasks" => format!(
                "https://tasks.googleapis.com/tasks/v1/users/@me/{}",
                resource
            ),
            "docs" => format!("https://docs.googleapis.com/v1/{}", resource),
            _ => unreachable!("service validated before reaching build_url"),
        }
    }

    /// Execute a Google REST API call.
    async fn call_google_api(
        &self,
        service: &str,
        resource: &str,
        method: &str,
        params: &Value,
        access_token: &str,
    ) -> anyhow::Result<ToolResult> {
        let base_url = Self::build_url(service, resource);

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10));
        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "tool.gws");
        let client = builder.build()?;

        let empty_map = serde_json::Map::new();
        let param_obj = params.as_object().unwrap_or(&empty_map);

        let response = match method {
            "list" => {
                // Encode all params as query string
                let mut req = client.get(&base_url);
                for (k, v) in param_obj {
                    if let Some(s) = v.as_str() {
                        req = req.query(&[(k.as_str(), s)]);
                    } else {
                        req = req.query(&[(k.as_str(), v.to_string().as_str())]);
                    }
                }
                req.bearer_auth(access_token).send().await?
            }
            "get" => {
                // Append resource ID to path; remaining params as query
                let id = param_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'get' method requires 'id' in params"))?;
                let url = format!("{}/{}", base_url, id);
                let mut req = client.get(&url);
                for (k, v) in param_obj.iter().filter(|(k, _)| k.as_str() != "id") {
                    if let Some(s) = v.as_str() {
                        req = req.query(&[(k.as_str(), s)]);
                    }
                }
                req.bearer_auth(access_token).send().await?
            }
            "create" | "send" => {
                // POST with JSON body
                client
                    .post(&base_url)
                    .bearer_auth(access_token)
                    .json(params)
                    .send()
                    .await?
            }
            "update" => {
                // PATCH with resource ID in path and JSON body (minus id)
                let id = param_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'update' method requires 'id' in params"))?;
                let url = format!("{}/{}", base_url, id);
                let mut body = param_obj.clone();
                body.remove("id");
                client
                    .patch(&url)
                    .bearer_auth(access_token)
                    .json(&Value::Object(body))
                    .send()
                    .await?
            }
            "delete" => {
                let id = param_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'delete' method requires 'id' in params"))?;
                let url = format!("{}/{}", base_url, id);
                client.delete(&url).bearer_auth(access_token).send().await?
            }
            other => {
                return Err(anyhow::anyhow!(
                    "Unsupported method '{}'. Supported: list, get, create, update, delete, send",
                    other
                ));
            }
        };

        let status = response.status();
        let status_code = status.as_u16();
        let body_text = response.text().await.unwrap_or_default();

        if status.is_success() {
            // Try to pretty-print JSON, fall back to raw text
            let output = serde_json::from_str::<Value>(&body_text)
                .map(|v| serde_json::to_string_pretty(&v).unwrap_or(body_text.clone()))
                .unwrap_or(body_text);
            Ok(ToolResult {
                success: true,
                output,
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: body_text.clone(),
                error: Some(format!(
                    "Google API error HTTP {}: {}",
                    status_code, body_text
                )),
            })
        }
    }
}

impl Default for GoogleWorkspaceTool {
    fn default() -> Self {
        Self::new(Arc::new(SecurityPolicy::default()))
    }
}

#[async_trait]
impl Tool for GoogleWorkspaceTool {
    fn name(&self) -> &str {
        "gws"
    }

    fn description(&self) -> &str {
        "Access Google Workspace services (Gmail, Calendar, Drive, Tasks, Docs) using local \
         OAuth2 credentials. Requires GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET, and \
         GOOGLE_REFRESH_TOKEN environment variables."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "service": {
                    "type": "string",
                    "enum": ["gmail", "calendar", "drive", "tasks", "docs"],
                    "description": "The Google Workspace service to access"
                },
                "resource": {
                    "type": "string",
                    "description": "The resource type (e.g., 'messages' for Gmail, 'events' for Calendar)"
                },
                "method": {
                    "type": "string",
                    "enum": ["list", "get", "create", "update", "delete", "send"],
                    "description": "The method to call"
                },
                "params": {
                    "type": "object",
                    "description": "Method-specific parameters. For 'get', 'update', 'delete': include 'id'. For 'list': query/filter params.",
                    "additionalProperties": true
                }
            },
            "required": ["service", "resource", "method"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
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

        // Validate credentials
        if let Err(e) = self.validate_credentials() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            });
        }

        // Extract parameters
        let service = args
            .get("service")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'service' parameter"))?;

        let resource = args
            .get("resource")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'resource' parameter"))?;

        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'method' parameter"))?;

        // Validate operation
        if let Err(e) = Self::validate_operation(service, resource, method) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            });
        }

        let params = args
            .get("params")
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new()));

        // Get (or refresh) access token
        let access_token = match self.get_access_token().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("OAuth2 token error: {e}")),
                });
            }
        };

        self.call_google_api(service, resource, method, &params, &access_token)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn tool_with_security(security: SecurityPolicy) -> GoogleWorkspaceTool {
        GoogleWorkspaceTool::new(Arc::new(security))
    }

    fn supervised_tool() -> GoogleWorkspaceTool {
        tool_with_security(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        })
    }

    // ── Validation tests ─────────────────────────────────────────────────────

    #[test]
    fn validate_operation_valid() {
        assert!(GoogleWorkspaceTool::validate_operation("gmail", "messages", "list").is_ok());
        assert!(GoogleWorkspaceTool::validate_operation("calendar", "events", "create").is_ok());
        assert!(GoogleWorkspaceTool::validate_operation("drive", "files", "get").is_ok());
        assert!(GoogleWorkspaceTool::validate_operation("tasks", "tasklists", "list").is_ok());
        assert!(GoogleWorkspaceTool::validate_operation("docs", "documents", "get").is_ok());
    }

    #[test]
    fn validate_operation_invalid_service() {
        assert!(GoogleWorkspaceTool::validate_operation("invalid", "messages", "list").is_err());
    }

    #[test]
    fn validate_operation_invalid_resource() {
        assert!(GoogleWorkspaceTool::validate_operation("gmail", "invalid", "list").is_err());
    }

    #[test]
    fn validate_operation_empty_method() {
        assert!(GoogleWorkspaceTool::validate_operation("gmail", "messages", "").is_err());
    }

    #[test]
    fn validate_operation_bad_method_chars() {
        assert!(GoogleWorkspaceTool::validate_operation("gmail", "messages", "li st").is_err());
    }

    // ── URL construction tests ────────────────────────────────────────────────

    #[test]
    fn build_url_gmail() {
        assert_eq!(
            GoogleWorkspaceTool::build_url("gmail", "messages"),
            "https://gmail.googleapis.com/gmail/v1/users/me/messages"
        );
    }

    #[test]
    fn build_url_calendar() {
        assert_eq!(
            GoogleWorkspaceTool::build_url("calendar", "events"),
            "https://www.googleapis.com/calendar/v3/calendars/primary/events"
        );
    }

    #[test]
    fn build_url_drive() {
        assert_eq!(
            GoogleWorkspaceTool::build_url("drive", "files"),
            "https://www.googleapis.com/drive/v3/files"
        );
    }

    #[test]
    fn build_url_tasks() {
        assert_eq!(
            GoogleWorkspaceTool::build_url("tasks", "tasklists"),
            "https://tasks.googleapis.com/tasks/v1/users/@me/tasklists"
        );
    }

    #[test]
    fn build_url_docs() {
        assert_eq!(
            GoogleWorkspaceTool::build_url("docs", "documents"),
            "https://docs.googleapis.com/v1/documents"
        );
    }

    // ── Security policy tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let tool = tool_with_security(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let result = tool
            .execute(json!({
                "service": "gmail",
                "resource": "messages",
                "method": "list"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_when_rate_limited() {
        let tool = tool_with_security(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let result = tool
            .execute(json!({
                "service": "gmail",
                "resource": "messages",
                "method": "list"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn execute_returns_error_when_credentials_missing() {
        // Construct a tool with credentials explicitly cleared
        let tool = GoogleWorkspaceTool {
            security: Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Supervised,
                ..SecurityPolicy::default()
            }),
            client_id: None,
            client_secret: None,
            refresh_token: None,
            token_cache: Arc::new(Mutex::new(None)),
        };
        let result = tool
            .execute(json!({
                "service": "gmail",
                "resource": "messages",
                "method": "list"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("credentials not configured"));
    }

    #[test]
    fn tool_has_name_and_description() {
        let tool = supervised_tool();
        assert_eq!(tool.name(), "gws");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn parameters_schema_is_valid_object() {
        let tool = supervised_tool();
        let schema = tool.parameters_schema();
        assert!(schema.is_object());
        assert!(schema["properties"].is_object());
        assert!(schema["required"].is_array());
    }
}
