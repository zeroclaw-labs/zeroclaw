//! Gmail OAuth2 authentication flow for IMAP/SMTP XOAUTH2.
//!
//! Supports:
//! - Authorization code flow with PKCE (loopback redirect on port 1457)
//! - Device code flow for headless environments
//!
//! Unlike gemini_oauth, client credentials are passed as parameters from
//! the user's email channel config rather than read from environment variables.

use crate::auth::oauth_common::{parse_query_params, url_decode, url_encode};
use crate::auth::profiles::TokenSet;
use anyhow::{Context, Result};
use base64::Engine;
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[allow(unused_imports)]
pub use crate::auth::oauth_common::{generate_pkce_state, PkceState};

pub const EMAIL_OAUTH_REDIRECT_URI: &str = "http://localhost:1457/auth/callback";

/// Gmail IMAP/SMTP scope — full mailbox access required for XOAUTH2.
pub const EMAIL_OAUTH_SCOPES: &str = "https://mail.google.com/";

// Google OAuth endpoints (shared with Gemini)
const GOOGLE_OAUTH_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_OAUTH_DEVICE_CODE_URL: &str = "https://oauth2.googleapis.com/device/code";

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
    verification_url: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Build the Google authorization URL for Gmail XOAUTH2 access.
pub fn build_authorize_url(client_id: &str, pkce: &PkceState) -> String {
    let mut params = BTreeMap::new();
    params.insert("response_type", "code");
    params.insert("client_id", client_id);
    params.insert("redirect_uri", EMAIL_OAUTH_REDIRECT_URI);
    params.insert("scope", EMAIL_OAUTH_SCOPES);
    params.insert("code_challenge", pkce.code_challenge.as_str());
    params.insert("code_challenge_method", "S256");
    params.insert("state", pkce.state.as_str());
    params.insert("access_type", "offline");
    params.insert("prompt", "consent");

    let mut encoded: Vec<String> = Vec::with_capacity(params.len());
    for (k, v) in params {
        encoded.push(format!("{}={}", url_encode(k), url_encode(v)));
    }

    format!("{}?{}", GOOGLE_OAUTH_AUTHORIZE_URL, encoded.join("&"))
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code_for_tokens(
    client: &Client,
    code: &str,
    pkce: &PkceState,
    client_id: &str,
    client_secret: &str,
) -> Result<TokenSet> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", EMAIL_OAUTH_REDIRECT_URI),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code_verifier", &pkce.code_verifier),
    ];

    let response = client
        .post(GOOGLE_OAUTH_TOKEN_URL)
        .form(&form)
        .send()
        .await
        .context("Failed to send token exchange request")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read token response body")?;

    if !status.is_success() {
        if let Ok(err) = serde_json::from_str::<OAuthErrorResponse>(&body) {
            anyhow::bail!(
                "Google OAuth error: {} - {}",
                err.error,
                err.error_description.unwrap_or_default()
            );
        }
        anyhow::bail!("Google OAuth token exchange failed ({}): {}", status, body);
    }

    let token_response: TokenResponse =
        serde_json::from_str(&body).context("Failed to parse token response")?;

    let expires_at = token_response
        .expires_in
        .map(|secs| Utc::now() + chrono::Duration::seconds(secs));

    Ok(TokenSet {
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        id_token: token_response.id_token,
        expires_at,
        token_type: token_response.token_type.or_else(|| Some("Bearer".into())),
        scope: token_response.scope,
    })
}

/// Refresh an access token using a refresh token.
pub async fn refresh_access_token(
    client: &Client,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<TokenSet> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ];

    let response = client
        .post(GOOGLE_OAUTH_TOKEN_URL)
        .form(&form)
        .send()
        .await
        .context("Failed to send refresh token request")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read refresh response body")?;

    if !status.is_success() {
        if let Ok(err) = serde_json::from_str::<OAuthErrorResponse>(&body) {
            anyhow::bail!(
                "Google OAuth refresh error: {} - {}",
                err.error,
                err.error_description.unwrap_or_default()
            );
        }
        anyhow::bail!("Google OAuth refresh failed ({}): {}", status, body);
    }

    let token_response: TokenResponse =
        serde_json::from_str(&body).context("Failed to parse refresh response")?;

    let expires_at = token_response
        .expires_in
        .map(|secs| Utc::now() + chrono::Duration::seconds(secs));

    Ok(TokenSet {
        access_token: token_response.access_token,
        refresh_token: token_response
            .refresh_token
            .or_else(|| Some(refresh_token.to_string())),
        id_token: token_response.id_token,
        expires_at,
        token_type: token_response.token_type.or_else(|| Some("Bearer".into())),
        scope: token_response.scope,
    })
}

