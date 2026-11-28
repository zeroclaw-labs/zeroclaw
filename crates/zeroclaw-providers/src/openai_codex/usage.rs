//! Local, opt-in ChatGPT/Codex subscription usage lookup for a ZeroClaw-native
//! Codex OAuth account (`zeroclaw auth usage`).
//!
//! The endpoint is undocumented. It is the same `wham/usage` resource the
//! open-source Codex client reads, so the wire shape may change without notice;
//! the parser therefore only trusts the few fields it needs and ignores the
//! rest (credits, additional per-feature buckets, plan metadata, ...).
//!
//! Safety properties, all enforced here rather than by callers:
//! - the production URL is a constant; there is no URL, endpoint, or header override;
//! - custom-endpoint and non-native routes are rejected before auth or network access;
//! - credentials come from one read-only local profile snapshot: no refresh, no
//!   write, no lock file, and no fallback to another profile or credential;
//! - the request is a single GET with a bounded timeout and response body, no
//!   redirects and no retries;
//! - errors and `Debug`/`Display` output never carry tokens, account ids, headers,
//!   or response bodies.

use super::OpenAiCodexModelProvider;
use crate::auth::openai_oauth::extract_chatgpt_account_id_from_jwt;
use crate::auth::profiles::OAuthAccessSnapshot;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use zeroclaw_config::schema::Config;

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const USAGE_TIMEOUT: Duration = Duration::from_secs(15);
const USAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const USAGE_MAX_BODY_BYTES: usize = 256 * 1024;
const USAGE_PROXY_SERVICE_KEY: &str = "model_provider.openai";
const FIVE_HOUR_WINDOW_SECONDS: i64 = 5 * 60 * 60;
const WEEKLY_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;
const CODEX_AUTH_PROVIDER: &str = "openai-codex";

/// Usage of one rate-limit window.
///
/// `used_percent` is `None` when the server omitted it or sent `null`; that is
/// deliberately distinct from a real `0.0`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodexUsageWindow {
    pub used_percent: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
}

/// The five-hour and weekly windows, classified by `limit_window_seconds`
/// (never by primary/secondary slot). A window the server did not report is `None`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodexUsage {
    pub five_hour: Option<CodexUsageWindow>,
    pub weekly: Option<CodexUsageWindow>,
}

/// Why a usage lookup did not produce a result. `Display` text is stable
/// English and contains no credential, identity, header, or body data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexUsageError {
    UnsupportedRoute(&'static str),
    AuthStoreUnreadable,
    ProfileNotFound { explicit_override: bool },
    ProfileNotOAuth,
    AccessTokenMissing,
    TokenExpired,
    AccountIdMissing,
    AccountIdMismatch,
    ResponseAccountMismatch,
    ClientUnavailable,
    Transport,
    HttpStatus(u16),
    BodyTooLarge,
    MalformedResponse,
    AmbiguousWindows,
    NoRecognizedWindows,
}

impl std::fmt::Display for CodexUsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedRoute(reason) => {
                write!(f, "Codex usage is not supported for this route: {reason}")
            }
            Self::AuthStoreUnreadable => {
                f.write_str("the local auth profile store could not be read")
            }
            Self::ProfileNotFound {
                explicit_override: true,
            } => f.write_str("the requested OpenAI Codex auth profile was not found"),
            Self::ProfileNotFound {
                explicit_override: false,
            } => f.write_str(
                "no OpenAI Codex OAuth profile found; run `zeroclaw auth login --model-provider openai-codex`",
            ),
            Self::ProfileNotOAuth => f.write_str("the OpenAI Codex auth profile is not OAuth-based"),
            Self::AccessTokenMissing => f.write_str("the OpenAI Codex profile has no access token"),
            Self::TokenExpired => f.write_str(
                "the OpenAI Codex access token is expired; usage lookup does not refresh tokens, \
                 so run a ZeroClaw command that uses this provider or `zeroclaw auth refresh --model-provider openai-codex` first",
            ),
            Self::AccountIdMissing => {
                f.write_str("the OpenAI Codex auth profile has no account id")
            }
            Self::AccountIdMismatch => f.write_str(
                "the OpenAI Codex access token and the stored account id belong to different accounts",
            ),
            Self::ResponseAccountMismatch => f.write_str(
                "the usage response belongs to a different account than the requested one",
            ),
            Self::ClientUnavailable => f.write_str("the HTTP client could not be built"),
            Self::Transport => f.write_str("the usage request failed or timed out"),
            Self::HttpStatus(status) => write!(f, "the usage endpoint returned HTTP {status}"),
            Self::BodyTooLarge => f.write_str("the usage response exceeded the size limit"),
            Self::MalformedResponse => f.write_str("the usage response could not be parsed"),
            Self::AmbiguousWindows => {
                f.write_str("the usage response reported the same window more than once")
            }
            Self::NoRecognizedWindows => {
                f.write_str("the usage response contained no five-hour or weekly window")
            }
        }
    }
}

impl std::error::Error for CodexUsageError {}

// ── Wire shape (only the fields we trust; everything else is ignored) ──

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    rate_limit: Option<WireRateLimit>,
}

#[derive(Debug, Deserialize)]
struct WireRateLimit {
    #[serde(default)]
    primary_window: Option<WireWindow>,
    #[serde(default)]
    secondary_window: Option<WireWindow>,
}

