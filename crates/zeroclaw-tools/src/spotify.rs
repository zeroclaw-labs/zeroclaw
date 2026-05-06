//! Spotify tool — Web API client driven by a refresh-token OAuth flow.
//!
//! Auth model: the operator does the one-time OAuth dance externally
//! (see `docs/book/src/tools/spotify.md`) and pastes the resulting
//! `refresh_token` into config. At runtime the tool exchanges the
//! refresh token for a short-lived access token via
//! `POST https://accounts.spotify.com/api/token`, caches it in memory,
//! and refreshes on `401` or when within 60s of expiry.
//!
//! Read actions (`get_playback_state`, `list_devices`, `list_playlists`,
//! `search`) require `ToolOperation::Read`. Mutating actions (`play`,
//! `pause`, `next`, `previous`, `set_volume`) require `ToolOperation::Act`
//! AND must appear in the operator's `allowed_actions` allowlist.
//!
//! Playback control requires Spotify Premium; the tool returns the
//! upstream `403` verbatim when the account is Free.

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

const SPOTIFY_API_BASE: &str = "https://api.spotify.com/v1";
const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const MAX_ERROR_BODY_CHARS: usize = 500;
/// Refresh tokens proactively this many seconds before the upstream
/// `expires_in`. Keeps a small buffer so a request that takes a moment
/// to dispatch still uses a valid token.
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

/// Tool for interacting with the Spotify Web API.
pub struct SpotifyTool {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    allowed_actions: Vec<String>,
    request_timeout_secs: u64,
    token: Arc<Mutex<Option<AccessToken>>>,
    http: reqwest::Client,
    security: Arc<SecurityPolicy>,
}

impl SpotifyTool {
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
            .post(SPOTIFY_TOKEN_URL)
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
            anyhow::bail!("Spotify token refresh failed ({status}): {truncated}");
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
                .map_err(|e| anyhow::anyhow!("Invalid Spotify token header: {e}"))?,
        );
        headers.insert("Content-Type", "application/json".parse().unwrap());
        Ok(headers)
    }

    /// Issue a request that retries once with a fresh token on `401`.
    /// `body` is sent only when `Some`. Spotify mutations frequently
    /// return `204 No Content`; we surface an empty JSON object in that
    /// case so callers always get a `Value`.
    async fn request_authed(
        &self,
        method: reqwest::Method,
        path_and_query: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<Value> {
        let url = format!("{SPOTIFY_API_BASE}{path_and_query}");
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
                anyhow::bail!("Spotify {method} {path_and_query} failed ({status}): {truncated}");
            }
            // 204 No Content is common on mutations.
            if status.as_u16() == 204 {
                return Ok(json!({}));
            }
            return resp.json().await.map_err(Into::into);
        }
        unreachable!("loop exits via return or bail")
    }
}

#[async_trait]
impl Tool for SpotifyTool {
    fn name(&self) -> &str {
        "spotify"
    }

