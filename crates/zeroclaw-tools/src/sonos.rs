//! Sonos tool — official Control API client driven by a refresh-token OAuth flow.
//!
//! Auth model (mirrors the `spotify` tool): the operator does the
//! one-time OAuth dance externally (see `docs/book/src/tools/sonos.md`)
//! and pastes the resulting `refresh_token` into config. At runtime the
//! tool exchanges the refresh token for a short-lived access token via
//! `POST https://api.sonos.com/login/v3/oauth/access`, caches it in
//! memory, and refreshes on `401` or when within 60s of expiry.
//!
//! Read actions (`list_households`, `list_groups`, `get_playback_status`,
//! `list_favorites`) require `ToolOperation::Read`. Mutating actions
//! (`play`, `pause`, `set_volume`, `play_favorite`) require
//! `ToolOperation::Act` AND must appear in `allowed_actions`.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

const SONOS_API_BASE: &str = "https://api.ws.sonos.com/control/api/v1";
const SONOS_TOKEN_URL: &str = "https://api.sonos.com/login/v3/oauth/access";
const MAX_ERROR_BODY_CHARS: usize = 500;
const TOKEN_REFRESH_LEAD_SECS: u64 = 60;
const VOLUME_MIN: u64 = 0;
const VOLUME_MAX: u64 = 100;

#[derive(Debug, Clone)]
struct AccessToken {
    token: String,
    expires_at: Instant,
}

impl AccessToken {
    fn is_fresh(&self) -> bool {
        self.expires_at
            .checked_duration_since(Instant::now())
            .is_some_and(|d| d.as_secs() > TOKEN_REFRESH_LEAD_SECS)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// Tool for interacting with the Sonos Control API.
pub struct SonosTool {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    allowed_actions: Vec<String>,
    request_timeout_secs: u64,
    token: Arc<Mutex<Option<AccessToken>>>,
    http: reqwest::Client,
    security: Arc<SecurityPolicy>,
}

impl SonosTool {
    pub fn new(
        client_id: String,
        client_secret: String,
        refresh_token: String,
        allowed_actions: Vec<String>,
        request_timeout_secs: u64,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        let allowed_actions = allowed_actions
            .into_iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();
        Self {
            client_id,
            client_secret,
            refresh_token,
            allowed_actions,
            request_timeout_secs,
            token: Arc::new(Mutex::new(None)),
            http: reqwest::Client::new(),
            security,
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs.max(1))
    }

    fn is_action_allowed(&self, action: &str) -> bool {
        self.allowed_actions.iter().any(|a| a == action)
    }

    /// Build the basic-auth header value used during refresh-token exchange.
    /// Public for unit testing — exercised independently of the network.
    fn basic_auth_header(client_id: &str, client_secret: &str) -> String {
        let creds = format!("{client_id}:{client_secret}");
        format!("Basic {}", B64.encode(creds))
    }

    /// Exchange the refresh token for a fresh access token. Replaces any
    /// prior cached token regardless of freshness.
    async fn refresh_access_token(&self) -> anyhow::Result<AccessToken> {
        let resp = self
            .http
            .post(SONOS_TOKEN_URL)
            .header(
                "Authorization",
                Self::basic_auth_header(&self.client_id, &self.client_secret),
            )
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.refresh_token.as_str()),
            ])
            .timeout(self.timeout())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Sonos token refresh failed ({status}): {truncated}");
        }
        let parsed: TokenResponse = resp.json().await?;
        let token = AccessToken {
            token: parsed.access_token,
            expires_at: Instant::now() + Duration::from_secs(parsed.expires_in),
        };
        *self.token.lock().await = Some(token.clone());
        Ok(token)
    }

    async fn current_token(&self) -> anyhow::Result<AccessToken> {
        if let Some(t) = self.token.lock().await.clone()
            && t.is_fresh()
        {
            return Ok(t);
        }
        self.refresh_access_token().await
    }

    fn auth_headers(token: &str) -> anyhow::Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid Sonos token header: {e}"))?,
        );
        headers.insert("Content-Type", "application/json".parse().unwrap());
        Ok(headers)
    }

    /// Issue a request that retries once with a fresh token on `401`.
    async fn request_authed(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<Value> {
        let url = format!("{SONOS_API_BASE}{path}");
        let mut token = self.current_token().await?;
        for attempt in 0..2 {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .headers(Self::auth_headers(&token.token)?)
                .timeout(self.timeout());
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            let status = resp.status();
            if status.as_u16() == 401 && attempt == 0 {
                token = self.refresh_access_token().await?;
                continue;
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                let truncated =
                    crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
                anyhow::bail!("Sonos {method} {path} failed ({status}): {truncated}");
            }
            // Sonos mutations sometimes return 200 with an empty body.
            let text = resp.text().await.unwrap_or_default();
            if text.trim().is_empty() {
                return Ok(json!({}));
            }
            return serde_json::from_str(&text).map_err(Into::into);
        }
        unreachable!("loop exits via return or bail")
    }
}

#[async_trait]
impl Tool for SonosTool {
    fn name(&self) -> &str {
        "sonos"
    }

