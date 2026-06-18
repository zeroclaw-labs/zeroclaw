use crate::auth::oauth_common::{parse_query_params, url_encode};
use crate::auth::profiles::TokenSet;
use anyhow::{Context, Result};
use base64::Engine;
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub use crate::auth::oauth_common::{PkceState, generate_pkce_state};

pub const XAI_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const XAI_OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
pub const XAI_OAUTH_ISSUER: &str = "https://auth.x.ai";
pub const XAI_OAUTH_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
pub const XAI_OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:56121/callback";
const XAI_DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, Clone)]
pub struct OAuthDiscovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
}

#[derive(Debug, Clone)]
pub struct DeviceCodeDiscovery {
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

#[derive(Debug, Clone)]
pub struct DeviceCodeStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
struct DiscoveryResponse {
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    device_authorization_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

pub async fn fetch_oauth_discovery(client: &Client) -> Result<OAuthDiscovery> {
    let discovery = fetch_discovery(client).await?;
    let authorization_endpoint = require_trusted_endpoint(
        discovery.authorization_endpoint.as_deref().ok_or_else(|| {
            anyhow::Error::msg("xAI OAuth discovery missing authorization_endpoint")
        })?,
        "authorization endpoint",
    )?;
    let token_endpoint = require_trusted_endpoint(
        discovery
            .token_endpoint
            .as_deref()
            .ok_or_else(|| anyhow::Error::msg("xAI OAuth discovery missing token_endpoint"))?,
        "token endpoint",
    )?;
    Ok(OAuthDiscovery {
        authorization_endpoint,
        token_endpoint,
    })
}

pub async fn fetch_device_code_discovery(client: &Client) -> Result<DeviceCodeDiscovery> {
    let discovery = fetch_discovery(client).await?;
    let device_authorization_endpoint = require_trusted_endpoint(
        discovery
            .device_authorization_endpoint
            .as_deref()
            .ok_or_else(|| {
                anyhow::Error::msg("xAI OAuth discovery missing device_authorization_endpoint")
            })?,
        "device authorization endpoint",
    )?;
    let token_endpoint = require_trusted_endpoint(
        discovery
            .token_endpoint
            .as_deref()
            .ok_or_else(|| anyhow::Error::msg("xAI OAuth discovery missing token_endpoint"))?,
        "token endpoint",
    )?;
    Ok(DeviceCodeDiscovery {
        device_authorization_endpoint,
        token_endpoint,
    })
}

async fn fetch_discovery(client: &Client) -> Result<DiscoveryResponse> {
    let response = client
        .get(XAI_OAUTH_DISCOVERY_URL)
        .header("Accept", "application/json")
        .send()
        .await
        .context("Failed to fetch xAI OAuth discovery")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("xAI OAuth discovery failed ({status}): {body}");
    }
    response
        .json()
        .await
        .context("Failed to parse xAI OAuth discovery")
}

pub fn build_authorize_url(authorization_endpoint: &str, pkce: &PkceState) -> String {
    let mut params = BTreeMap::new();
    params.insert("response_type", "code");
    params.insert("client_id", XAI_OAUTH_CLIENT_ID);
    params.insert("redirect_uri", XAI_OAUTH_REDIRECT_URI);
    params.insert("scope", XAI_OAUTH_SCOPE);
    params.insert("state", pkce.state.as_str());
    params.insert("code_challenge", pkce.code_challenge.as_str());
    params.insert("code_challenge_method", "S256");
    params.insert("plan", "generic");
    params.insert("referrer", "zeroclaw");

    let encoded = params
        .into_iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>();
    format!("{authorization_endpoint}?{}", encoded.join("&"))
}

pub async fn exchange_code_for_tokens(
    client: &Client,
    token_endpoint: &str,
    code: &str,
    pkce: &PkceState,
) -> Result<TokenSet> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", XAI_OAUTH_REDIRECT_URI),
        ("client_id", XAI_OAUTH_CLIENT_ID),
        ("code_verifier", pkce.code_verifier.as_str()),
        // xAI's shared OAuth client validates these PKCE fields again at token exchange.
        ("code_challenge", pkce.code_challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];

    let response = client
        .post(require_trusted_endpoint(token_endpoint, "token endpoint")?)
        .form(&form)
        .send()
        .await
        .context("Failed to exchange xAI OAuth authorization code")?;

    parse_token_response(response).await
}

pub async fn refresh_access_token(client: &Client, refresh_token: &str) -> Result<TokenSet> {
    let discovery = fetch_oauth_discovery(client).await?;
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", XAI_OAUTH_CLIENT_ID),
    ];

    let response = client
        .post(discovery.token_endpoint)
        .form(&form)
        .send()
        .await
        .context("Failed to refresh xAI OAuth token")?;

    parse_token_response(response).await
}

