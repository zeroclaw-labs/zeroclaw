use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

const HA_REQUEST_TIMEOUT_SECS: u64 = 30;
/// Maximum number of characters to include from an error response body.
const MAX_ERROR_BODY_CHARS: usize = 500;

// ── Input validation ──────────────────────────────────────────────────────────

/// True when `slug` is a non-empty lowercase Home Assistant slug matching
/// `^[a-z0-9_]+$` (the character class HA uses for domains, services, and
/// entity object ids).
fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Validates a Home Assistant `domain` or `service` name. Prevents path
/// traversal if a crafted value like `../../other` were interpolated directly
/// into the request URL.
fn validate_slug(kind: &str, value: &str) -> anyhow::Result<()> {
    if is_valid_slug(value) {
        Ok(())
    } else {
        anyhow::bail!(
            "Invalid {kind} '{value}'. Expected a lowercase slug matching [a-z0-9_]+ (e.g. light, turn_on)"
        )
    }
}

/// Validates an entity id of the form `<domain>.<object_id>` where both parts
/// match `^[a-z0-9_]+$` (exactly one dot). Prevents path traversal via crafted
/// ids interpolated into the request URL.
fn validate_entity_id(entity_id: &str) -> anyhow::Result<()> {
    let valid = entity_id
        .split_once('.')
        .is_some_and(|(domain, object_id)| is_valid_slug(domain) && is_valid_slug(object_id));
    if valid {
        Ok(())
    } else {
        anyhow::bail!(
            "Invalid entity_id '{entity_id}'. Expected format: <domain>.<object_id> with both parts matching [a-z0-9_]+ (e.g. light.kitchen)"
        )
    }
}

/// Tool for interacting with a Home Assistant instance over its native REST
/// API (`HASS_URL` + a long-lived access token).
///
/// Actions are gated by the appropriate security operation:
/// - `list_entities` / `get_state` are read-only (`Read`).
/// - `call_service` mutates device state (`Act`).
///
/// This intentionally stays small (read state + a guarded service call). It is
/// NOT the Model Context Protocol server integration — it talks plain HA REST.
pub struct HomeAssistantTool {
    base_url: String,
    token: String,
    http: reqwest::Client,
    security: Arc<SecurityPolicy>,
}

