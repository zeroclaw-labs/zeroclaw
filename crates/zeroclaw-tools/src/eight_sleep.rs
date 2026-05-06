//! 8Sleep tool — unofficial cloud API client for the Pod.
//!
//! **API stability.** 8Sleep does not publish a stable public API. This
//! module talks to the same HTTPS endpoints the official mobile app
//! reaches, following the conventions popularized by the open-source
//! `pyEight` library. Endpoints can change at any time without notice;
//! treat this integration as best-effort and expect occasional breakage.
//!
//! Auth flow: POST `{email, password}` to `<api_base_url>/login`. The
//! response carries a session token; subsequent requests send it in the
//! `Session-Token` header. The token is cached in memory only and
//! refreshed automatically on `401`.
//!
//! Read actions (`get_bed_state`, `get_metrics`) require
//! `ToolOperation::Read`. The mutating action (`set_temperature`)
//! requires `ToolOperation::Act` and is further gated by `allowed_sides`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

const MAX_ERROR_BODY_CHARS: usize = 500;
/// Heating level is `-100..=100` per pyEight conventions: negative cools,
/// positive warms, 0 holds without active conditioning.
const HEATING_LEVEL_MIN: i64 = -100;
const HEATING_LEVEL_MAX: i64 = 100;

#[derive(Debug, Clone)]
struct SessionToken {
    token: String,
    user_id: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    session: LoginSession,
}

#[derive(Debug, Deserialize)]
struct LoginSession {
    token: String,
    #[serde(rename = "userId")]
    user_id: String,
}

/// Tool for interacting with an 8Sleep Pod via the unofficial cloud API.
pub struct EightSleepTool {
    email: String,
    password: String,
    api_base_url: String,
    allowed_sides: Vec<String>,
    request_timeout_secs: u64,
    token: Arc<Mutex<Option<SessionToken>>>,
    http: reqwest::Client,
    security: Arc<SecurityPolicy>,
}

impl EightSleepTool {
    /// Create a new 8Sleep tool. `api_base_url` is normalized by stripping
    /// any trailing `/`.
    pub fn new(
        email: String,
        password: String,
        api_base_url: String,
        allowed_sides: Vec<String>,
        request_timeout_secs: u64,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        let api_base_url = api_base_url.trim().trim_end_matches('/').to_string();
        let allowed_sides = allowed_sides
            .into_iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        Self {
            email,
            password,
            api_base_url,
            allowed_sides,
            request_timeout_secs,
            token: Arc::new(Mutex::new(None)),
            http: reqwest::Client::new(),
            security,
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs.max(1))
    }

    fn is_side_allowed(&self, side: &str) -> bool {
        self.allowed_sides.iter().any(|s| s == side)
    }

    /// Build the heating-level request body for `set_temperature`. Public
    /// for unit testing — exercised independently of the network.
    fn build_temperature_body(side: &str, level: i64) -> Value {
        let now_field = format!("{side}Now");
        let target_field = format!("{side}TargetHeatingLevel");
        let level_field = format!("{side}HeatingLevel");
        json!({
            now_field: true,
            target_field: level,
            level_field: level,
        })
    }

