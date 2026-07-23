use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};
use zeroclaw_config::schema::{Config, HomeAssistantConfig};

const HA_REQUEST_TIMEOUT_SECS: u64 = 30;
const HA_CONNECT_TIMEOUT_SECS: u64 = 10;
const HA_PROXY_SERVICE_KEY: &str = "tool.homeassistant";
/// Maximum number of characters to include from an error response body.
const MAX_ERROR_BODY_CHARS: usize = 500;

const TOOL_DESCRIPTION_KEY: &str = "tool-homeassistant";
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

fn tool_msg(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_tool_string_with_args(key, args)
}

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
        Err(anyhow::Error::msg(tool_msg_with_args(
            "tool-homeassistant-error-invalid-slug",
            &[("kind", kind), ("value", value)],
        )))
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
        Err(anyhow::Error::msg(tool_msg_with_args(
            "tool-homeassistant-error-invalid-entity-id",
            &[("entity_id", entity_id)],
        )))
    }
}

/// Resolves per-call Home Assistant configuration (url, token, allowed
/// domains) from the runtime's canonical config so a config reload is
/// observed without rebuilding the tool. Mirrors `send_via::AgentPeerGroupResolver`.
pub type HomeAssistantConfigResolver = Arc<dyn Fn() -> HomeAssistantConfig + Send + Sync>;

/// Build a resolver backed by the runtime's live (canonical) config handle.
/// Every call re-reads `live.read().homeassistant`, so a config reload takes
/// effect on the very next tool invocation.
pub fn live_config_resolver(live: Arc<parking_lot::RwLock<Config>>) -> HomeAssistantConfigResolver {
    Arc::new(move || live.read().homeassistant.clone())
}

/// Build a resolver over a fixed snapshot. Used when no live config handle is
/// available (e.g. one-shot callers) — matches `root_config` fallback used by
/// `send_via`'s peer-group resolver in `all_tools_with_runtime`.
pub fn snapshot_config_resolver(config: HomeAssistantConfig) -> HomeAssistantConfigResolver {
    Arc::new(move || config.clone())
}

/// Tool for interacting with a Home Assistant instance over its native REST
/// API (`HASS_URL` + a long-lived access token).
///
/// Actions are gated by the appropriate security operation:
/// - `list_entities` / `get_state` are read-only (`Read`).
/// - `call_service` mutates device state (`Act`), and is additionally
///   constrained by the operator's `homeassistant.allowed_domains` config
///   (deny-by-default: empty allowlist blocks every `call_service` call).
///
/// `url`, `token`, and `allowed_domains` are resolved fresh from canonical
/// config on every call via `config` (see [`HomeAssistantConfigResolver`]) —
/// the tool holds no copied credential/policy state, so a config reload is
/// observed without rebuilding the tool. The HTTP client is cached
/// separately through the runtime proxy-client facility.
///
/// This intentionally stays small (read state + a guarded service call). It is
/// NOT the Model Context Protocol server integration — it talks plain HA REST.
pub struct HomeAssistantTool {
    config: HomeAssistantConfigResolver,
    security: Arc<SecurityPolicy>,
}