#[derive(Debug, Deserialize)]
struct WireWindow {
    #[serde(default)]
    used_percent: Option<f64>,
    #[serde(default)]
    limit_window_seconds: Option<i64>,
    #[serde(default)]
    reset_at: Option<i64>,
}

/// Parse a `wham/usage` response body into the two windows we report.
///
/// When the response identifies an account (`account_id`) it must equal
/// `expected_account_id`; an absent or empty identifier is accepted. Neither id
/// ever appears in an error.
pub fn parse_codex_usage(
    body: &[u8],
    expected_account_id: Option<&str>,
) -> Result<CodexUsage, CodexUsageError> {
    let wire: WireUsage =
        serde_json::from_slice(body).map_err(|_| CodexUsageError::MalformedResponse)?;
    if let Some(reported) = wire
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        && expected_account_id.is_none_or(|expected| expected != reported)
    {
        return Err(CodexUsageError::ResponseAccountMismatch);
    }
    let rate_limit = wire
        .rate_limit
        .ok_or(CodexUsageError::NoRecognizedWindows)?;

    let mut five_hour = None;
    let mut weekly = None;
    for window in [rate_limit.primary_window, rate_limit.secondary_window]
        .into_iter()
        .flatten()
    {
        let slot = match window.limit_window_seconds {
            Some(FIVE_HOUR_WINDOW_SECONDS) => &mut five_hour,
            Some(WEEKLY_WINDOW_SECONDS) => &mut weekly,
            // Unclassifiable windows (other durations, or no duration) are ignored.
            _ => continue,
        };
        if slot.is_some() {
            return Err(CodexUsageError::AmbiguousWindows);
        }
        *slot = Some(convert_window(&window)?);
    }

    if five_hour.is_none() && weekly.is_none() {
        return Err(CodexUsageError::NoRecognizedWindows);
    }
    Ok(CodexUsage { five_hour, weekly })
}

fn convert_window(window: &WireWindow) -> Result<CodexUsageWindow, CodexUsageError> {
    if let Some(percent) = window.used_percent
        && (!percent.is_finite() || percent < 0.0)
    {
        return Err(CodexUsageError::MalformedResponse);
    }
    let resets_at = window
        .reset_at
        .map(|secs| DateTime::from_timestamp(secs, 0).ok_or(CodexUsageError::MalformedResponse))
        .transpose()?;
    Ok(CodexUsageWindow {
        used_percent: window.used_percent,
        resets_at,
    })
}

// ── Route selection ──

/// Resolve `openai.<alias>` from configuration into a provider usable for the
/// usage lookup, or reject it. Only a configured `openai` alias that opts into
/// Codex OAuth (`requires_openai_auth = true`) on the default Codex endpoint is
/// accepted. Nothing here reads auth state or touches the network.
pub fn codex_usage_provider_for_route(
    config: &Config,
    model_provider: &str,
) -> Result<OpenAiCodexModelProvider, CodexUsageError> {
    let (family, alias) = model_provider
        .trim()
        .split_once('.')
        .filter(|(family, alias)| !family.is_empty() && !alias.is_empty())
        .ok_or(CodexUsageError::UnsupportedRoute(
            "expected `openai.<alias>`",
        ))?;
    if family != "openai" {
        return Err(CodexUsageError::UnsupportedRoute(
            "only `openai.<alias>` routes are supported",
        ));
    }
    let entry =
        config
            .providers
            .models
            .find(family, alias)
            .ok_or(CodexUsageError::UnsupportedRoute(
                "the alias is not configured",
            ))?;
    if !entry.requires_openai_auth {
        return Err(CodexUsageError::UnsupportedRoute(
            "the alias does not use Codex OAuth (`requires_openai_auth`)",
        ));
    }
    if let Some(kind) = entry
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        && !matches!(kind, "openai" | "openai-codex" | "openai_codex" | "codex")
    {
        return Err(CodexUsageError::UnsupportedRoute(
            "the alias is not a native OpenAI Codex provider",
        ));
    }

    // Validate the endpoint override quietly (no provider construction, no
    // logging, no echo of the configured value) before building anything.
    if let Some(uri) = super::first_nonempty(entry.uri.as_deref()) {
        let normalized = super::normalize_responses_url(&uri).map_err(|_| {
            CodexUsageError::UnsupportedRoute("the alias has an invalid endpoint override")
        })?;
        if !super::is_default_responses_url(&normalized) {
            return Err(CodexUsageError::UnsupportedRoute(
                "custom endpoints are not supported",
            ));
        }
    }

    // Build with the override cleared: the endpoint was just proven to be the
    // default, so construction cannot fail or log on it.
    let mut options =
        crate::model_provider_runtime_options_from_model_provider_entry(config, Some(entry));
    options.provider_api_url = None;
    let provider = OpenAiCodexModelProvider::new(alias, &options, None)
        .map_err(|_| CodexUsageError::UnsupportedRoute("the provider could not be constructed"))?;
    Ok(provider)
}

// ── Provider method ──

struct UsageCredentials {
    access_token: String,
    account_id: String,
}

impl OpenAiCodexModelProvider {
    /// Fetch the five-hour and weekly usage of this provider's ZeroClaw-native
    /// Codex OAuth account. See the module docs for the safety contract.
    pub async fn fetch_usage(&self) -> Result<CodexUsage, CodexUsageError> {
        self.fetch_usage_from(USAGE_URL).await
    }