/// Start a device code flow for headless environments.
pub async fn start_device_code_flow(client: &Client, client_id: &str) -> Result<DeviceCodeStart> {
    let form = [("client_id", client_id), ("scope", EMAIL_OAUTH_SCOPES)];

    let response = client
        .post(GOOGLE_OAUTH_DEVICE_CODE_URL)
        .form(&form)
        .send()
        .await
        .context("Failed to start device code flow")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read device code response")?;

    if !status.is_success() {
        if status == 403 && (body.contains("Cloudflare") || body.contains("challenge-platform")) {
            anyhow::bail!(
                "Device-code endpoint is protected by Cloudflare (403 Forbidden). \
                This is expected for server environments. Use browser flow instead."
            );
        }

        if let Ok(err) = serde_json::from_str::<OAuthErrorResponse>(&body) {
            anyhow::bail!(
                "Google device code error: {} - {}",
                err.error,
                err.error_description.unwrap_or_default()
            );
        }
        anyhow::bail!("Google device code request failed ({}): {}", status, body);
    }

    let device_response: DeviceCodeResponse =
        serde_json::from_str(&body).context("Failed to parse device code response")?;

    let user_code = device_response.user_code;
    let verification_url = device_response.verification_url;

    Ok(DeviceCodeStart {
        device_code: device_response.device_code,
        verification_uri_complete: Some(format!("{}?user_code={}", &verification_url, &user_code)),
        user_code,
        verification_uri: verification_url,
        expires_in: device_response.expires_in.unwrap_or(1800),
        interval: device_response.interval.unwrap_or(5),
    })
}

/// Poll the device code endpoint until the user authorizes.
pub async fn poll_device_code_tokens(
    client: &Client,
    device: &DeviceCodeStart,
    client_id: &str,
    client_secret: &str,
) -> Result<TokenSet> {
    let deadline = std::time::Instant::now() + Duration::from_secs(device.expires_in);
    let interval = Duration::from_secs(device.interval.max(5));

    loop {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("Device code expired before authorization was completed");
        }

        tokio::time::sleep(interval).await;

        let form = [
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("device_code", device.device_code.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ];

        let response = client
            .post(GOOGLE_OAUTH_TOKEN_URL)
            .form(&form)
            .send()
            .await
            .context("Failed to poll device code")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("Failed to read device code poll response body")?;

        if status.is_success() {
            let token_response: TokenResponse =
                serde_json::from_str(&body).context("Failed to parse token response")?;

            let expires_at = token_response
                .expires_in
                .map(|secs| Utc::now() + chrono::Duration::seconds(secs));

            return Ok(TokenSet {
                access_token: token_response.access_token,
                refresh_token: token_response.refresh_token,
                id_token: token_response.id_token,
                expires_at,
                token_type: token_response.token_type.or_else(|| Some("Bearer".into())),
                scope: token_response.scope,
            });
        }

        let err: OAuthErrorResponse = serde_json::from_str(&body).context(format!(
            "Device code poll returned non-JSON error (HTTP {}): {}",
            status, body
        ))?;

        match err.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            "access_denied" => {
                anyhow::bail!("User denied authorization");
            }
            "expired_token" => {
                anyhow::bail!("Device code expired");
            }
            _ => {
                anyhow::bail!(
                    "Google OAuth error: {} - {}",
                    err.error,
                    err.error_description.unwrap_or_default()
                );
            }
        }
    }
}

/// Receive OAuth code via loopback callback OR manual stdin input.
pub async fn receive_loopback_code(expected_state: &str, timeout: Duration) -> Result<String> {
    let listener = match TcpListener::bind("127.0.0.1:1457").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Could not bind to localhost:1457: {e}");
            eprintln!("Falling back to manual input.");
            return receive_code_from_stdin(expected_state).await;
        }
    };

    println!("Waiting for callback at http://localhost:1457/auth/callback ...");
    println!("(Or paste the full callback URL / authorization code below if running remotely)");

    let accept_result = tokio::time::timeout(timeout, listener.accept()).await;

    match accept_result {
        Ok(Ok((mut stream, _))) => {
            let mut buffer = vec![0u8; 4096];
            let n = stream
                .read(&mut buffer)
                .await
                .context("Failed to read from callback connection")?;

            let request = String::from_utf8_lossy(&buffer[..n]);
            let (code, state) = parse_callback_request(&request)?;

            if state != expected_state {
                let response = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\n\r\n\
                     <html><body><h1>State mismatch</h1><p>Please try again.</p></body></html>";
                let _ = stream.write_all(response.as_bytes()).await;
                anyhow::bail!("OAuth state mismatch");
            }

            let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
                 <html><body><h1>Success!</h1><p>You can close this window and return to the terminal.</p></body></html>";
            let _ = stream.write_all(response.as_bytes()).await;

            Ok(code)
        }
        Ok(Err(e)) => Err(anyhow::anyhow!("Failed to accept connection: {e}")),
        Err(_) => {
            eprintln!("\nCallback timeout. Falling back to manual input.");
            receive_code_from_stdin(expected_state).await
        }
    }
}