    fn description(&self) -> &str {
        "Drive a Spotify account via the Web API. Read playback state, \
         list devices and playlists, search the catalogue, and (when the \
         operator allows) control playback (play/pause/next/previous/\
         set_volume). Mutations require Spotify Premium and an action \
         in allowed_actions."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "get_playback_state",
                        "list_devices",
                        "list_playlists",
                        "search",
                        "play",
                        "pause",
                        "next",
                        "previous",
                        "set_volume"
                    ],
                    "description": "The Spotify operation to perform."
                },
                "query": {
                    "type": "string",
                    "description": "Search query. Required for search."
                },
                "search_type": {
                    "type": "string",
                    "enum": ["track", "album", "artist", "playlist", "show", "episode"],
                    "description": "Type of object to search for. Default: \"track\"."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Pagination limit (1-50). Default: 20."
                },
                "uris": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Spotify URIs (e.g. `spotify:track:...`) for play. Optional; without it, play resumes the current context."
                },
                "context_uri": {
                    "type": "string",
                    "description": "Spotify context URI (album/playlist/artist) for play. Optional."
                },
                "device_id": {
                    "type": "string",
                    "description": "Target device ID for playback mutations. Optional; defaults to the active device."
                },
                "volume_percent": {
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
            "get_playback_state" | "list_devices" | "list_playlists" | "search" => {
                ToolOperation::Read
            }
            "play" | "pause" | "next" | "previous" | "set_volume" => ToolOperation::Act,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action: {action}. Valid actions: get_playback_state, list_devices, list_playlists, search, play, pause, next, previous, set_volume"
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

        if let Err(error) = self.security.enforce_tool_operation(operation, "spotify") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let device_query = args
            .get("device_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|d| format!("?device_id={}", urlencoding(d)))
            .unwrap_or_default();

        let result = match action {
            "get_playback_state" => {
                self.request_authed(reqwest::Method::GET, "/me/player", None)
                    .await
            }
            "list_devices" => {
                self.request_authed(reqwest::Method::GET, "/me/player/devices", None)
                    .await
            }
            "list_playlists" => {
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 50);
                self.request_authed(
                    reqwest::Method::GET,
                    &format!("/me/playlists?limit={limit}"),
                    None,
                )
                .await
            }
            "search" => {
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) if !q.trim().is_empty() => q.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("search requires query parameter".into()),
                        });
                    }
                };
                let search_type = args
                    .get("search_type")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("track");
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 50);
                self.request_authed(
                    reqwest::Method::GET,
                    &format!(
                        "/search?q={}&type={}&limit={limit}",
                        urlencoding(&query),
                        urlencoding(search_type)
                    ),
                    None,
                )
                .await
            }
            "play" => {
                let mut body = json!({});
                if let Some(uris) = args.get("uris").and_then(|v| v.as_array())
                    && !uris.is_empty()
                {
                    body["uris"] = Value::Array(uris.clone());
                }
                if let Some(ctx) = args.get("context_uri").and_then(|v| v.as_str())
                    && !ctx.trim().is_empty()
                {
                    body["context_uri"] = Value::String(ctx.trim().to_string());
                }
                let body_opt = if body.as_object().is_none_or(|o| o.is_empty()) {
                    None
                } else {
                    Some(&body)
                };
                self.request_authed(
                    reqwest::Method::PUT,
                    &format!("/me/player/play{device_query}"),
                    body_opt,
                )
                .await
            }
            "pause" => {
                self.request_authed(
                    reqwest::Method::PUT,
                    &format!("/me/player/pause{device_query}"),
                    None,
                )
                .await
            }
            "next" => {
                self.request_authed(
                    reqwest::Method::POST,
                    &format!("/me/player/next{device_query}"),
                    None,
                )
                .await
            }
            "previous" => {
                self.request_authed(
                    reqwest::Method::POST,
                    &format!("/me/player/previous{device_query}"),
                    None,
                )
                .await
            }
            "set_volume" => {
                let volume = match args.get("volume_percent").and_then(|v| v.as_u64()) {
                    Some(v) => v,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("set_volume requires volume_percent (0-100)".into()),
                        });
                    }
                };
                if !(VOLUME_MIN..=VOLUME_MAX).contains(&volume) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "volume_percent {volume} out of range. Must be {VOLUME_MIN}-{VOLUME_MAX}."
                        )),
                    });
                }
                let device_suffix = if device_query.is_empty() {
                    String::new()
                } else {
                    // already starts with "?device_id=..."; append rather than prefix `?`
                    format!("&{}", &device_query[1..])
                };
                self.request_authed(
                    reqwest::Method::PUT,
                    &format!("/me/player/volume?volume_percent={volume}{device_suffix}"),
                    None,
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

/// Minimal application/x-www-form-urlencoded encoding for query-string
/// values. Avoids pulling in the `urlencoding` crate for one helper.
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

    fn read_only_tool() -> SpotifyTool {
        SpotifyTool::new(
            "client-id".into(),
            "client-secret".into(),
            "refresh-token".into(),
            vec![
                "get_playback_state".into(),
                "list_devices".into(),
                "list_playlists".into(),
                "search".into(),
            ],
            15,
            Arc::new(SecurityPolicy::default()),
        )
    }

    fn full_access_tool() -> SpotifyTool {
        SpotifyTool::new(
            "client-id".into(),
            "client-secret".into(),
            "refresh-token".into(),
            vec![
                "get_playback_state".into(),
                "list_devices".into(),
                "list_playlists".into(),
                "search".into(),
                "play".into(),
                "pause".into(),
                "next".into(),
                "previous".into(),
                "set_volume".into(),
            ],
            15,
            Arc::new(SecurityPolicy::default()),
        )
    }

    #[test]
    fn name_is_spotify() {
        assert_eq!(read_only_tool().name(), "spotify");
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
            "get_playback_state",
            "list_devices",
            "list_playlists",
            "search",
            "play",
            "pause",
            "next",
            "previous",
            "set_volume",
        ] {
            assert!(names.contains(expected), "missing action: {expected}");
        }
    }

    #[test]
    fn parameters_schema_volume_bounds() {
        let schema = read_only_tool().parameters_schema();
        let v = &schema["properties"]["volume_percent"];
        assert_eq!(v["minimum"], 0);
        assert_eq!(v["maximum"], 100);
    }

    #[test]
    fn allowed_actions_trimmed_and_empty_dropped() {
        let tool = SpotifyTool::new(
            "c".into(),
            "s".into(),
            "r".into(),
            vec!["  search ".into(), "".into(), "play".into()],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        assert!(tool.is_action_allowed("search"));
        assert!(tool.is_action_allowed("play"));
        assert!(!tool.is_action_allowed(""));
    }

    #[test]
    fn basic_auth_header_format_matches_oauth2() {
        let header = SpotifyTool::basic_auth_header("abc", "xyz");
        // base64("abc:xyz") = "YWJjOnh5eg=="
        assert_eq!(header, "Basic YWJjOnh5eg==");
    }

    #[test]
    fn urlencoding_handles_unsafe_chars() {
        assert_eq!(urlencoding("hello world"), "hello%20world");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
        // Unreserved characters pass through.
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
            .execute(json!({"action": "play"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not in allowed_actions"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_search_missing_query_returns_error() {
        let result = read_only_tool()
            .execute(json!({"action": "search"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("query"));
    }

    #[tokio::test]
    async fn execute_set_volume_missing_param_returns_error() {
        let result = full_access_tool()
            .execute(json!({"action": "set_volume"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("volume_percent"));
    }

    #[tokio::test]
    async fn execute_set_volume_out_of_range_returns_error() {
        let result = full_access_tool()
            .execute(json!({"action": "set_volume", "volume_percent": 150}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn execute_blocked_when_allowed_actions_empty() {
        let tool = SpotifyTool::new(
            "c".into(),
            "s".into(),
            "r".into(),
            vec![],
            15,
            Arc::new(SecurityPolicy::default()),
        );
        let result = tool
            .execute(json!({"action": "get_playback_state"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("none"));
    }

    #[test]
    fn spec_reflects_metadata() {
        let tool = read_only_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "spotify");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters.is_object());
    }
}