pub async fn start_device_code_flow(
    client: &Client,
    device_authorization_endpoint: &str,
) -> Result<DeviceCodeStart> {
    let form = [
        ("client_id", XAI_OAUTH_CLIENT_ID),
        ("scope", XAI_OAUTH_SCOPE),
    ];

    let response = client
        .post(require_trusted_endpoint(
            device_authorization_endpoint,
            "device authorization endpoint",
        )?)
        .form(&form)
        .send()
        .await
        .context("Failed to start xAI OAuth device-code flow")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("xAI device-code start failed ({status}): {body}");
    }

    let parsed: DeviceCodeResponse = response
        .json()
        .await
        .context("Failed to parse xAI device-code response")?;
    Ok(DeviceCodeStart {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        verification_uri: require_trusted_endpoint(&parsed.verification_uri, "verification URI")?,
        verification_uri_complete: parsed
            .verification_uri_complete
            .as_deref()
            .map(|uri| require_trusted_endpoint(uri, "complete verification URI"))
            .transpose()?,
        expires_in: parsed.expires_in,
        interval: parsed.interval.unwrap_or(5).max(1),
    })
}

pub async fn poll_device_code_tokens(
    client: &Client,
    token_endpoint: &str,
    device: &DeviceCodeStart,
) -> Result<TokenSet> {
    let token_endpoint = require_trusted_endpoint(token_endpoint, "token endpoint")?;
    let started = Instant::now();
    let mut interval_secs = device.interval.max(1);

    loop {
        if started.elapsed() > Duration::from_secs(device.expires_in) {
            anyhow::bail!("xAI device-code flow timed out before authorization completed");
        }

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        let form = [
            ("grant_type", XAI_DEVICE_CODE_GRANT_TYPE),
            ("device_code", device.device_code.as_str()),
            ("client_id", XAI_OAUTH_CLIENT_ID),
        ];

        let response = client
            .post(&token_endpoint)
            .form(&form)
            .send()
            .await
            .context("Failed polling xAI device-code token endpoint")?;

        if response.status().is_success() {
            return parse_token_response(response).await;
        }

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if let Ok(err) = serde_json::from_str::<OAuthErrorResponse>(&text) {
            match err.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval_secs = interval_secs.saturating_add(5);
                    continue;
                }
                "access_denied" | "authorization_denied" => {
                    anyhow::bail!("xAI device-code authorization was denied")
                }
                "expired_token" => anyhow::bail!("xAI device-code expired"),
                _ => anyhow::bail!(
                    "xAI device-code polling failed ({status}): {}",
                    err.error_description.unwrap_or(err.error)
                ),
            }
        }
        anyhow::bail!("xAI device-code polling failed ({status}): {text}");
    }
}

async fn parse_token_response(response: reqwest::Response) -> Result<TokenSet> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        if let Ok(err) = serde_json::from_str::<OAuthErrorResponse>(&body) {
            anyhow::bail!(
                "xAI OAuth token request failed ({status}): {}",
                err.error_description.unwrap_or(err.error)
            );
        }
        anyhow::bail!("xAI OAuth token request failed ({status}): {body}");
    }

    let parsed: TokenResponse =
        serde_json::from_str(&body).context("Failed to parse xAI OAuth token response")?;
    let expires_at = parsed
        .expires_in
        .map(|secs| Utc::now() + chrono::Duration::seconds(secs))
        .or_else(|| derive_expires_at_from_jwt(&parsed.access_token));

    Ok(TokenSet {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        id_token: parsed.id_token,
        expires_at,
        token_type: parsed.token_type.or_else(|| Some("Bearer".into())),
        scope: parsed.scope,
    })
}

pub async fn receive_loopback_code(expected_state: &str, timeout: Duration) -> Result<String> {
    ::zeroclaw_log::scope!(
        model_provider_type: "xai",
        model_provider_alias: "oauth",
        => async move {
            receive_loopback_code_inner(expected_state, timeout).await
        }
    )
    .await
}

async fn receive_loopback_code_inner(expected_state: &str, timeout: Duration) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:56121")
        .await
        .context("Failed to bind callback listener at 127.0.0.1:56121")?;
    let accepted = tokio::time::timeout(timeout, listener.accept())
        .await
        .context("Timed out waiting for xAI browser callback")?
        .context("Failed to accept xAI callback connection")?;

    let (mut stream, _) = accepted;
    let mut buffer = vec![0_u8; 8192];
    let bytes_read = stream
        .read(&mut buffer)
        .await
        .context("Failed to read xAI callback request")?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow::Error::msg("Malformed xAI callback request"))?;
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::Error::msg("xAI callback request missing path"))?;
    let code = parse_code_from_redirect(path, Some(expected_state))?;

    let body = "<html><body><h2>ZeroClaw xAI login complete</h2><p>You can close this tab.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
    Ok(code)
}

pub fn parse_code_from_redirect(input: &str, expected_state: Option<&str>) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        anyhow::bail!("No xAI OAuth code provided");
    }
    let query = trimmed.split_once('?').map_or(trimmed, |(_, query)| query);
    let params = parse_query_params(query);
    if let Some(err) = params.get("error") {
        let desc = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| "xAI OAuth authorization failed".to_string());
        anyhow::bail!("{err}: {desc}");
    }
    if let Some(expected) = expected_state {
        let actual = params
            .get("state")
            .ok_or_else(|| anyhow::Error::msg("xAI OAuth callback missing state parameter"))?;
        if actual != expected {
            anyhow::bail!("xAI OAuth state mismatch");
        }
    }
    if let Some(code) = params.get("code") {
        if !code.trim().is_empty() {
            return Ok(code.trim().to_string());
        }
    }
    if !trimmed.contains('=') && !trimmed.contains('?') {
        return Ok(trimmed.to_string());
    }
    anyhow::bail!("xAI OAuth callback missing code parameter")
}