    /// POST credentials to `<api_base_url>/login` and cache the resulting
    /// session token. Replaces any prior cached token.
    async fn login(&self) -> anyhow::Result<SessionToken> {
        let url = format!("{}/login", self.api_base_url);
        let resp = self
            .http
            .post(&url)
            .json(&json!({
                "email": self.email,
                "password": self.password,
            }))
            .timeout(self.timeout())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("8Sleep login failed ({status}): {truncated}");
        }
        let parsed: LoginResponse = resp.json().await?;
        let token = SessionToken {
            token: parsed.session.token,
            user_id: parsed.session.user_id,
        };
        *self.token.lock().await = Some(token.clone());
        Ok(token)
    }

    async fn current_token(&self) -> anyhow::Result<SessionToken> {
        if let Some(t) = self.token.lock().await.clone() {
            return Ok(t);
        }
        self.login().await
    }

    fn auth_headers(token: &str) -> anyhow::Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Session-Token",
            token
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid 8Sleep session token: {e}"))?,
        );
        headers.insert("Content-Type", "application/json".parse().unwrap());
        Ok(headers)
    }

    /// Issue a GET that retries once with a fresh token on `401`.
    async fn get_authed(&self, path_after_base: &str) -> anyhow::Result<Value> {
        let url = format!("{}{}", self.api_base_url, path_after_base);
        let mut token = self.current_token().await?;
        for attempt in 0..2 {
            let resp = self
                .http
                .get(&url)
                .headers(Self::auth_headers(&token.token)?)
                .timeout(self.timeout())
                .send()
                .await?;
            let status = resp.status();
            if status.as_u16() == 401 && attempt == 0 {
                token = self.login().await?;
                continue;
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                let truncated =
                    crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
                anyhow::bail!("8Sleep GET {path_after_base} failed ({status}): {truncated}");
            }
            return resp.json().await.map_err(Into::into);
        }
        unreachable!("loop exits via return or bail")
    }

    /// Issue a PUT that retries once with a fresh token on `401`.
    async fn put_authed(&self, path_after_base: &str, body: &Value) -> anyhow::Result<Value> {
        let url = format!("{}{}", self.api_base_url, path_after_base);
        let mut token = self.current_token().await?;
        for attempt in 0..2 {
            let resp = self
                .http
                .put(&url)
                .headers(Self::auth_headers(&token.token)?)
                .json(body)
                .timeout(self.timeout())
                .send()
                .await?;
            let status = resp.status();
            if status.as_u16() == 401 && attempt == 0 {
                token = self.login().await?;
                continue;
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                let truncated =
                    crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
                anyhow::bail!("8Sleep PUT {path_after_base} failed ({status}): {truncated}");
            }
            return resp.json().await.map_err(Into::into);
        }
        unreachable!("loop exits via return or bail")
    }

    /// Resolve the device id for the current account by hitting
    /// `/users/me`. The response shape varies between Pod generations;
    /// we look for `user.currentDevice.id` first and fall back to the
    /// first entry of `user.devices`.
    async fn resolve_device_id(&self) -> anyhow::Result<String> {
        let user = self.get_authed("/users/me").await?;
        if let Some(id) = user
            .get("user")
            .and_then(|u| u.get("currentDevice"))
            .and_then(|d| d.get("id"))
            .and_then(|v| v.as_str())
        {
            return Ok(id.to_string());
        }
        if let Some(id) = user
            .get("user")
            .and_then(|u| u.get("devices"))
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
        {
            return Ok(id.to_string());
        }
        anyhow::bail!(
            "8Sleep: could not resolve device id from /users/me response — account may have no Pod registered"
        );
    }

    async fn get_bed_state(&self) -> anyhow::Result<Value> {
        let device_id = self.resolve_device_id().await?;
        self.get_authed(&format!("/devices/{device_id}")).await
    }

    async fn get_metrics(&self) -> anyhow::Result<Value> {
        let token = self.current_token().await?;
        self.get_authed(&format!("/users/{}/intervals", token.user_id))
            .await
    }

    async fn set_temperature(&self, side: &str, level: i64) -> anyhow::Result<Value> {
        let device_id = self.resolve_device_id().await?;
        let body = Self::build_temperature_body(side, level);
        self.put_authed(&format!("/devices/{device_id}"), &body)
            .await
    }
}

#[async_trait]
impl Tool for EightSleepTool {
    fn name(&self) -> &str {
        "eight_sleep"
    }

