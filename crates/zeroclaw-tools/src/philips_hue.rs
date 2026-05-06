//! Philips Hue tool — local Hue Bridge v2 CLIP API client.
//!
//! Authenticates with an `application_key` (the bridge "username" minted
//! via push-button pairing). Sent in the `hue-application-key` header.
//!
//! Read actions (`list_*`, `get_light`) require `ToolOperation::Read`.
//! Mutating actions (`set_light`, `recall_scene`, `set_group`) require
//! `ToolOperation::Act` and are further restricted to resource types in
//! `allowed_resource_types`.
//!
//! TLS: bridges ship with self-signed certs on the local network, so the
//! client accepts invalid certs unless the operator opts into verification
//! by setting `verify_tls = true`.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

const MAX_ERROR_BODY_CHARS: usize = 500;

/// Tool for interacting with a Philips Hue Bridge over its v2 CLIP API.
pub struct PhilipsHueTool {
    bridge_address: String,
    application_key: String,
    allowed_resource_types: Vec<String>,
    request_timeout_secs: u64,
    http: reqwest::Client,
    security: Arc<SecurityPolicy>,
}

impl PhilipsHueTool {
    /// Create a new Philips Hue tool.
    ///
    /// Returns an error only if the underlying `reqwest::Client` cannot be
    /// constructed (e.g. system TLS init failure). `verify_tls = false`
    /// flips on `danger_accept_invalid_certs` because Hue bridges present
    /// a self-signed cert on the local network.
    pub fn new(
        bridge_address: String,
        application_key: String,
        allowed_resource_types: Vec<String>,
        verify_tls: bool,
        request_timeout_secs: u64,
        security: Arc<SecurityPolicy>,
    ) -> anyhow::Result<Self> {
        let bridge_address = bridge_address.trim().trim_end_matches('/').to_string();
        let allowed_resource_types = allowed_resource_types
            .into_iter()
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect();
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(!verify_tls)
            .timeout(Duration::from_secs(request_timeout_secs.max(1)))
            .build()?;
        Ok(Self {
            bridge_address,
            application_key,
            allowed_resource_types,
            request_timeout_secs,
            http,
            security,
        })
    }