pub fn extract_account_id_from_jwt(token: &str) -> Option<String> {
    let payload = decode_jwt_payload(token)?;
    payload
        .get("email")
        .or_else(|| payload.get("sub"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

pub async fn import_grok_auth_profile(
    auth_service: &super::AuthService,
    profile: &str,
    import_path: &std::path::Path,
) -> Result<()> {
    ::zeroclaw_log::scope!(
        model_provider_type: "xai",
        model_provider_alias: profile,
        => async move {
            import_grok_auth_profile_inner(auth_service, profile, import_path).await
        }
    )
    .await
}

async fn import_grok_auth_profile_inner(
    auth_service: &super::AuthService,
    profile: &str,
    import_path: &std::path::Path,
) -> Result<()> {
    #[derive(Debug, Deserialize)]
    struct GrokAuthEntry {
        key: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_at: Option<String>,
        #[serde(default)]
        user_id: Option<String>,
        #[serde(default)]
        principal_id: Option<String>,
        #[serde(default)]
        email: Option<String>,
        #[serde(default)]
        oidc_issuer: Option<String>,
        #[serde(default)]
        oidc_client_id: Option<String>,
    }

    let raw = std::fs::read_to_string(import_path).with_context(|| {
        format!(
            "Failed to read Grok auth import file {}",
            import_path.display()
        )
    })?;
    let entries: BTreeMap<String, GrokAuthEntry> =
        serde_json::from_str(&raw).with_context(|| {
            format!(
                "Failed to parse Grok auth import file {}",
                import_path.display()
            )
        })?;

    let expected_key = format!("{XAI_OAUTH_ISSUER}::{XAI_OAUTH_CLIENT_ID}");
    let entry = entries
        .get(&expected_key)
        .or_else(|| {
            entries.values().find(|entry| {
                entry.oidc_issuer.as_deref() == Some(XAI_OAUTH_ISSUER)
                    && entry.oidc_client_id.as_deref() == Some(XAI_OAUTH_CLIENT_ID)
            })
        })
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "Grok auth file does not contain credentials for {expected_key}"
            ))
        })?;

    let expires_at = entry
        .expires_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| derive_expires_at_from_jwt(&entry.key));

    let token_set = TokenSet {
        access_token: entry.key.clone(),
        refresh_token: entry.refresh_token.clone(),
        id_token: None,
        expires_at,
        token_type: Some("Bearer".to_string()),
        scope: None,
    };
    let account_id = entry
        .email
        .clone()
        .or_else(|| entry.user_id.clone())
        .or_else(|| entry.principal_id.clone())
        .or_else(|| extract_account_id_from_jwt(&token_set.access_token));

    auth_service
        .store_xai_tokens(profile, token_set, account_id, true)
        .await?;
    Ok(())
}

fn derive_expires_at_from_jwt(token: &str) -> Option<chrono::DateTime<Utc>> {
    let payload = decode_jwt_payload(token)?;
    let exp = payload.get("exp")?.as_i64()?;
    chrono::DateTime::<Utc>::from_timestamp(exp, 0)
}

fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::URL_SAFE
                .decode(payload)
                .ok()
        })?;
    serde_json::from_slice(&bytes).ok()
}

fn require_trusted_endpoint(endpoint: &str, label: &str) -> Result<String> {
    let url = reqwest::Url::parse(endpoint).with_context(|| format!("Invalid xAI {label}"))?;
    if url.scheme() != "https" {
        anyhow::bail!("xAI OAuth discovery returned non-HTTPS {label}");
    }
    let host = url.host_str().unwrap_or_default();
    if host == "x.ai" || host.ends_with(".x.ai") {
        return Ok(endpoint.to_string());
    }
    anyhow::bail!("xAI OAuth discovery returned untrusted {label}: {endpoint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_contains_xai_oauth_params() {
        let pkce = PkceState {
            code_verifier: "verifier".into(),
            code_challenge: "challenge".into(),
            state: "state".into(),
        };
        let url = build_authorize_url("https://auth.x.ai/oauth2/authorize", &pkce);
        assert!(url.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(url.contains(
            "scope=openid%20profile%20email%20offline_access%20grok-cli%3Aaccess%20api%3Aaccess"
        ));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A56121%2Fcallback"));
        assert!(url.contains("code_challenge=challenge"));
    }

    #[test]
    fn parse_redirect_validates_state() {
        let code = parse_code_from_redirect("/callback?code=abc&state=xyz", Some("xyz"))
            .expect("code should parse");
        assert_eq!(code, "abc");
        assert!(parse_code_from_redirect("/callback?code=abc&state=bad", Some("xyz")).is_err());
    }
}