    /// `url` is a parameter only so tests can target a local mock; the sole
    /// production caller passes [`USAGE_URL`].
    async fn fetch_usage_from(&self, url: &str) -> Result<CodexUsage, CodexUsageError> {
        if self.custom_endpoint {
            return Err(CodexUsageError::UnsupportedRoute(
                "custom endpoints are not supported",
            ));
        }
        let credentials = self.read_usage_credentials().await?;
        let client = usage_client()?;

        let mut response = client
            .get(url)
            .bearer_auth(&credentials.access_token)
            .header("chatgpt-account-id", &credentials.account_id)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| CodexUsageError::Transport)?;

        let status = response.status();
        if !status.is_success() {
            return Err(CodexUsageError::HttpStatus(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > USAGE_MAX_BODY_BYTES as u64)
        {
            return Err(CodexUsageError::BodyTooLarge);
        }

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| CodexUsageError::Transport)?
        {
            if body.len().saturating_add(chunk.len()) > USAGE_MAX_BODY_BYTES {
                return Err(CodexUsageError::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        parse_codex_usage(&body, Some(&credentials.account_id))
    }

    /// One read-only snapshot of the selected OAuth profile. Never refreshes,
    /// writes, or falls back to another profile/credential.
    async fn read_usage_credentials(&self) -> Result<UsageCredentials, CodexUsageError> {
        let explicit_override = self.auth_profile_override.is_some();
        let profile = self
            .auth
            .get_oauth_profile_access_only(
                CODEX_AUTH_PROVIDER,
                self.auth_profile_override.as_deref(),
            )
            .await
            .map_err(|_| CodexUsageError::AuthStoreUnreadable)?
            .ok_or(CodexUsageError::ProfileNotFound { explicit_override })?;

        let (stored_account_id, tokens) = match profile {
            OAuthAccessSnapshot::Access { account_id, tokens } => (account_id, tokens),
            OAuthAccessSnapshot::NotOAuth => return Err(CodexUsageError::ProfileNotOAuth),
            OAuthAccessSnapshot::MissingAccessToken => {
                return Err(CodexUsageError::AccessTokenMissing);
            }
        };
        let access_token = tokens.access_token.trim();
        if tokens.is_expiring_within(Duration::ZERO) {
            return Err(CodexUsageError::TokenExpired);
        }

        let stored = stored_account_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let from_token = extract_chatgpt_account_id_from_jwt(access_token);
        let account_id = match (stored, from_token) {
            (Some(stored), Some(token)) if stored != token => {
                return Err(CodexUsageError::AccountIdMismatch);
            }
            (Some(stored), _) => stored.to_string(),
            (None, Some(token)) => token,
            (None, None) => return Err(CodexUsageError::AccountIdMissing),
        };

        Ok(UsageCredentials {
            access_token: access_token.to_string(),
            account_id,
        })
    }
}

/// Dedicated client: no redirects, no retries (reqwest retries protocol NACKs
/// by default), bounded connect/total timeouts, and the
/// runtime proxy policy for the OpenAI service key.
fn usage_client() -> Result<reqwest::Client, CodexUsageError> {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(USAGE_CONNECT_TIMEOUT)
        .timeout(USAGE_TIMEOUT);
    zeroclaw_config::schema::try_apply_runtime_proxy_to_builder(builder, USAGE_PROXY_SERVICE_KEY)
        .and_then(|builder| builder.build().map_err(anyhow::Error::from))
        .map_err(|_| CodexUsageError::ClientUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelProviderRuntimeOptions;
    use crate::auth::AuthService;
    use crate::auth::profiles::TokenSet;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use base64::Engine;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use zeroclaw_config::schema::{ModelProviderConfig, OpenAIModelProviderConfig};

    // Obviously fake, redacted fixtures.
    const FAKE_ACCOUNT: &str = "acct-redacted-1";
    const FAKE_OTHER_ACCOUNT: &str = "acct-redacted-2";
    const FAKE_REFRESH: &str = "refresh-redacted";

    fn fake_jwt(account_id: Option<&str>) -> String {
        let claims = match account_id {
            Some(id) => json!({"chatgpt_account_id": id}),
            None => json!({"iss": "redacted"}),
        };
        let enc = |value: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        };
        format!(
            "{}.{}.sig-redacted",
            enc(&json!({"alg": "none"})),
            enc(&claims)
        )
    }

    fn usage_body() -> Value {
        json!({
            "plan_type": "redacted",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 12,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 100,
                    "reset_at": 1_800_000_000
                },
                "secondary_window": {
                    "used_percent": 0,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 200,
                    "reset_at": 1_800_100_000
                }
            },
            "credits": {"has_credits": true, "balance": "99"},
            "additional_rate_limits": [{
                "limit_name": "other",
                "rate_limit": {"primary_window": {
                    "used_percent": 99,
                    "limit_window_seconds": 18000,
                    "reset_at": 1
                }}
            }]
        })
    }

    // ── parser ──

    #[test]
    fn parses_both_windows_by_duration_and_ignores_unrelated_fields() {
        let usage = parse_codex_usage(usage_body().to_string().as_bytes(), None).unwrap();
        let five = usage.five_hour.unwrap();
        let week = usage.weekly.unwrap();
        assert_eq!(five.used_percent, Some(12.0));
        assert_eq!(five.resets_at.unwrap().timestamp(), 1_800_000_000);
        // A real zero stays zero, not "missing".
        assert_eq!(week.used_percent, Some(0.0));
        assert_eq!(week.resets_at.unwrap().timestamp(), 1_800_100_000);
    }

    #[test]
    fn classifies_by_duration_not_slot() {
        let body = json!({"rate_limit": {
            "primary_window": {"used_percent": 7, "limit_window_seconds": 604800, "reset_at": 10},
            "secondary_window": {"used_percent": 3, "limit_window_seconds": 18000, "reset_at": 20}
        }});
        let usage = parse_codex_usage(body.to_string().as_bytes(), None).unwrap();
        assert_eq!(usage.five_hour.unwrap().used_percent, Some(3.0));
        assert_eq!(usage.weekly.unwrap().used_percent, Some(7.0));
    }

    #[test]
    fn allows_weekly_only_primary() {
        let body = json!({"rate_limit": {
            "primary_window": {"used_percent": 40, "limit_window_seconds": 604800, "reset_at": 10},
            "secondary_window": null
        }});
        let usage = parse_codex_usage(body.to_string().as_bytes(), None).unwrap();
        assert!(usage.five_hour.is_none());
        assert_eq!(usage.weekly.unwrap().used_percent, Some(40.0));
    }

    #[test]
    fn distinguishes_missing_and_null_from_zero() {
        let body = json!({"rate_limit": {
            "primary_window": {"used_percent": null, "limit_window_seconds": 18000},
            "secondary_window": {"limit_window_seconds": 604800, "reset_at": 10}
        }});
        let usage = parse_codex_usage(body.to_string().as_bytes(), None).unwrap();
        let five = usage.five_hour.unwrap();
        let week = usage.weekly.unwrap();
        assert_eq!(five.used_percent, None);
        assert_eq!(five.resets_at, None);
        assert_eq!(week.used_percent, None);
        assert!(week.resets_at.is_some());
    }

    #[test]
    fn ignores_unclassifiable_windows() {
        let body = json!({"rate_limit": {
            "primary_window": {"used_percent": 1, "limit_window_seconds": 3600},
            "secondary_window": {"used_percent": 2}
        }});
        assert_eq!(
            parse_codex_usage(body.to_string().as_bytes(), None),
            Err(CodexUsageError::NoRecognizedWindows)
        );
    }

    #[test]
    fn rejects_duplicate_windows_and_bad_values() {
        let dup = json!({"rate_limit": {
            "primary_window": {"used_percent": 1, "limit_window_seconds": 18000},
            "secondary_window": {"used_percent": 2, "limit_window_seconds": 18000}
        }});
        assert_eq!(
            parse_codex_usage(dup.to_string().as_bytes(), None),
            Err(CodexUsageError::AmbiguousWindows)
        );
        let negative = json!({"rate_limit": {
            "primary_window": {"used_percent": -1, "limit_window_seconds": 18000}
        }});
        assert_eq!(
            parse_codex_usage(negative.to_string().as_bytes(), None),
            Err(CodexUsageError::MalformedResponse)
        );
        assert_eq!(
            parse_codex_usage(b"not json", None),
            Err(CodexUsageError::MalformedResponse)
        );
        assert_eq!(
            parse_codex_usage(b"{}", None),
            Err(CodexUsageError::NoRecognizedWindows)
        );
        assert_eq!(
            parse_codex_usage(br#"{"rate_limit": null}"#, None),
            Err(CodexUsageError::NoRecognizedWindows)
        );
    }

    #[test]
    fn json_output_uses_null_for_missing_and_utc_for_resets() {
        let body = json!({"rate_limit": {
            "primary_window": {"limit_window_seconds": 604800, "reset_at": 1_800_000_000}
        }});
        let usage = parse_codex_usage(body.to_string().as_bytes(), None).unwrap();
        let rendered = serde_json::to_value(&usage).unwrap();
        assert_eq!(rendered["five_hour"], Value::Null);
        assert_eq!(rendered["weekly"]["used_percent"], Value::Null);
        assert!(
            rendered["weekly"]["resets_at"]
                .as_str()
                .unwrap()
                .ends_with('Z')
        );
    }

    // ── route selection ──

    fn config_with(alias: &str, base: ModelProviderConfig) -> Config {
        let mut config = Config::default();
        config
            .providers
            .models
            .openai
            .insert(alias.to_string(), OpenAIModelProviderConfig { base });
        config
    }

    fn codex_base() -> ModelProviderConfig {
        ModelProviderConfig {
            requires_openai_auth: true,
            ..Default::default()
        }
    }

    #[test]
    fn route_accepts_native_codex_alias() {
        let config = config_with("codex", codex_base());
        let provider = codex_usage_provider_for_route(&config, "openai.codex").unwrap();
        assert!(!provider.custom_endpoint);
        assert!(provider.auth_profile_override.is_none());
    }

    #[test]
    fn route_rejects_non_native_and_unknown_routes() {
        let config = config_with("codex", codex_base());
        let plain = config_with("plain", ModelProviderConfig::default());
        for (cfg, route) in [
            (&config, "openai"),
            (&config, "openai."),
            (&config, ".codex"),
            (&config, "openai.missing"),
            (&config, "anthropic.codex"),
            (&config, "openai-codex.codex"),
            (&plain, "openai.plain"),
        ] {
            assert!(
                matches!(
                    codex_usage_provider_for_route(cfg, route),
                    Err(CodexUsageError::UnsupportedRoute(_))
                ),
                "{route} must be rejected"
            );
        }

        let custom_kind = config_with(
            "k",
            ModelProviderConfig {
                kind: Some("compatible".to_string()),
                ..codex_base()
            },
        );
        assert!(codex_usage_provider_for_route(&custom_kind, "openai.k").is_err());
    }

    #[test]
    fn route_rejects_custom_endpoints_but_accepts_the_default_one() {
        let custom = config_with(
            "c",
            ModelProviderConfig {
                uri: Some("https://example.invalid/backend-api".to_string()),
                ..codex_base()
            },
        );
        assert!(codex_usage_provider_for_route(&custom, "openai.c").is_err());

        let invalid = config_with(
            "bad",
            ModelProviderConfig {
                uri: Some("not a url".to_string()),
                ..codex_base()
            },
        );
        assert!(codex_usage_provider_for_route(&invalid, "openai.bad").is_err());

        let default = config_with(
            "d",
            ModelProviderConfig {
                uri: Some(super::super::DEFAULT_CODEX_RESPONSES_URL.to_string()),
                ..codex_base()
            },
        );
        assert!(codex_usage_provider_for_route(&default, "openai.d").is_ok());
    }

    // ── wire binding and credentials against redacted local mocks ──

    #[derive(Default)]
    struct Seen {
        requests: Vec<(String, HeaderMap)>,
        redirect_target_hits: usize,
    }

    enum Reply {
        Json(Value),
        Status(StatusCode),
        Redirect,
        Raw(Vec<u8>),
    }

    struct Mock {
        base: String,
        seen: Arc<Mutex<Seen>>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn start_mock(reply: Reply) -> Mock {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let reply = Arc::new(reply);
        let app = Router::new()
            .route(
                "/wham/usage",
                get(
                    |State((seen, reply)): State<(Arc<Mutex<Seen>>, Arc<Reply>)>,
                     headers: HeaderMap| async move {
                        seen.lock()
                            .unwrap()
                            .requests
                            .push(("GET".to_string(), headers));
                        match reply.as_ref() {
                            Reply::Json(body) => Json(body.clone()).into_response(),
                            Reply::Status(status) => status.into_response(),
                            Reply::Redirect => {
                                (StatusCode::FOUND, [(header::LOCATION, "/redirect-target")])
                                    .into_response()
                            }
                            Reply::Raw(bytes) => (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "application/json")],
                                bytes.clone(),
                            )
                                .into_response(),
                        }
                    },
                ),
            )
            .route(
                "/redirect-target",
                get(
                    |State((seen, _)): State<(Arc<Mutex<Seen>>, Arc<Reply>)>| async move {
                        seen.lock().unwrap().redirect_target_hits += 1;
                        StatusCode::OK
                    },
                ),
            )
            .with_state((Arc::clone(&seen), reply));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Mock {
            base: format!("http://{addr}/wham/usage"),
            seen,
            server,
        }
    }

    async fn provider_with_profile(
        dir: &tempfile::TempDir,
        access_token: &str,
        account_id: Option<&str>,
        expires_in: chrono::Duration,
        profile_override: Option<&str>,
    ) -> OpenAiCodexModelProvider {
        let options = ModelProviderRuntimeOptions {
            zeroclaw_dir: Some(dir.path().to_path_buf()),
            secrets_encrypt: false,
            auth_profile_override: profile_override.map(ToString::to_string),
            ..ModelProviderRuntimeOptions::default()
        };
        AuthService::new(dir.path(), false)
            .store_openai_tokens(
                "default",
                TokenSet {
                    access_token: access_token.to_string(),
                    refresh_token: Some(FAKE_REFRESH.to_string()),
                    id_token: None,
                    expires_at: Some(Utc::now() + expires_in),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
                account_id.map(ToString::to_string),
                true,
            )
            .await
            .unwrap();
        OpenAiCodexModelProvider::new("test", &options, None).unwrap()
    }

    #[tokio::test]
    async fn sends_one_bearer_get_with_account_header_and_returns_parsed_usage() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        let token = fake_jwt(Some(FAKE_ACCOUNT));
        let provider = provider_with_profile(
            &dir,
            &token,
            Some(FAKE_ACCOUNT),
            chrono::Duration::hours(1),
            None,
        )
        .await;

        let usage = provider.fetch_usage_from(&mock.base).await.unwrap();
        assert_eq!(usage.five_hour.unwrap().used_percent, Some(12.0));

        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen.requests.len(), 1, "exactly one request, no retries");
        let headers = &seen.requests[0].1;
        assert_eq!(
            headers
                .get(header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("Bearer {token}")
        );
        assert_eq!(
            headers.get("chatgpt-account-id").unwrap().to_str().unwrap(),
            FAKE_ACCOUNT
        );
    }

    #[tokio::test]
    async fn reads_without_writing_or_refreshing_the_auth_store() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(Some(FAKE_ACCOUNT)),
            Some(FAKE_ACCOUNT),
            chrono::Duration::hours(1),
            None,
        )
        .await;
        let store_path = dir.path().join("auth-profiles.json");
        let before = std::fs::read(&store_path).unwrap();
        let entries_before = std::fs::read_dir(dir.path()).unwrap().count();

