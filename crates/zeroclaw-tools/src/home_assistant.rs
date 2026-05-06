//! Home Assistant tool — REST API client for a self-hosted Home Assistant instance.
//!
//! Authenticates with a long-lived access token. Read actions (`get_state`,
//! `list_states`, `list_services`) require `ToolOperation::Read`; mutating
//! actions (`call_service`) require `ToolOperation::Act`. Service domains
//! callable through `call_service` are restricted by `allowed_domains` —
//! configure narrowly in production.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

/// Maximum number of characters to include from an error response body.
const MAX_ERROR_BODY_CHARS: usize = 500;

/// Tool for interacting with a Home Assistant instance over its REST API.
pub struct HomeAssistantTool {
    base_url: String,
    access_token: String,
    allowed_domains: Vec<String>,
    request_timeout_secs: u64,
    http: reqwest::Client,
    security: Arc<SecurityPolicy>,
}

impl HomeAssistantTool {
    /// Create a new Home Assistant tool. `base_url` is normalized by stripping
    /// any trailing `/`.
    pub fn new(
        base_url: String,
        access_token: String,
        allowed_domains: Vec<String>,
        request_timeout_secs: u64,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        let allowed_domains = allowed_domains
            .into_iter()
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect();
        Self {
            base_url,
            access_token,
            allowed_domains,
            request_timeout_secs,
            http: reqwest::Client::new(),
            security,
        }
    }

    fn headers(&self) -> anyhow::Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", self.access_token)
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid Home Assistant token header: {e}"))?,
        );
        headers.insert("Content-Type", "application/json".parse().unwrap());
        Ok(headers)
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs.max(1))
    }

    fn is_domain_allowed(&self, domain: &str) -> bool {
        self.allowed_domains.iter().any(|d| d == domain)
    }

    async fn list_states(&self) -> anyhow::Result<Value> {
        let url = format!("{}/api/states", self.base_url);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers()?)
            .timeout(self.timeout())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Home Assistant list_states failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    async fn get_state(&self, entity_id: &str) -> anyhow::Result<Value> {
        let url = format!("{}/api/states/{entity_id}", self.base_url);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers()?)
            .timeout(self.timeout())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
            anyhow::bail!(
                "Home Assistant get_state failed for '{entity_id}' ({status}): {truncated}"
            );
        }
        resp.json().await.map_err(Into::into)
    }

    async fn list_services(&self) -> anyhow::Result<Value> {
        let url = format!("{}/api/services", self.base_url);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers()?)
            .timeout(self.timeout())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Home Assistant list_services failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    async fn call_service(
        &self,
        domain: &str,
        service: &str,
        service_data: Option<&Value>,
    ) -> anyhow::Result<Value> {
        let url = format!("{}/api/services/{domain}/{service}", self.base_url);
        let body = service_data.cloned().unwrap_or_else(|| json!({}));
        let resp = self
            .http
            .post(&url)
            .headers(self.headers()?)
            .json(&body)
            .timeout(self.timeout())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
            anyhow::bail!(
                "Home Assistant call_service {domain}.{service} failed ({status}): {truncated}"
            );
        }
        resp.json().await.map_err(Into::into)
    }
}

#[async_trait]
impl Tool for HomeAssistantTool {
    fn name(&self) -> &str {
        "home_assistant"
    }