    fn headers(&self) -> anyhow::Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "hue-application-key",
            self.application_key
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid Hue application_key header: {e}"))?,
        );
        headers.insert("Content-Type", "application/json".parse().unwrap());
        Ok(headers)
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs.max(1))
    }

    fn is_resource_type_allowed(&self, resource_type: &str) -> bool {
        self.allowed_resource_types
            .iter()
            .any(|t| t == resource_type)
    }

    fn resource_url(&self, resource_type: &str, id: Option<&str>) -> String {
        let base = format!(
            "https://{}/clip/v2/resource/{resource_type}",
            self.bridge_address
        );
        match id {
            Some(id) => format!("{base}/{id}"),
            None => base,
        }
    }

    async fn list_resource(&self, resource_type: &str) -> anyhow::Result<Value> {
        let url = self.resource_url(resource_type, None);
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
            anyhow::bail!("Philips Hue list {resource_type} failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    async fn get_resource(&self, resource_type: &str, id: &str) -> anyhow::Result<Value> {
        let url = self.resource_url(resource_type, Some(id));
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
            anyhow::bail!("Philips Hue get {resource_type}/{id} failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    async fn put_resource(
        &self,
        resource_type: &str,
        id: &str,
        body: &Value,
    ) -> anyhow::Result<Value> {
        let url = self.resource_url(resource_type, Some(id));
        let resp = self
            .http
            .put(&url)
            .headers(self.headers()?)
            .json(body)
            .timeout(self.timeout())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Philips Hue put {resource_type}/{id} failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }

    /// Build a v2 light state body from optional flags.
    fn build_light_state(
        on: Option<bool>,
        brightness: Option<f64>,
        color_xy: Option<(f64, f64)>,
        color_temperature_mirek: Option<u32>,
    ) -> Value {
        let mut body = json!({});
        if let Some(on) = on {
            body["on"] = json!({ "on": on });
        }
        if let Some(b) = brightness {
            body["dimming"] = json!({ "brightness": b });
        }
        if let Some((x, y)) = color_xy {
            body["color"] = json!({ "xy": { "x": x, "y": y } });
        }
        if let Some(mirek) = color_temperature_mirek {
            body["color_temperature"] = json!({ "mirek": mirek });
        }
        body
    }
}

#[async_trait]
impl Tool for PhilipsHueTool {
    fn name(&self) -> &str {
        "philips_hue"
    }

    fn description(&self) -> &str {
        "Control a local Philips Hue Bridge via the v2 CLIP API. Read \
         lights, scenes, rooms, and groups (list_lights, get_light, \
         list_scenes, list_rooms, list_groups). Mutate state with \
         set_light, recall_scene, or set_group — restricted to the \
         operator-configured allowed_resource_types."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "list_lights",
                        "get_light",
                        "set_light",
                        "list_scenes",
                        "recall_scene",
                        "list_rooms",
                        "list_groups",
                        "set_group"
                    ],
                    "description": "The Philips Hue operation to perform."
                },
                "id": {
                    "type": "string",
                    "description": "Resource ID (UUID). Required for get_light, set_light, recall_scene, set_group."
                },
                "on": {
                    "type": "boolean",
                    "description": "Power state for set_light / set_group."
                },
                "brightness": {
                    "type": "number",
                    "minimum": 0,
                    "maximum": 100,
                    "description": "Brightness percentage 0-100 for set_light / set_group."
                },
                "color_xy": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "number" },
                        "y": { "type": "number" }
                    },
                    "description": "CIE 1931 xy color coordinates for set_light / set_group."
                },
                "color_temperature_mirek": {
                    "type": "integer",
                    "minimum": 153,
                    "maximum": 500,
                    "description": "Color temperature in mirek (153-500) for set_light / set_group."
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

        let (operation, resource_type) = match action {
            "list_lights" | "get_light" => (ToolOperation::Read, "light"),
            "set_light" => (ToolOperation::Act, "light"),
            "list_scenes" => (ToolOperation::Read, "scene"),
            "recall_scene" => (ToolOperation::Act, "scene"),
            "list_rooms" => (ToolOperation::Read, "room"),
            "list_groups" => (ToolOperation::Read, "grouped_light"),
            "set_group" => (ToolOperation::Act, "grouped_light"),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action: {action}. Valid actions: list_lights, get_light, set_light, list_scenes, recall_scene, list_rooms, list_groups, set_group"
                    )),
                });
            }
        };

        if let Err(error) = self
            .security
            .enforce_tool_operation(operation, "philips_hue")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        if matches!(operation, ToolOperation::Act) && !self.is_resource_type_allowed(resource_type)
        {
            let allowed = if self.allowed_resource_types.is_empty() {
                "(none — allowed_resource_types is empty)".to_string()
            } else {
                self.allowed_resource_types.join(", ")
            };
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Resource type '{resource_type}' is not in allowed_resource_types. Allowed: {allowed}"
                )),
            });
        }

        let result = match action {
            "list_lights" => self.list_resource("light").await,
            "list_scenes" => self.list_resource("scene").await,
            "list_rooms" => self.list_resource("room").await,
            "list_groups" => self.list_resource("grouped_light").await,
            "get_light" => match args.get("id").and_then(|v| v.as_str()) {
                Some(id) if !id.trim().is_empty() => self.get_resource("light", id.trim()).await,
                _ => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("get_light requires id parameter".into()),
                    });
                }
            },
            "set_light" | "set_group" => {
                let id = match args.get("id").and_then(|v| v.as_str()) {
                    Some(id) if !id.trim().is_empty() => id.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("{action} requires id parameter")),
                        });
                    }
                };
                let on = args.get("on").and_then(|v| v.as_bool());
                let brightness = args.get("brightness").and_then(|v| v.as_f64());
                let color_xy = args
                    .get("color_xy")
                    .and_then(|v| v.as_object())
                    .and_then(|o| {
                        let x = o.get("x").and_then(|v| v.as_f64())?;
                        let y = o.get("y").and_then(|v| v.as_f64())?;
                        Some((x, y))
                    });
                let mirek = args
                    .get("color_temperature_mirek")
                    .and_then(|v| v.as_u64())
                    .map(|m| m as u32);
                let body = Self::build_light_state(on, brightness, color_xy, mirek);
                if body.as_object().is_none_or(|o| o.is_empty()) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "{action} requires at least one of: on, brightness, color_xy, color_temperature_mirek"
                        )),
                    });
                }
                self.put_resource(resource_type, &id, &body).await
            }
            "recall_scene" => match args.get("id").and_then(|v| v.as_str()) {
                Some(id) if !id.trim().is_empty() => {
                    let body = json!({ "recall": { "action": "active" } });
                    self.put_resource("scene", id.trim(), &body).await
                }
                _ => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("recall_scene requires id parameter".into()),
                    });
                }
            },
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

    fn test_tool() -> PhilipsHueTool {
        PhilipsHueTool::new(
            "192.0.2.10".into(),
            "test-key".into(),
            vec![
                "light".into(),
                "grouped_light".into(),
                "scene".into(),
                "room".into(),
            ],
            false,
            15,
            Arc::new(SecurityPolicy::default()),
        )
        .expect("test client builds")
    }

    #[test]
    fn name_is_philips_hue() {
        assert_eq!(test_tool().name(), "philips_hue");
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
        for expected in &[
            "list_lights",
            "get_light",
            "set_light",
            "list_scenes",
            "recall_scene",
            "list_rooms",
            "list_groups",
            "set_group",
        ] {
            assert!(names.contains(expected), "missing action: {expected}");
        }
    }

    #[test]
    fn bridge_address_trailing_slash_stripped() {
        let tool = PhilipsHueTool::new(
            "192.0.2.10/".into(),
            "k".into(),
            vec!["light".into()],
            false,
            15,
            Arc::new(SecurityPolicy::default()),
        )
        .unwrap();
        assert_eq!(tool.bridge_address, "192.0.2.10");
    }

    #[test]
    fn allowed_resource_types_trimmed_and_empty_dropped() {
        let tool = PhilipsHueTool::new(
            "h".into(),
            "k".into(),
            vec!["  light ".into(), "".into(), "scene".into()],
            false,
            15,
            Arc::new(SecurityPolicy::default()),
        )
        .unwrap();
        assert!(tool.is_resource_type_allowed("light"));
        assert!(tool.is_resource_type_allowed("scene"));
        assert!(!tool.is_resource_type_allowed(""));
    }

    #[test]
    fn resource_url_shapes() {
        let tool = test_tool();
        assert_eq!(
            tool.resource_url("light", None),
            "https://192.0.2.10/clip/v2/resource/light"
        );
        assert_eq!(
            tool.resource_url("scene", Some("abc-123")),
            "https://192.0.2.10/clip/v2/resource/scene/abc-123"
        );
    }

    #[test]
    fn build_light_state_omits_unset_fields() {
        let body = PhilipsHueTool::build_light_state(Some(true), None, None, None);
        let obj = body.as_object().unwrap();
        assert!(obj.contains_key("on"));
        assert!(!obj.contains_key("dimming"));
        assert!(!obj.contains_key("color"));
        assert!(!obj.contains_key("color_temperature"));
    }

    #[test]
    fn build_light_state_emits_all_when_set() {
        let body =
            PhilipsHueTool::build_light_state(Some(true), Some(75.0), Some((0.4, 0.5)), Some(250));
        assert_eq!(body["on"]["on"], true);
        assert_eq!(body["dimming"]["brightness"], 75.0);
        assert_eq!(body["color"]["xy"]["x"], 0.4);
        assert_eq!(body["color"]["xy"]["y"], 0.5);
        assert_eq!(body["color_temperature"]["mirek"], 250);
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
    async fn execute_get_light_missing_id_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "get_light"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("id"));
    }

    #[tokio::test]
    async fn execute_set_light_missing_id_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "set_light", "on": true}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("id"));
    }

    #[tokio::test]
    async fn execute_set_light_no_state_fields_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "set_light", "id": "abc"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("at least one of"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_recall_scene_missing_id_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "recall_scene"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("id"));
    }

    #[tokio::test]
    async fn execute_set_light_blocked_when_resource_type_not_allowed() {
        let tool = PhilipsHueTool::new(
            "h".into(),
            "k".into(),
            vec!["scene".into()], // light is NOT allowed
            false,
            15,
            Arc::new(SecurityPolicy::default()),
        )
        .unwrap();
        let result = tool
            .execute(json!({"action": "set_light", "id": "abc", "on": true}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not in allowed_resource_types"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_set_light_blocked_when_allowlist_empty() {
        let tool = PhilipsHueTool::new(
            "h".into(),
            "k".into(),
            vec![],
            false,
            15,
            Arc::new(SecurityPolicy::default()),
        )
        .unwrap();
        let result = tool
            .execute(json!({"action": "set_light", "id": "abc", "on": true}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("none"));
    }

    #[test]
    fn spec_reflects_metadata() {
        let tool = test_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "philips_hue");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters.is_object());
    }
}