    fn description(&self) -> &str {
        "Drive a Sonos household via the official Control API. Read \
         households, groups, playback status, and favorites; with \
         operator opt-in via allowed_actions, drive playback \
         (play/pause/set_volume/play_favorite). Refresh-token OAuth — \
         the operator does the one-time auth dance externally."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "list_households",
                        "list_groups",
                        "get_playback_status",
                        "list_favorites",
                        "play",
                        "pause",
                        "set_volume",
                        "play_favorite"
                    ],
                    "description": "The Sonos operation to perform."
                },
                "household_id": {
                    "type": "string",
                    "description": "Sonos household ID. Required for list_groups, list_favorites."
                },
                "group_id": {
                    "type": "string",
                    "description": "Sonos group ID. Required for get_playback_status, play, pause, set_volume, play_favorite."
                },
                "favorite_id": {
                    "type": "string",
                    "description": "Sonos favorite ID. Required for play_favorite."
                },
                "volume": {
                    "type": "integer",
                    "minimum": VOLUME_MIN,
                    "maximum": VOLUME_MAX,
                    "description": "Volume 0-100. Required for set_volume."
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
            "list_households" | "list_groups" | "get_playback_status" | "list_favorites" => {
                ToolOperation::Read
            }
            "play" | "pause" | "set_volume" | "play_favorite" => ToolOperation::Act,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action: {action}. Valid actions: list_households, list_groups, get_playback_status, list_favorites, play, pause, set_volume, play_favorite"
                    )),
                });
            }
        };

        if !self.is_action_allowed(action) {
            let allowed = if self.allowed_actions.is_empty() {
                "(none — allowed_actions is empty)".to_string()
            } else {
                self.allowed_actions.join(", ")
            };
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Action '{action}' is not in allowed_actions. Allowed: {allowed}"
                )),
            });
        }

        if let Err(error) = self.security.enforce_tool_operation(operation, "sonos") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let result = match action {
            "list_households" => {
                self.request_authed(reqwest::Method::GET, "/households", None)
                    .await
            }
            "list_groups" => {
                let hh = match args.get("household_id").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("list_groups requires household_id parameter".into()),
                        });
                    }
                };
                self.request_authed(
                    reqwest::Method::GET,
                    &format!("/households/{}/groups", urlencoding(&hh)),
                    None,
                )
                .await
            }
            "list_favorites" => {
                let hh = match args.get("household_id").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("list_favorites requires household_id parameter".into()),
                        });
                    }
                };
                self.request_authed(
                    reqwest::Method::GET,
                    &format!("/households/{}/favorites", urlencoding(&hh)),
                    None,
                )
                .await
            }
            "get_playback_status" => {
                let group = match require_group_id(&args, "get_playback_status") {
                    Ok(g) => g,
                    Err(tr) => return Ok(tr),
                };
                self.request_authed(
                    reqwest::Method::GET,
                    &format!("/groups/{}/playback", urlencoding(&group)),
                    None,
                )
                .await
            }
            "play" => {
                let group = match require_group_id(&args, "play") {
                    Ok(g) => g,
                    Err(tr) => return Ok(tr),
                };
                self.request_authed(
                    reqwest::Method::POST,
                    &format!("/groups/{}/playback/play", urlencoding(&group)),
                    None,
                )
                .await
            }
            "pause" => {
                let group = match require_group_id(&args, "pause") {
                    Ok(g) => g,
                    Err(tr) => return Ok(tr),
                };
                self.request_authed(
                    reqwest::Method::POST,
                    &format!("/groups/{}/playback/pause", urlencoding(&group)),
                    None,
                )
                .await
            }
            "set_volume" => {
                let group = match require_group_id(&args, "set_volume") {
                    Ok(g) => g,
                    Err(tr) => return Ok(tr),
                };
                let volume = match args.get("volume").and_then(|v| v.as_u64()) {
                    Some(v) => v,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("set_volume requires volume parameter (0-100)".into()),
                        });
                    }
                };
                if !(VOLUME_MIN..=VOLUME_MAX).contains(&volume) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "volume {volume} out of range. Must be {VOLUME_MIN}-{VOLUME_MAX}."
                        )),
                    });
                }
                let body = json!({ "volume": volume });
                self.request_authed(
                    reqwest::Method::POST,
                    &format!("/groups/{}/groupVolume", urlencoding(&group)),
                    Some(&body),
                )
                .await
            }
            "play_favorite" => {
                let group = match require_group_id(&args, "play_favorite") {
                    Ok(g) => g,
                    Err(tr) => return Ok(tr),
                };
                let favorite = match args.get("favorite_id").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("play_favorite requires favorite_id parameter".into()),
                        });
                    }
                };
                let body = json!({ "favoriteId": favorite });
                self.request_authed(
                    reqwest::Method::POST,
                    &format!("/groups/{}/favorites", urlencoding(&group)),
                    Some(&body),
                )
                .await
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

/// Extract `group_id` from args. On missing/empty, returns a fully-formed
/// `ToolResult` so the caller can `return Ok(tr)` and short-circuit.
fn require_group_id(args: &Value, action: &str) -> Result<String, ToolResult> {
    match args.get("group_id").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("{action} requires group_id parameter")),
        }),
    }
}