impl HomeAssistantTool {
    /// Create a new Home Assistant tool backed by `config`, a resolver that
    /// materializes url/token/allowed_domains fresh on every call.
    pub fn new(config: HomeAssistantConfigResolver, security: Arc<SecurityPolicy>) -> Self {
        Self { config, security }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            HA_PROXY_SERVICE_KEY,
            HA_REQUEST_TIMEOUT_SECS,
            HA_CONNECT_TIMEOUT_SECS,
        )
    }

    fn authed(&self, token: &str, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.bearer_auth(token)
            .timeout(Duration::from_secs(HA_REQUEST_TIMEOUT_SECS))
    }

    /// List all entity ids and their current state (compact — no attributes),
    /// optionally filtered to a single domain prefix (e.g. `light`).
    async fn list_entities(
        &self,
        base_url: &str,
        token: &str,
        domain: Option<&str>,
    ) -> anyhow::Result<Value> {
        if let Some(d) = domain {
            validate_slug("domain", d)?;
        }
        let url = format!("{base_url}/api/states");
        let resp = self
            .authed(token, self.http_client().get(&url))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            return Err(anyhow::Error::msg(tool_msg_with_args(
                "tool-homeassistant-error-list-entities-failed",
                &[("status", &status.to_string()), ("body", &truncated)],
            )));
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
    async fn get_state(
        &self,
        base_url: &str,
        token: &str,
        entity_id: &str,
    ) -> anyhow::Result<Value> {
        validate_entity_id(entity_id)?;
        let url = format!("{base_url}/api/states/{entity_id}");
        let resp = self
            .authed(token, self.http_client().get(&url))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            return Err(anyhow::Error::msg(tool_msg_with_args(
                "tool-homeassistant-error-get-state-failed",
                &[("status", &status.to_string()), ("body", &truncated)],
            )));
        }
        resp.json().await.map_err(|e| {
            anyhow::Error::msg(tool_msg_with_args(
                "tool-homeassistant-error-decode-failed",
                &[("err", &e.to_string())],
            ))
        })
    }

    /// Call a service (`POST /api/services/<domain>/<service>`) with an optional
    /// JSON service-data body (e.g. `{ "entity_id": "light.kitchen" }`).
    ///
    /// Enforces the operator's `allowed_domains` boundary BEFORE the request
    /// is sent — this is authorization on top of (not instead of) the
    /// `ToolOperation::Act` gate already applied by the caller.
    async fn call_service(
        &self,
        base_url: &str,
        token: &str,
        allowed_domains: &[String],
        domain: &str,
        service: &str,
        service_data: Option<&Value>,
    ) -> anyhow::Result<Value> {
        validate_slug("domain", domain)?;
        validate_slug("service", service)?;
        if !allowed_domains.iter().any(|d| d == domain) {
            let allowed = if allowed_domains.is_empty() {
                tool_msg("tool-homeassistant-allowed-domains-empty")
            } else {
                allowed_domains.join(", ")
            };
            return Err(anyhow::Error::msg(tool_msg_with_args(
                "tool-homeassistant-error-domain-not-allowed",
                &[("domain", domain), ("allowed", &allowed)],
            )));
        }
        let url = format!("{base_url}/api/services/{domain}/{service}");
        let body = service_data.cloned().unwrap_or_else(|| json!({}));
        let resp = self
            .authed(token, self.http_client().post(&url))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&text, MAX_ERROR_BODY_CHARS);
            return Err(anyhow::Error::msg(tool_msg_with_args(
                "tool-homeassistant-error-call-service-failed",
                &[("status", &status.to_string()), ("body", &truncated)],
            )));
        }
        // HA returns a JSON array of changed states on success (may be
        // empty), but a 2xx with an undecodable body is NOT a documented
        // empty-body success shape — treat it as an error rather than
        // silently reporting a false success receipt for a physical action.
        let text = resp.text().await.unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(json!([]));
        }
        serde_json::from_str::<Value>(&text).map_err(|e| {
            anyhow::Error::msg(tool_msg_with_args(
                "tool-homeassistant-error-call-service-malformed-body",
                &[("err", &e.to_string())],
            ))
        })
    }
}