    fn description(&self) -> &str {
        "Read and adjust an 8Sleep Pod via the unofficial cloud API. Read \
         current bed state (get_bed_state) or last-night metrics \
         (get_metrics). Mutate per-side heating level (-100 cools, 0 \
         holds, +100 warms) with set_temperature — restricted to \
         allowed_sides. Note: API is unofficial and may break without \
         notice; alarms and prime-cycle control are not supported in v1."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get_bed_state", "get_metrics", "set_temperature"],
                    "description": "The 8Sleep operation to perform."
                },
                "side": {
                    "type": "string",
                    "enum": ["left", "right"],
                    "description": "Bed side. Required for set_temperature."
                },
                "level": {
                    "type": "integer",
                    "minimum": HEATING_LEVEL_MIN,
                    "maximum": HEATING_LEVEL_MAX,
                    "description": "Heating level -100..100. Negative cools, 0 holds, positive warms. Required for set_temperature."
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
            "get_bed_state" | "get_metrics" => ToolOperation::Read,
            "set_temperature" => ToolOperation::Act,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action: {action}. Valid actions: get_bed_state, get_metrics, set_temperature"
                    )),
                });
            }
        };

        if let Err(error) = self
            .security
            .enforce_tool_operation(operation, "eight_sleep")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let result = match action {
            "get_bed_state" => self.get_bed_state().await,
            "get_metrics" => self.get_metrics().await,
            "set_temperature" => {
                let side = match args.get("side").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s.trim().to_lowercase(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "set_temperature requires side parameter (\"left\" or \"right\")"
                                    .into(),
                            ),
                        });
                    }
                };
                if side != "left" && side != "right" {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Invalid side '{side}'. Must be \"left\" or \"right\"."
                        )),
                    });
                }
                if !self.is_side_allowed(&side) {
                    let allowed = if self.allowed_sides.is_empty() {
                        "(none — allowed_sides is empty)".to_string()
                    } else {
                        self.allowed_sides.join(", ")
                    };
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Side '{side}' is not in allowed_sides. Allowed: {allowed}"
                        )),
                    });
                }
                let level = match args.get("level").and_then(|v| v.as_i64()) {
                    Some(l) => l,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(
                                "set_temperature requires level parameter (integer -100..100)"
                                    .into(),
                            ),
                        });
                    }
                };
                if !(HEATING_LEVEL_MIN..=HEATING_LEVEL_MAX).contains(&level) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "level {level} out of range. Must be {HEATING_LEVEL_MIN}..={HEATING_LEVEL_MAX}."
                        )),
                    });
                }
                self.set_temperature(&side, level).await
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

    fn test_tool() -> EightSleepTool {
        EightSleepTool::new(
            "user@example.invalid".into(),
            "secret".into(),
            "https://client-api.8slp.net/v1".into(),
            vec!["left".into(), "right".into()],
            15,
            Arc::new(SecurityPolicy::default()),
        )
    }

    #[test]
    fn name_is_eight_sleep() {
        assert_eq!(test_tool().name(), "eight_sleep");
    }

    #[test]
    fn description_is_non_empty_and_warns_unofficial() {
        let tool = test_tool();
        let d = tool.description();
        assert!(!d.is_empty());
        assert!(d.to_lowercase().contains("unofficial"));
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
        for expected in &["get_bed_state", "get_metrics", "set_temperature"] {
            assert!(names.contains(expected), "missing action: {expected}");
        }
    }

    #[test]
    fn parameters_schema_level_bounds() {
        let schema = test_tool().parameters_schema();
        let level = &schema["properties"]["level"];
        assert_eq!(level["minimum"], HEATING_LEVEL_MIN);
        assert_eq!(level["maximum"], HEATING_LEVEL_MAX);
    }

    #[test]
    fn api_base_url_trailing_slash_stripped() {
        let tool = EightSleepTool::new(
            "e".into(),
            "p".into(),
            "https://example.invalid/v1/".into(),
            vec!["left".into()],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        assert_eq!(tool.api_base_url, "https://example.invalid/v1");
    }

    #[test]
    fn allowed_sides_lowercased_trimmed_and_empty_dropped() {
        let tool = EightSleepTool::new(
            "e".into(),
            "p".into(),
            "https://h".into(),
            vec!["  Left ".into(), "".into(), "RIGHT".into()],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        assert!(tool.is_side_allowed("left"));
        assert!(tool.is_side_allowed("right"));
        assert!(!tool.is_side_allowed("Left")); // we lowercase incoming arg too
    }

    #[test]
    fn build_temperature_body_includes_per_side_fields() {
        let body = EightSleepTool::build_temperature_body("left", 42);
        assert_eq!(body["leftNow"], true);
        assert_eq!(body["leftTargetHeatingLevel"], 42);
        assert_eq!(body["leftHeatingLevel"], 42);
        // No leakage to the other side
        assert!(body.get("rightNow").is_none());
    }

    #[test]
    fn build_temperature_body_handles_negative_levels() {
        let body = EightSleepTool::build_temperature_body("right", -100);
        assert_eq!(body["rightTargetHeatingLevel"], -100);
        assert_eq!(body["rightHeatingLevel"], -100);
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
    async fn execute_set_temperature_missing_side_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "set_temperature", "level": 10}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("side"));
    }

    #[tokio::test]
    async fn execute_set_temperature_invalid_side_returns_error() {
        let result = test_tool()
            .execute(json!({
                "action": "set_temperature",
                "side": "middle",
                "level": 10
            }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("Invalid side") || err.contains("not in allowed_sides"));
    }

    #[tokio::test]
    async fn execute_set_temperature_missing_level_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "set_temperature", "side": "left"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("level"));
    }

    #[tokio::test]
    async fn execute_set_temperature_level_out_of_range_returns_error() {
        let result = test_tool()
            .execute(json!({
                "action": "set_temperature",
                "side": "left",
                "level": 200
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn execute_set_temperature_blocked_when_side_not_allowed() {
        let tool = EightSleepTool::new(
            "e".into(),
            "p".into(),
            "https://h".into(),
            vec!["left".into()], // right is NOT allowed
            15,
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({
                "action": "set_temperature",
                "side": "right",
                "level": 10
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not in allowed_sides"));
    }

    #[tokio::test]
    async fn execute_set_temperature_blocked_when_allowed_sides_empty() {
        let tool = EightSleepTool::new(
            "e".into(),
            "p".into(),
            "https://h".into(),
            vec![],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({
                "action": "set_temperature",
                "side": "left",
                "level": 10
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
        assert_eq!(spec.name, "eight_sleep");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters.is_object());
    }
}