impl HomeAssistantTool {
    /// Create a new Home Assistant tool. `base_url` is the HA origin
    /// (e.g. `http://192.0.2.10:8123`); `token` is a long-lived access token.
    pub fn new(base_url: String, token: String, security: Arc<SecurityPolicy>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
            security,
        }
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.bearer_auth(&self.token)
            .timeout(Duration::from_secs(HA_REQUEST_TIMEOUT_SECS))
    }

    /// List all entity ids and their current state (compact — no attributes),
    /// optionally filtered to a single domain prefix (e.g. `light`).
    async fn list_entities(&self, domain: Option<&str>) -> anyhow::Result<Value> {
        if let Some(d) = domain {
            validate_slug("domain", d)?;
        }
        let url = format!("{}/api/states", self.base_url);
        let resp = self.authed(self.http.get(&url)).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Home Assistant list_entities failed ({status}): {truncated}");
        }
        let states: Value = resp.json().await?;
        let prefix = domain.map(|d| format!("{}.", d.trim_end_matches('.')));
        let entities: Vec<Value> = states
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter(|e| {
                        let id = e.get("entity_id").and_then(|v| v.as_str()).unwrap_or("");
                        prefix.as_deref().is_none_or(|p| id.starts_with(p))
                    })
                    .map(|e| {
                        json!({
                            "entity_id": e.get("entity_id").cloned().unwrap_or(Value::Null),
                            "state": e.get("state").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({ "count": entities.len(), "entities": entities }))
    }

    /// Read the full state (including attributes) of one entity.
    async fn get_state(&self, entity_id: &str) -> anyhow::Result<Value> {
        validate_entity_id(entity_id)?;
        let url = format!("{}/api/states/{entity_id}", self.base_url);
        let resp = self.authed(self.http.get(&url)).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Home Assistant get_state failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    /// Call a service (`POST /api/services/<domain>/<service>`) with an optional
    /// JSON service-data body (e.g. `{ "entity_id": "light.kitchen" }`).
    async fn call_service(
        &self,
        domain: &str,
        service: &str,
        service_data: Option<&Value>,
    ) -> anyhow::Result<Value> {
        validate_slug("domain", domain)?;
        validate_slug("service", service)?;
        let url = format!("{}/api/services/{domain}/{service}", self.base_url);
        let body = service_data.cloned().unwrap_or_else(|| json!({}));
        let resp = self.authed(self.http.post(&url)).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Home Assistant call_service failed ({status}): {truncated}");
        }
        // HA returns a JSON array of changed states (may be empty).
        let value: Value = resp.json().await.unwrap_or_else(|_| json!([]));
        Ok(value)
    }
}

#[async_trait]
impl Tool for HomeAssistantTool {
    fn name(&self) -> &str {
        "homeassistant"
    }

    fn description(&self) -> &str {
        "Control and query a Home Assistant smart-home instance over its REST API. \
         Actions: 'list_entities' (all entity ids + state, optionally filtered by domain \
         such as 'light' or 'sensor'), 'get_state' (full state + attributes of one entity), \
         and 'call_service' (invoke a service like light.turn_on with service_data). \
         list_entities and get_state are read-only; call_service changes device state."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list_entities", "get_state", "call_service"],
                    "description": "The Home Assistant action to perform"
                },
                "entity_id": {
                    "type": "string",
                    "description": "Entity id for get_state (e.g. 'light.kitchen')"
                },
                "domain": {
                    "type": "string",
                    "description": "Domain filter for list_entities, or the service domain for call_service (e.g. 'light')"
                },
                "service": {
                    "type": "string",
                    "description": "Service name for call_service (e.g. 'turn_on')"
                },
                "service_data": {
                    "type": "object",
                    "description": "Optional JSON body for call_service (e.g. {\"entity_id\": \"light.kitchen\"})"
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
            "list_entities" | "get_state" => ToolOperation::Read,
            "call_service" => ToolOperation::Act,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action: {action}. Valid actions: list_entities, get_state, call_service"
                    )),
                });
            }
        };

        if let Err(error) = self
            .security
            .enforce_tool_operation(operation, "homeassistant")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let result = match action {
            "list_entities" => {
                let domain = args.get("domain").and_then(|v| v.as_str());
                self.list_entities(domain).await
            }
            "get_state" => {
                let entity_id = match args.get("entity_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.trim().is_empty() => id,
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
                    Some(d) if !d.trim().is_empty() => d,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("call_service requires domain parameter".into()),
                        });
                    }
                };
                let service = match args.get("service").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("call_service requires service parameter".into()),
                        });
                    }
                };
                let service_data = args.get("service_data");
                self.call_service(domain, service, service_data).await
            }
            _ => unreachable!(), // Already handled above
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
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool() -> HomeAssistantTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        HomeAssistantTool::new(
            "http://localhost:8123/".into(),
            "test-token".into(),
            security,
        )
    }

    #[test]
    fn tool_name_is_homeassistant() {
        assert_eq!(test_tool().name(), "homeassistant");
    }

    #[test]
    fn base_url_trailing_slash_trimmed() {
        let tool = test_tool();
        assert_eq!(tool.base_url, "http://localhost:8123");
    }

    #[test]
    fn schema_requires_action_and_lists_all() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
        let actions: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(actions.contains(&"list_entities"));
        assert!(actions.contains(&"get_state"));
        assert!(actions.contains(&"call_service"));
    }

    #[tokio::test]
    async fn execute_missing_action_returns_error() {
        let result = test_tool().execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("action"));
    }

    #[tokio::test]
    async fn execute_unknown_action_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "explode"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn execute_get_state_missing_entity_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "get_state"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("entity_id"));
    }

    #[tokio::test]
    async fn execute_call_service_missing_domain_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("domain"));
    }

    #[tokio::test]
    async fn execute_call_service_missing_service_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "domain": "light"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("service"));
    }

    #[tokio::test]
    async fn call_service_blocked_in_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = HomeAssistantTool::new("http://localhost:8123".into(), "t".into(), security);
        let result = tool
            .execute(json!({"action": "call_service", "domain": "light", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("read-only"));
    }

    // ── Input validation tests ──────────────────────────────────────────────

    #[test]
    fn validate_slug_accepts_lowercase_slugs() {
        assert!(validate_slug("domain", "light").is_ok());
        assert!(validate_slug("service", "turn_on").is_ok());
        assert!(validate_slug("domain", "sensor2").is_ok());
    }

    #[test]
    fn validate_slug_rejects_invalid() {
        assert!(validate_slug("domain", "").is_err()); // empty
        assert!(validate_slug("domain", "Light").is_err()); // uppercase
        assert!(validate_slug("service", "turn on").is_err()); // space
        assert!(validate_slug("domain", "../../etc/passwd").is_err()); // traversal
        assert!(validate_slug("service", "turn_on/../..").is_err()); // traversal
        assert!(validate_slug("domain", "light.kitchen").is_err()); // dot not allowed
    }

    #[test]
    fn validate_entity_id_accepts_valid_ids() {
        assert!(validate_entity_id("light.kitchen").is_ok());
        assert!(validate_entity_id("sensor.living_room_temp").is_ok());
        assert!(validate_entity_id("binary_sensor.front_door_2").is_ok());
    }

    #[test]
    fn validate_entity_id_rejects_invalid() {
        assert!(validate_entity_id("").is_err()); // empty
        assert!(validate_entity_id("light").is_err()); // missing dot
        assert!(validate_entity_id("light.").is_err()); // empty object id
        assert!(validate_entity_id(".kitchen").is_err()); // empty domain
        assert!(validate_entity_id("light.kitchen.extra").is_err()); // extra dots
        assert!(validate_entity_id("Light.Kitchen").is_err()); // uppercase
        assert!(validate_entity_id("../../secrets").is_err()); // traversal
        assert!(validate_entity_id("light/../../etc").is_err()); // traversal
    }

    #[tokio::test]
    async fn execute_get_state_rejects_traversal_entity_id() {
        let result = test_tool()
            .execute(json!({"action": "get_state", "entity_id": "../../etc/passwd"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("Invalid entity_id")
        );
    }

    #[tokio::test]
    async fn execute_call_service_rejects_traversal_domain() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "domain": "../../etc", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Invalid domain"));
    }

    #[tokio::test]
    async fn execute_call_service_rejects_uppercase_service() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "domain": "light", "service": "Turn_On"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Invalid service"));
    }

    #[tokio::test]
    async fn execute_list_entities_rejects_bad_domain() {
        let result = test_tool()
            .execute(json!({"action": "list_entities", "domain": "../etc"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Invalid domain"));
    }

    // ── HTTP-level (wiremock) tests ──────────────────────────────────────────

    fn wiremock_tool(base_url: String, autonomy: AutonomyLevel) -> HomeAssistantTool {
        let security = Arc::new(SecurityPolicy {
            autonomy,
            ..SecurityPolicy::default()
        });
        HomeAssistantTool::new(base_url, "test-token".into(), security)
    }

    #[tokio::test]
    async fn list_entities_filters_domain_and_projects_compactly() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/states"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "entity_id": "light.kitchen", "state": "on", "attributes": { "brightness": 200 } },
                { "entity_id": "light.bedroom", "state": "off", "attributes": {} },
                { "entity_id": "sensor.temp",   "state": "21", "attributes": {} }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let tool = wiremock_tool(server.uri(), AutonomyLevel::Supervised);
        let result = tool
            .execute(json!({"action": "list_entities", "domain": "light"}))
            .await
            .unwrap();
        assert!(result.success, "unexpected error: {:?}", result.error);
        let output: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["count"], 2);
        let entities = output["entities"].as_array().unwrap();
        assert_eq!(entities.len(), 2);
        // Compact projection: only entity_id + state, no attributes.
        assert_eq!(entities[0]["entity_id"], "light.kitchen");
        assert_eq!(entities[0]["state"], "on");
        assert!(entities[0].get("attributes").is_none());
        server.verify().await;
    }

    #[tokio::test]
    async fn read_action_succeeds_under_readonly_autonomy() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/states"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "entity_id": "light.kitchen", "state": "on" }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        // Positive gate: a Read action must SUCCEED under ReadOnly autonomy so
        // an over-blocking regression is caught (complements the Act-blocked case).
        let tool = wiremock_tool(server.uri(), AutonomyLevel::ReadOnly);
        let result = tool
            .execute(json!({"action": "list_entities"}))
            .await
            .unwrap();
        assert!(
            result.success,
            "read action should succeed under ReadOnly: {:?}",
            result.error
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn get_state_error_status_body_is_truncated() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let long_body = "E".repeat(MAX_ERROR_BODY_CHARS + 200);
        Mock::given(method("GET"))
            .and(path("/api/states/light.kitchen"))
            .respond_with(ResponseTemplate::new(500).set_body_string(long_body))
            .expect(1)
            .mount(&server)
            .await;

        let tool = wiremock_tool(server.uri(), AutonomyLevel::Supervised);
        let result = tool
            .execute(json!({"action": "get_state", "entity_id": "light.kitchen"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("500"));
        assert!(err.ends_with("..."), "expected truncation ellipsis: {err}");
        // Truncated well below the raw body length.
        assert!(err.len() < MAX_ERROR_BODY_CHARS + 200);
        server.verify().await;
    }

    #[tokio::test]
    async fn call_service_passes_body_when_args_provided() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let expected_body = json!({ "entity_id": "light.kitchen" });
        Mock::given(method("POST"))
            .and(path("/api/services/light/turn_on"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_json(&expected_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let tool = wiremock_tool(server.uri(), AutonomyLevel::Supervised);
        let result = tool
            .execute(json!({
                "action": "call_service",
                "domain": "light",
                "service": "turn_on",
                "service_data": { "entity_id": "light.kitchen" }
            }))
            .await
            .unwrap();
        assert!(result.success, "unexpected error: {:?}", result.error);
        server.verify().await;
    }

    #[tokio::test]
    async fn call_service_defaults_to_empty_body_when_no_args() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/services/light/turn_off"))
            .and(body_json(json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let tool = wiremock_tool(server.uri(), AutonomyLevel::Supervised);
        let result = tool
            .execute(json!({
                "action": "call_service",
                "domain": "light",
                "service": "turn_off"
            }))
            .await
            .unwrap();
        assert!(result.success, "unexpected error: {:?}", result.error);
        server.verify().await;
    }
}