    fn description(&self) -> &str {
        "Interact with a self-hosted Home Assistant instance via its REST API. \
         Read entity state (get_state, list_states), discover available services \
         (list_services), or trigger automations and devices (call_service). \
         Service domains callable through call_service are restricted by the \
         operator's allowed_domains config."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get_state", "list_states", "list_services", "call_service"],
                    "description": "The Home Assistant operation to perform."
                },
                "entity_id": {
                    "type": "string",
                    "description": "Entity ID, e.g. 'light.kitchen'. Required for get_state."
                },
                "domain": {
                    "type": "string",
                    "description": "Service domain, e.g. 'light'. Required for call_service."
                },
                "service": {
                    "type": "string",
                    "description": "Service name, e.g. 'turn_on'. Required for call_service."
                },
                "service_data": {
                    "type": "object",
                    "description": "Optional service payload. May include 'entity_id' to scope the call."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: action".into()),
                });
            }
        };

        let operation = match action {
            "get_state" | "list_states" | "list_services" => ToolOperation::Read,
            "call_service" => ToolOperation::Act,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action: {action}. Valid actions: get_state, list_states, list_services, call_service"
                    )),
                });
            }
        };

        if let Err(error) = self
            .security
            .enforce_tool_operation(operation, "home_assistant")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let result = match action {
            "list_states" => self.list_states().await,
            "list_services" => self.list_services().await,
            "get_state" => {
                let entity_id = match args.get("entity_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.trim().is_empty() => id.trim(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("get_state requires entity_id parameter".into()),
                        });
                    }
                };
                self.get_state(entity_id).await
            }
            "call_service" => {
                let domain = match args.get("domain").and_then(|v| v.as_str()) {
                    Some(d) if !d.trim().is_empty() => d.trim(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("call_service requires domain parameter".into()),
                        });
                    }
                };
                let service = match args.get("service").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s.trim(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("call_service requires service parameter".into()),
                        });
                    }
                };
                if !self.is_domain_allowed(domain) {
                    let allowed = if self.allowed_domains.is_empty() {
                        "(none — allowed_domains is empty)".to_string()
                    } else {
                        self.allowed_domains.join(", ")
                    };
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Service domain '{domain}' is not in allowed_domains. Allowed: {allowed}"
                        )),
                    });
                }
                let service_data = args.get("service_data");
                self.call_service(domain, service, service_data).await
            }
            _ => unreachable!(),
        };

        match result {
            Ok(value) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool() -> HomeAssistantTool {
        HomeAssistantTool::new(
            "http://homeassistant.local:8123".into(),
            "test-token".into(),
            vec!["light".into(), "switch".into()],
            15,
            Arc::new(SecurityPolicy::default()),
        )
    }

    #[test]
    fn name_is_home_assistant() {
        assert_eq!(test_tool().name(), "home_assistant");
    }

    #[test]
    fn description_is_non_empty() {
        assert!(!test_tool().description().is_empty());
    }

    #[test]
    fn parameters_schema_requires_action() {
        let schema = test_tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[test]
    fn parameters_schema_lists_all_actions() {
        let schema = test_tool().parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let names: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        for expected in &["get_state", "list_states", "list_services", "call_service"] {
            assert!(names.contains(expected), "missing action: {expected}");
        }
    }

    #[test]
    fn base_url_trailing_slash_stripped() {
        let tool = HomeAssistantTool::new(
            "http://hass.local:8123/".into(),
            "t".into(),
            vec![],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        assert_eq!(tool.base_url, "http://hass.local:8123");
    }

    #[test]
    fn allowed_domains_trimmed_and_empty_dropped() {
        let tool = HomeAssistantTool::new(
            "http://h".into(),
            "t".into(),
            vec!["  light ".into(), "".into(), "switch".into()],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        assert!(tool.is_domain_allowed("light"));
        assert!(tool.is_domain_allowed("switch"));
        assert!(!tool.is_domain_allowed(""));
    }

    #[tokio::test]
    async fn execute_missing_action_returns_error() {
        let result = test_tool().execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("action"));
    }

    #[tokio::test]
    async fn execute_unknown_action_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "nope"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn execute_get_state_missing_entity_id_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "get_state"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("entity_id"));
    }

    #[tokio::test]
    async fn execute_call_service_missing_domain_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("domain"));
    }

    #[tokio::test]
    async fn execute_call_service_missing_service_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "domain": "light"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("service"));
    }

    #[tokio::test]
    async fn execute_call_service_disallowed_domain_returns_error() {
        let result = test_tool()
            .execute(json!({
                "action": "call_service",
                "domain": "lock",
                "service": "unlock"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not in allowed_domains"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_call_service_empty_allowed_domains_blocks_all() {
        let tool = HomeAssistantTool::new(
            "http://h".into(),
            "t".into(),
            vec![],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({
                "action": "call_service",
                "domain": "light",
                "service": "turn_on"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("none"));
    }

    #[test]
    fn spec_reflects_metadata() {
        let tool = test_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "home_assistant");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters.is_object());
    }
}