/// Minimal application/x-www-form-urlencoded encoding for path segments.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.bytes() {
        match ch {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(ch as char);
            }
            _ => {
                out.push_str(&format!("%{ch:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::policy::SecurityPolicy;

    fn read_only_tool() -> SonosTool {
        SonosTool::new(
            "client-id".into(),
            "client-secret".into(),
            "refresh-token".into(),
            vec![
                "list_households".into(),
                "list_groups".into(),
                "get_playback_status".into(),
                "list_favorites".into(),
            ],
            15,
            Arc::new(SecurityPolicy::default()),
        )
    }

    fn full_access_tool() -> SonosTool {
        SonosTool::new(
            "client-id".into(),
            "client-secret".into(),
            "refresh-token".into(),
            vec![
                "list_households".into(),
                "list_groups".into(),
                "get_playback_status".into(),
                "list_favorites".into(),
                "play".into(),
                "pause".into(),
                "set_volume".into(),
                "play_favorite".into(),
            ],
            15,
            Arc::new(SecurityPolicy::default()),
        )
    }

    #[test]
    fn name_is_sonos() {
        assert_eq!(read_only_tool().name(), "sonos");
    }

    #[test]
    fn description_is_non_empty() {
        let tool = read_only_tool();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn parameters_schema_requires_action() {
        let schema = read_only_tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[test]
    fn parameters_schema_lists_all_actions() {
        let schema = read_only_tool().parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let names: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        for expected in &[
            "list_households",
            "list_groups",
            "get_playback_status",
            "list_favorites",
            "play",
            "pause",
            "set_volume",
            "play_favorite",
        ] {
            assert!(names.contains(expected), "missing action: {expected}");
        }
    }

    #[test]
    fn parameters_schema_volume_bounds() {
        let schema = read_only_tool().parameters_schema();
        let v = &schema["properties"]["volume"];
        assert_eq!(v["minimum"], 0);
        assert_eq!(v["maximum"], 100);
    }

    #[test]
    fn allowed_actions_trimmed_and_empty_dropped() {
        let tool = SonosTool::new(
            "c".into(),
            "s".into(),
            "r".into(),
            vec!["  list_groups ".into(), "".into(), "play".into()],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        assert!(tool.is_action_allowed("list_groups"));
        assert!(tool.is_action_allowed("play"));
        assert!(!tool.is_action_allowed(""));
    }

    #[test]
    fn basic_auth_header_format_matches_oauth2() {
        let header = SonosTool::basic_auth_header("abc", "xyz");
        // base64("abc:xyz") = "YWJjOnh5eg=="
        assert_eq!(header, "Basic YWJjOnh5eg==");
    }

    #[test]
    fn urlencoding_handles_unsafe_chars() {
        assert_eq!(urlencoding("a&b/c"), "a%26b%2Fc");
        assert_eq!(urlencoding("ABCabc019-_.~"), "ABCabc019-_.~");
    }

    #[test]
    fn access_token_freshness_check() {
        let stale = AccessToken {
            token: "x".into(),
            expires_at: Instant::now(),
        };
        assert!(!stale.is_fresh());
        let fresh = AccessToken {
            token: "x".into(),
            expires_at: Instant::now() + Duration::from_secs(3600),
        };
        assert!(fresh.is_fresh());
    }

    #[tokio::test]
    async fn execute_missing_action_returns_error() {
        let result = read_only_tool().execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("action"));
    }

    #[tokio::test]
    async fn execute_unknown_action_returns_error() {
        let result = read_only_tool()
            .execute(json!({"action": "nope"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn execute_play_blocked_when_not_in_allowlist() {
        let result = read_only_tool()
            .execute(json!({"action": "play", "group_id": "g"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not in allowed_actions"));
    }

    #[tokio::test]
    async fn execute_list_groups_missing_household_returns_error() {
        let result = read_only_tool()
            .execute(json!({"action": "list_groups"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("household_id"));
    }

    #[tokio::test]
    async fn execute_get_playback_status_missing_group_returns_error() {
        let result = read_only_tool()
            .execute(json!({"action": "get_playback_status"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("group_id"));
    }

    #[tokio::test]
    async fn execute_set_volume_missing_volume_returns_error() {
        let result = full_access_tool()
            .execute(json!({"action": "set_volume", "group_id": "g"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("volume"));
    }

    #[tokio::test]
    async fn execute_set_volume_out_of_range_returns_error() {
        let result = full_access_tool()
            .execute(json!({"action": "set_volume", "group_id": "g", "volume": 200}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn execute_play_favorite_missing_favorite_returns_error() {
        let result = full_access_tool()
            .execute(json!({"action": "play_favorite", "group_id": "g"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("favorite_id"));
    }

    #[tokio::test]
    async fn execute_blocked_when_allowed_actions_empty() {
        let tool = SonosTool::new(
            "c".into(),
            "s".into(),
            "r".into(),
            vec![],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({"action": "list_households"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("none"));
    }

    #[test]
    fn spec_reflects_metadata() {
        let tool = read_only_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "sonos");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters.is_object());
    }
}