        provider.fetch_usage_from(&mock.base).await.unwrap();

        assert_eq!(std::fs::read(&store_path).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            entries_before
        );
    }

    #[tokio::test]
    async fn expired_token_fails_without_refresh_or_network() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(Some(FAKE_ACCOUNT)),
            Some(FAKE_ACCOUNT),
            chrono::Duration::seconds(-30),
            None,
        )
        .await;
        let before = std::fs::read(dir.path().join("auth-profiles.json")).unwrap();

        assert_eq!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::TokenExpired)
        );
        assert!(mock.seen.lock().unwrap().requests.is_empty());
        assert_eq!(
            std::fs::read(dir.path().join("auth-profiles.json")).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn account_mismatch_and_missing_account_fail_before_network() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;

        let dir = tempfile::tempdir().unwrap();
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(Some(FAKE_OTHER_ACCOUNT)),
            Some(FAKE_ACCOUNT),
            chrono::Duration::hours(1),
            None,
        )
        .await;
        assert_eq!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::AccountIdMismatch)
        );

        let dir = tempfile::tempdir().unwrap();
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(None),
            None,
            chrono::Duration::hours(1),
            None,
        )
        .await;
        assert_eq!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::AccountIdMissing)
        );
        assert!(mock.seen.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn account_id_from_token_alone_is_accepted() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(Some(FAKE_ACCOUNT)),
            None,
            chrono::Duration::hours(1),
            None,
        )
        .await;
        provider.fetch_usage_from(&mock.base).await.unwrap();
        let seen = mock.seen.lock().unwrap();
        assert_eq!(
            seen.requests[0]
                .1
                .get("chatgpt-account-id")
                .unwrap()
                .to_str()
                .unwrap(),
            FAKE_ACCOUNT
        );
    }

    #[tokio::test]
    async fn invalid_explicit_profile_override_does_not_fall_back_to_default() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        // A valid default profile exists; the explicit override names another.
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(Some(FAKE_ACCOUNT)),
            Some(FAKE_ACCOUNT),
            chrono::Duration::hours(1),
            Some("does-not-exist"),
        )
        .await;
        assert_eq!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::ProfileNotFound {
                explicit_override: true
            })
        );
        assert!(mock.seen.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn missing_profile_is_reported_without_network() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        let options = ModelProviderRuntimeOptions {
            zeroclaw_dir: Some(dir.path().to_path_buf()),
            secrets_encrypt: false,
            ..ModelProviderRuntimeOptions::default()
        };
        let provider = OpenAiCodexModelProvider::new("test", &options, None).unwrap();
        assert_eq!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::ProfileNotFound {
                explicit_override: false
            })
        );
        assert!(mock.seen.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn custom_endpoint_provider_is_rejected_before_auth_or_network() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        let options = ModelProviderRuntimeOptions {
            zeroclaw_dir: Some(dir.path().to_path_buf()),
            secrets_encrypt: false,
            provider_api_url: Some("https://example.invalid/backend-api".to_string()),
            ..ModelProviderRuntimeOptions::default()
        };
        // A malformed snapshot would produce AuthStoreUnreadable if consulted.
        std::fs::write(dir.path().join("auth-profiles.json"), "not json").unwrap();
        let provider = OpenAiCodexModelProvider::new("test", &options, Some("key")).unwrap();
        assert!(matches!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::UnsupportedRoute(_))
        ));
        assert!(mock.seen.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Redirect).await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(Some(FAKE_ACCOUNT)),
            Some(FAKE_ACCOUNT),
            chrono::Duration::hours(1),
            None,
        )
        .await;
        assert_eq!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::HttpStatus(302))
        );
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen.requests.len(), 1);
        assert_eq!(seen.redirect_target_hits, 0);
    }

    #[tokio::test]
    async fn failures_do_not_leak_credentials_or_bodies() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let dir = tempfile::tempdir().unwrap();
        let token = fake_jwt(Some(FAKE_ACCOUNT));
        let provider = provider_with_profile(
            &dir,
            &token,
            Some(FAKE_ACCOUNT),
            chrono::Duration::hours(1),
            None,
        )
        .await;

        let unauthorized = start_mock(Reply::Status(StatusCode::UNAUTHORIZED)).await;
        let err = provider
            .fetch_usage_from(&unauthorized.base)
            .await
            .unwrap_err();
        assert_eq!(err, CodexUsageError::HttpStatus(401));

        // A body that echoes secrets must not surface in the error.
        let echoing = start_mock(Reply::Raw(
            format!(r#"{{"rate_limit": "{token} {FAKE_ACCOUNT}"}}"#).into_bytes(),
        ))
        .await;
        let err = provider.fetch_usage_from(&echoing.base).await.unwrap_err();
        assert_eq!(err, CodexUsageError::MalformedResponse);

        for err in [
            CodexUsageError::HttpStatus(401),
            CodexUsageError::MalformedResponse,
            CodexUsageError::Transport,
            CodexUsageError::AccountIdMismatch,
        ] {
            let rendered = format!("{err} {err:?}");
            assert!(!rendered.contains(&token));
            assert!(!rendered.contains(FAKE_ACCOUNT));
            assert!(!rendered.contains(FAKE_REFRESH));
        }
    }

    #[tokio::test]
    async fn oversized_response_is_rejected() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Raw(vec![b' '; USAGE_MAX_BODY_BYTES + 1])).await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(Some(FAKE_ACCOUNT)),
            Some(FAKE_ACCOUNT),
            chrono::Duration::hours(1),
            None,
        )
        .await;
        assert_eq!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::BodyTooLarge)
        );
    }

    // ── response account binding ──

    #[test]
    fn response_account_must_match_when_present() {
        let mut body = usage_body();
        // Absent: accepted.
        assert!(parse_codex_usage(body.to_string().as_bytes(), Some(FAKE_ACCOUNT)).is_ok());
        // Matching: accepted.
        body["account_id"] = json!(FAKE_ACCOUNT);
        assert!(parse_codex_usage(body.to_string().as_bytes(), Some(FAKE_ACCOUNT)).is_ok());
        // Null / empty: treated as absent.
        body["account_id"] = Value::Null;
        assert!(parse_codex_usage(body.to_string().as_bytes(), Some(FAKE_ACCOUNT)).is_ok());
        body["account_id"] = json!("  ");
        assert!(parse_codex_usage(body.to_string().as_bytes(), Some(FAKE_ACCOUNT)).is_ok());
        // Conflicting: rejected, and neither id leaks into the error.
        body["account_id"] = json!(FAKE_OTHER_ACCOUNT);
        let err = parse_codex_usage(body.to_string().as_bytes(), Some(FAKE_ACCOUNT)).unwrap_err();
        assert_eq!(err, CodexUsageError::ResponseAccountMismatch);
        let rendered = format!("{err} {err:?}");
        assert!(!rendered.contains(FAKE_ACCOUNT) && !rendered.contains(FAKE_OTHER_ACCOUNT));
        // An identified response with nothing to compare against is rejected too.
        assert_eq!(
            parse_codex_usage(body.to_string().as_bytes(), None),
            Err(CodexUsageError::ResponseAccountMismatch)
        );
    }

    #[tokio::test]
    async fn conflicting_response_account_returns_no_usage() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mut body = usage_body();
        body["account_id"] = json!(FAKE_OTHER_ACCOUNT);
        let mock = start_mock(Reply::Json(body)).await;
        let dir = tempfile::tempdir().unwrap();
        let provider = provider_with_profile(
            &dir,
            &fake_jwt(Some(FAKE_ACCOUNT)),
            Some(FAKE_ACCOUNT),
            chrono::Duration::hours(1),
            None,
        )
        .await;
        assert_eq!(
            provider.fetch_usage_from(&mock.base).await,
            Err(CodexUsageError::ResponseAccountMismatch)
        );
    }

    // ── selected-credential-only reads ──

    fn provider_in(
        dir: &tempfile::TempDir,
        profile_override: Option<&str>,
    ) -> OpenAiCodexModelProvider {
        let options = ModelProviderRuntimeOptions {
            zeroclaw_dir: Some(dir.path().to_path_buf()),
            secrets_encrypt: false,
            auth_profile_override: profile_override.map(ToString::to_string),
            ..ModelProviderRuntimeOptions::default()
        };
        OpenAiCodexModelProvider::new("test", &options, None).unwrap()
    }

    async fn store_profile(dir: &tempfile::TempDir, name: &str, account: &str, active: bool) {
        AuthService::new(dir.path(), false)
            .store_openai_tokens(
                name,
                TokenSet {
                    access_token: fake_jwt(Some(account)),
                    refresh_token: Some(FAKE_REFRESH.to_string()),
                    id_token: None,
                    expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                    token_type: Some("Bearer".to_string()),
                    scope: None,
                },
                Some(account.to_string()),
                active,
            )
            .await
            .unwrap();
    }

    fn edit_store(dir: &tempfile::TempDir, edit: impl FnOnce(&mut Value)) {
        let path = dir.path().join("auth-profiles.json");
        let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edit(&mut doc);
        std::fs::write(&path, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    }

    fn sent_account(mock: &Mock) -> String {
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen.requests.len(), 1);
        seen.requests[0]
            .1
            .get("chatgpt-account-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn selection_follows_override_then_active_then_default() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let dir = tempfile::tempdir().unwrap();
        store_profile(&dir, "default", FAKE_ACCOUNT, true).await;
        store_profile(&dir, "work", FAKE_OTHER_ACCOUNT, false).await;

        // Active profile (no override).
        let mock = start_mock(Reply::Json(usage_body())).await;
        provider_in(&dir, None)
            .fetch_usage_from(&mock.base)
            .await
            .unwrap();
        assert_eq!(sent_account(&mock), FAKE_ACCOUNT);

        // Short-name and full-id overrides pick the other account.
        for requested in ["work", "openai-codex:work"] {
            let mock = start_mock(Reply::Json(usage_body())).await;
            provider_in(&dir, Some(requested))
                .fetch_usage_from(&mock.base)
                .await
                .unwrap();
            assert_eq!(sent_account(&mock), FAKE_OTHER_ACCOUNT);
        }

        // No active entry: falls back to the `default` profile.
        edit_store(&dir, |doc| doc["active_profiles"] = json!({}));
        let mock = start_mock(Reply::Json(usage_body())).await;
        provider_in(&dir, None)
            .fetch_usage_from(&mock.base)
            .await
            .unwrap();
        assert_eq!(sent_account(&mock), FAKE_ACCOUNT);
    }

    #[tokio::test]
    async fn full_non_codex_profile_override_is_rejected_before_network() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        store_profile(&dir, "default", FAKE_ACCOUNT, true).await;
        edit_store(&dir, |doc| {
            let mut other = doc["profiles"]["openai-codex:default"].clone();
            other["model_provider"] = json!("gemini");
            other["profile_name"] = json!("default");
            doc["profiles"]["gemini:default"] = other;
        });
        assert_eq!(
            provider_in(&dir, Some("gemini:default"))
                .fetch_usage_from(&mock.base)
                .await,
            Err(CodexUsageError::ProfileNotFound {
                explicit_override: true
            })
        );
        assert!(mock.seen.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn unrelated_corrupt_or_legacy_secrets_are_neither_decoded_nor_rewritten() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        store_profile(&dir, "default", FAKE_ACCOUNT, true).await;
        edit_store(&dir, |doc| {
            // Selected profile: corrupt refresh/ID secrets must be ignored.
            let selected = &mut doc["profiles"]["openai-codex:default"];
            selected["refresh_token"] = json!("enc2:not-valid-hex");
            selected["id_token"] = json!("enc:zz");
            // Unrelated profiles: corrupt secret, unreadable 1Password ref,
            // legacy cipher, and an unparsable kind.
            let template = selected.clone();
            for (id, token) in [
                ("openai-codex:corrupt", "enc2:not-valid-hex"),
                ("openai-codex:legacy", "enc:abcd"),
                ("openai-codex:opref", "op://redacted-vault/item/field"),
            ] {
                let mut other = template.clone();
                other["profile_name"] = json!(id.rsplit(':').next().unwrap());
                other["access_token"] = json!(token);
                doc["profiles"][id] = other;
            }
            let mut bad_kind = template.clone();
            bad_kind["kind"] = json!("mystery");
            doc["profiles"]["other:weird"] = bad_kind;
        });
        let store_path = dir.path().join("auth-profiles.json");
        let before = std::fs::read(&store_path).unwrap();
        let entries_before = std::fs::read_dir(dir.path()).unwrap().count();

        provider_in(&dir, None)
            .fetch_usage_from(&mock.base)
            .await
            .unwrap();

        assert_eq!(sent_account(&mock), FAKE_ACCOUNT);
        assert_eq!(std::fs::read(&store_path).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            entries_before
        );
    }

    #[tokio::test]
    async fn corrupt_selected_access_secret_fails_without_leaking_it() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        store_profile(&dir, "default", FAKE_ACCOUNT, true).await;
        edit_store(&dir, |doc| {
            doc["profiles"]["openai-codex:default"]["access_token"] =
                json!("enc2:marker-redacted-corrupt");
        });
        let err = provider_in(&dir, None)
            .fetch_usage_from(&mock.base)
            .await
            .unwrap_err();
        assert_eq!(err, CodexUsageError::AuthStoreUnreadable);
        assert!(!format!("{err} {err:?}").contains("marker-redacted"));
        assert!(mock.seen.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn invalid_selected_profile_is_not_confused_with_missing_or_replaced() {
        let _proxy = crate::RuntimeProxyTestGuard::acquire().await;
        let mock = start_mock(Reply::Json(usage_body())).await;
        let dir = tempfile::tempdir().unwrap();
        store_profile(&dir, "default", FAKE_ACCOUNT, false).await;
        store_profile(&dir, "work", FAKE_OTHER_ACCOUNT, true).await;

        for (kind, token, expected) in [
            (
                "token",
                json!("enc2:not-valid-hex"),
                CodexUsageError::ProfileNotOAuth,
            ),
            (
                "invalid-kind",
                json!(FAKE_ACCOUNT),
                CodexUsageError::AuthStoreUnreadable,
            ),
            ("oauth", Value::Null, CodexUsageError::AccessTokenMissing),
            ("oauth", json!(""), CodexUsageError::AccessTokenMissing),
            ("oauth", json!(" \t"), CodexUsageError::AccessTokenMissing),
        ] {
            edit_store(&dir, |doc| {
                let selected = &mut doc["profiles"]["openai-codex:work"];
                selected["kind"] = json!(kind);
                selected["access_token"] = token;
            });
            let path = dir.path().join("auth-profiles.json");
            let before = std::fs::read(&path).unwrap();
            assert_eq!(
                provider_in(&dir, None).fetch_usage_from(&mock.base).await,
                Err(expected)
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        assert!(mock.seen.lock().unwrap().requests.is_empty());
    }

    // ── endpoint validation ──

    #[test]
    fn rejected_endpoints_are_not_echoed() {
        const MARKER: &str = "sensitive-marker-redacted";
        for uri in [
            format!("not a url?token={MARKER}"),
            format!("ftp://example.invalid/?token={MARKER}"),
            format!("https://example.invalid/base?token={MARKER}"),
        ] {
            let config = config_with(
                "x",
                ModelProviderConfig {
                    uri: Some(uri),
                    ..codex_base()
                },
            );
            let err = codex_usage_provider_for_route(&config, "openai.x")
                .err()
                .unwrap();
            assert!(matches!(err, CodexUsageError::UnsupportedRoute(_)));
            assert!(!format!("{err} {err:?}").contains(MARKER));
        }
    }

    #[test]
    fn default_endpoint_spellings_are_accepted_and_blank_means_default() {
        for uri in [
            "https://chatgpt.com/backend-api/codex",
            "https://chatgpt.com/backend-api/codex/responses/",
            "  ",
        ] {
            let config = config_with(
                "d",
                ModelProviderConfig {
                    uri: Some(uri.to_string()),
                    ..codex_base()
                },
            );
            assert!(
                codex_usage_provider_for_route(&config, "openai.d").is_ok(),
                "{uri}"
            );
        }
    }

    #[test]
    fn production_url_is_the_fixed_wham_usage_endpoint() {
        assert_eq!(USAGE_URL, "https://chatgpt.com/backend-api/wham/usage");
    }
}