#[async_trait]
impl Tool for HomeAssistantTool {
    fn name(&self) -> &str {
        "homeassistant"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| crate::i18n::get_required_tool_string(TOOL_DESCRIPTION_KEY))
            .as_str()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list_entities", "get_state", "call_service"],
                    "description": tool_msg("tool-homeassistant-param-action")
                },
                "entity_id": {
                    "type": "string",
                    "description": tool_msg("tool-homeassistant-param-entity-id")
                },
                "domain": {
                    "type": "string",
                    "description": tool_msg("tool-homeassistant-param-domain")
                },
                "service": {
                    "type": "string",
                    "description": tool_msg("tool-homeassistant-param-service")
                },
                "service_data": {
                    "type": "object",
                    "description": tool_msg("tool-homeassistant-param-service-data")
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
                    output: ToolOutput::default(),
                    error: Some(tool_msg("tool-homeassistant-error-missing-action")),
                });
            }
        };

        let operation = match action {
            "list_entities" | "get_state" => ToolOperation::Read,
            "call_service" => ToolOperation::Act,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_msg_with_args(
                        "tool-homeassistant-error-unknown-action",
                        &[("action", action)],
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
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        // Materialize url/token/allowed_domains fresh from canonical config
        // for this call — never a copied field on `self` (SSOT).
        let cfg = (self.config)();
        let base_url = if cfg.url.trim().is_empty() {
            std::env::var("HASS_URL").unwrap_or_default()
        } else {
            cfg.url.trim().to_string()
        };
        let base_url = base_url.trim_end_matches('/').to_string();
        let token = if cfg.token.trim().is_empty() {
            std::env::var("HASS_TOKEN").unwrap_or_default()
        } else {
            cfg.token.trim().to_string()
        };
        if base_url.is_empty() || token.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-homeassistant-error-not-configured")),
            });
        }

        let result = match action {
            "list_entities" => {
                let domain = args.get("domain").and_then(|v| v.as_str());
                self.list_entities(&base_url, &token, domain).await
            }
            "get_state" => {
                let entity_id = match args.get("entity_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.trim().is_empty() => id,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(tool_msg("tool-homeassistant-error-missing-entity-id")),
                        });
                    }
                };
                self.get_state(&base_url, &token, entity_id).await
            }
            "call_service" => {
                let domain = match args.get("domain").and_then(|v| v.as_str()) {
                    Some(d) if !d.trim().is_empty() => d,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(tool_msg("tool-homeassistant-error-missing-domain")),
                        });
                    }
                };
                let service = match args.get("service").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(tool_msg("tool-homeassistant-error-missing-service")),
                        });
                    }
                };
                let service_data = args.get("service_data");
                self.call_service(
                    &base_url,
                    &token,
                    &cfg.allowed_domains,
                    domain,
                    service,
                    service_data,
                )
                .await
            }
            _ => unreachable!(), // Already handled above
        };

        match result {
            Ok(value) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&value)
                    .unwrap_or_else(|_| value.to_string())
                    .into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
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

    fn test_config() -> HomeAssistantConfig {
        HomeAssistantConfig {
            enabled: true,
            url: "http://localhost:8123/".into(),
            token: "test-token".into(),
            allowed_domains: vec!["light".into(), "switch".into()],
        }
    }

    fn test_tool() -> HomeAssistantTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        HomeAssistantTool::new(snapshot_config_resolver(test_config()), security)
    }

    #[test]
    fn tool_name_is_homeassistant() {
        assert_eq!(test_tool().name(), "homeassistant");
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
        let err = result.error.unwrap();
        assert!(err.contains("explode"), "got: {err}");
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
        let tool = HomeAssistantTool::new(snapshot_config_resolver(test_config()), security);
        let result = tool
            .execute(json!({"action": "call_service", "domain": "light", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_not_configured_returns_error_when_url_or_token_empty() {
        let mut cfg = test_config();
        cfg.url = String::new();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = HomeAssistantTool::new(snapshot_config_resolver(cfg), security);
        let result = tool
            .execute(json!({"action": "list_entities"}))
            .await
            .unwrap();
        assert!(!result.success);
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
                .contains("../../etc/passwd")
        );
    }

    #[tokio::test]
    async fn execute_call_service_rejects_traversal_domain() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "domain": "../../etc", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("../../etc"));
    }

    #[tokio::test]
    async fn execute_call_service_rejects_uppercase_service() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "domain": "light", "service": "Turn_On"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Turn_On"));
    }

    #[tokio::test]
    async fn execute_list_entities_rejects_bad_domain() {
        let result = test_tool()
            .execute(json!({"action": "list_entities", "domain": "../etc"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("../etc"));
    }

    // ── allowed_domains policy tests ─────────────────────────────────────────

    #[tokio::test]
    async fn call_service_allowed_domain_passes_policy_check() {
        // Domain is allowed by policy but there's no live HA server, so this
        // exercises the policy gate: assert the error is a network/connect
        // failure, NOT a "not in allowed_domains" rejection.
        let result = test_tool()
            .execute(json!({"action": "call_service", "domain": "light", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            !err.contains("not in allowed_domains") && !err.contains("not allowed"),
            "domain 'light' should pass the allowlist check: {err}"
        );
    }

    #[tokio::test]
    async fn call_service_denied_domain_is_rejected_before_http() {
        let result = test_tool()
            .execute(json!({"action": "call_service", "domain": "lock", "service": "unlock"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("lock"), "got: {err}");
    }

    #[tokio::test]
    async fn call_service_empty_allowed_domains_blocks_all() {
        let mut cfg = test_config();
        cfg.allowed_domains = Vec::new();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = HomeAssistantTool::new(snapshot_config_resolver(cfg), security);
        let result = tool
            .execute(json!({"action": "call_service", "domain": "light", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("light"), "got: {err}");
    }

    // ── SSOT: config resolved live at call time ──────────────────────────────

    #[tokio::test]
    async fn config_update_is_observed_without_rebuilding_tool() {
        // The same long-lived HomeAssistantTool must reflect a config reload
        // (allowed_domains changing) without being rebuilt — it resolves its
        // policy from the live source each call rather than a copied field.
        let live: Arc<parking_lot::RwLock<Config>> = Arc::new(parking_lot::RwLock::new(Config {
            homeassistant: HomeAssistantConfig {
                allowed_domains: vec!["light".into()],
                ..test_config()
            },
            ..Config::default()
        }));
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = HomeAssistantTool::new(live_config_resolver(Arc::clone(&live)), security);

        // Initially "lock" is not allowed.
        let result = tool
            .execute(json!({"action": "call_service", "domain": "lock", "service": "unlock"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("lock"));

        // Reload config to widen the allowlist — no tool rebuild.
        live.write().homeassistant.allowed_domains = vec!["light".into(), "lock".into()];

        // Now "lock" passes the allowlist check (fails downstream on the
        // network call instead, since there's no live HA server).
        let result = tool
            .execute(json!({"action": "call_service", "domain": "lock", "service": "unlock"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            !err.contains("not in allowed_domains") && !err.contains("not allowed"),
            "domain 'lock' should pass the allowlist check after reload: {err}"
        );
    }

    // ── Proxy routing ─────────────────────────────────────────────────────────

    #[test]
    fn homeassistant_is_a_supported_proxy_service_key() {
        assert!(
            zeroclaw_config::schema::ProxyConfig::supported_service_keys()
                .contains(&HA_PROXY_SERVICE_KEY)
        );
    }

    #[test]
    fn service_scoped_proxy_applies_to_homeassistant_when_listed() {
        use zeroclaw_config::schema::{ProxyConfig, ProxyScope};

        let proxy = ProxyConfig {
            enabled: true,
            scope: ProxyScope::Services,
            services: vec![HA_PROXY_SERVICE_KEY.to_string()],
            ..ProxyConfig::default()
        };
        assert!(proxy.should_apply_to_service(HA_PROXY_SERVICE_KEY));
        assert!(!proxy.should_apply_to_service("tool.unrelated"));
    }

    // ── HTTP-level (wiremock) tests ──────────────────────────────────────────

    fn wiremock_tool(base_url: String, autonomy: AutonomyLevel) -> HomeAssistantTool {
        let security = Arc::new(SecurityPolicy {
            autonomy,
            ..SecurityPolicy::default()
        });
        let cfg = HomeAssistantConfig {
            enabled: true,
            url: base_url,
            token: "test-token".into(),
            allowed_domains: vec!["light".into()],
        };
        HomeAssistantTool::new(snapshot_config_resolver(cfg), security)
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

    #[tokio::test]
    async fn call_service_denied_domain_never_hits_http() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // No mock mounted for /api/services/lock/unlock — if the request
        // reached the server at all, `verify()` on an empty expectation set
        // is a no-op, so we assert on the *tool* error message instead to
        // pin the pre-HTTP rejection.
        Mock::given(method("POST"))
            .and(path("/api/services/light/turn_on"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;

        let tool = wiremock_tool(server.uri(), AutonomyLevel::Supervised);
        let result = tool
            .execute(json!({"action": "call_service", "domain": "lock", "service": "unlock"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("lock"));
        server.verify().await;
    }

    #[tokio::test]
    async fn call_service_2xx_malformed_body_is_reported_as_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // A 2xx status with a body that isn't valid JSON must NOT collapse
        // into a synthetic `[]` success for a physical action.
        Mock::given(method("POST"))
            .and(path("/api/services/light/turn_on"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not valid json {"))
            .expect(1)
            .mount(&server)
            .await;

        let tool = wiremock_tool(server.uri(), AutonomyLevel::Supervised);
        let result = tool
            .execute(json!({"action": "call_service", "domain": "light", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(
            !result.success,
            "malformed 2xx body must not report success"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn call_service_2xx_empty_body_is_documented_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/services/light/turn_on"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .expect(1)
            .mount(&server)
            .await;

        let tool = wiremock_tool(server.uri(), AutonomyLevel::Supervised);
        let result = tool
            .execute(json!({"action": "call_service", "domain": "light", "service": "turn_on"}))
            .await
            .unwrap();
        assert!(
            result.success,
            "a genuinely empty 2xx body is a documented success: {:?}",
            result.error
        );
        server.verify().await;
    }
}