async fn receive_code_from_stdin(expected_state: &str) -> Result<String> {
    use std::io::{self, BufRead};

    let expected = expected_state.to_string();
    let input = tokio::task::spawn_blocking(move || {
        let stdin = io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).ok();
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!("No input received"));
        }
        parse_code_from_redirect(&trimmed, Some(&expected))
    })
    .await
    .context("Failed to read from stdin")??;

    Ok(input)
}

fn parse_callback_request(request: &str) -> Result<(String, String)> {
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();

    let query_start = path.find('?').map(|i| i + 1).unwrap_or(path.len());
    let query = &path[query_start..];

    let mut code = None;
    let mut state = None;

    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            match key {
                "code" => code = Some(url_decode(value)),
                "state" => state = Some(url_decode(value)),
                _ => {}
            }
        }
    }

    let code = code.ok_or_else(|| anyhow::anyhow!("No 'code' parameter in callback"))?;
    let state = state.ok_or_else(|| anyhow::anyhow!("No 'state' parameter in callback"))?;

    Ok((code, state))
}

pub fn parse_code_from_redirect(input: &str, expected_state: Option<&str>) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        anyhow::bail!("No OAuth code provided");
    }

    let query = if let Some((_, right)) = trimmed.split_once('?') {
        right
    } else {
        trimmed
    };

    let params = parse_query_params(query);

    if let Some(code) = params.get("code") {
        if let Some(expected) = expected_state {
            let actual = params
                .get("state")
                .ok_or_else(|| anyhow::anyhow!("OAuth state parameter missing from redirect"))?;
            if actual != expected {
                anyhow::bail!(
                    "OAuth state mismatch: expected {}, got {}",
                    expected,
                    actual
                );
            }
        }
        return Ok(code.clone());
    }

    if expected_state.is_none()
        && trimmed.len() > 10
        && !trimmed.contains(' ')
        && !trimmed.contains('&')
    {
        return Ok(trimmed.to_string());
    }

    anyhow::bail!("Could not parse OAuth code from input")
}

/// Extract account email from Google ID token (same as Gemini).
pub fn extract_account_email_from_id_token(id_token: &str) -> Option<String> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;

    #[derive(Deserialize)]
    struct IdTokenPayload {
        email: Option<String>,
    }

    let payload: IdTokenPayload = serde_json::from_slice(&payload).ok()?;
    payload.email
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_authorize_url_contains_gmail_scope() {
        let pkce = generate_pkce_state();
        let url = build_authorize_url("test-client-id", &pkce);
        assert!(url.contains("mail.google.com"));
        assert!(url.contains("accounts.google.com"));
    }

    #[test]
    fn build_authorize_url_uses_port_1457() {
        let pkce = generate_pkce_state();
        let url = build_authorize_url("test-client-id", &pkce);
        assert!(url.contains("1457"));
        assert!(url.contains("redirect_uri="));
    }

    #[test]
    fn build_authorize_url_contains_required_params() {
        let pkce = generate_pkce_state();
        let url = build_authorize_url("my-client-id", &pkce);
        assert!(url.contains("client_id=my-client-id"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
    }

    #[test]
    fn parse_code_from_url() {
        let url = "http://localhost:1457/auth/callback?code=4/0test&state=xyz";
        let code = parse_code_from_redirect(url, Some("xyz")).unwrap();
        assert_eq!(code, "4/0test");
    }

    #[test]
    fn parse_code_from_raw() {
        let raw = "4/0AcvDMrC1234567890abcdef";
        let code = parse_code_from_redirect(raw, None).unwrap();
        assert_eq!(code, raw);
    }

    #[test]
    fn extract_email_from_id_token() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"email":"test@example.com"}"#);
        let token = format!("{}.{}.signature", header, payload);

        let email = extract_account_email_from_id_token(&token);
        assert_eq!(email, Some("test@example.com".to_string()));
    }
}
