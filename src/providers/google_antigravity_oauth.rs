//! Google Antigravity OAuth 2.0 PKCE authentication flow.
//!
//! Implements the authorization-code + PKCE flow to obtain OAuth tokens
//! for the Google Cloud Code Assist API (`cloudcode-pa.googleapis.com`),
//! which provides Anthropic-compatible endpoints for Claude models.
//!
//! Ported from OpenClaw's `google-antigravity-auth` TypeScript plugin.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::Duration;

// ── OAuth constants ─────────────────────────────────────────────────
const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";
const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const OAUTH_CALLBACK_PORT: u16 = 51121;
const DEFAULT_PROJECT_ID: &str = "rising-fact-p41fc";
const CALLBACK_TIMEOUT_SECS: u64 = 300;

const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
    https://www.googleapis.com/auth/userinfo.email \
    https://www.googleapis.com/auth/userinfo.profile \
    https://www.googleapis.com/auth/cclog \
    https://www.googleapis.com/auth/experimentsandconfigs";

const CODE_ASSIST_ENDPOINTS: &[&str] = &[
    "https://cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
];

// ── Data structures ─────────────────────────────────────────────────

/// PKCE parameters for OAuth 2.0.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

/// Successful authentication result.
#[derive(Debug, Clone)]
pub struct GoogleAntigravityCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Expiration as unix timestamp (seconds).
    pub expires_at: u64,
    pub email: Option<String>,
    pub project_id: String,
}

#[derive(Debug, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Debug, serde::Deserialize)]
struct UserInfo {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct LoadCodeAssistResponse {
    #[serde(default, rename = "cloudaicompanionProject")]
    cloudaicompanion_project: Option<serde_json::Value>,
}

// ── PKCE ────────────────────────────────────────────────────────────

/// Generate PKCE code verifier, challenge (S256), and state.
pub fn generate_pkce() -> Pkce {
    let mut rng = rand::rng();

    let mut verifier_bytes = [0u8; 32];
    rng.fill_bytes(&mut verifier_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let mut state_bytes = [0u8; 16];
    rng.fill_bytes(&mut state_bytes);
    let state = URL_SAFE_NO_PAD.encode(state_bytes);

    Pkce {
        verifier,
        challenge,
        state,
    }
}

// ── URL construction ────────────────────────────────────────────────

fn build_auth_url(pkce: &Pkce) -> String {
    format!(
        "{AUTH_URL}?\
         client_id={}&\
         response_type=code&\
         redirect_uri={}&\
         scope={}&\
         code_challenge={}&\
         code_challenge_method=S256&\
         state={}&\
         access_type=offline&\
         prompt=consent",
        urlencod(CLIENT_ID),
        urlencod(REDIRECT_URI),
        urlencod(SCOPES),
        urlencod(&pkce.challenge),
        urlencod(&pkce.state),
    )
}

/// Simple percent-encoding for URL query values.
fn urlencod(s: &str) -> String {
    use std::fmt::Write;
    let mut encoded = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            b' ' => encoded.push('+'),
            _ => {
                let _ = write!(encoded, "%{b:02X}");
            }
        }
    }
    encoded
}

// ── Localhost callback server ───────────────────────────────────────

/// Start a TCP listener on `127.0.0.1:OAUTH_CALLBACK_PORT`, wait for the
/// OAuth callback, validate the `state` parameter, and return the
/// authorization code.
async fn wait_for_oauth_callback(expected_state: &str, timeout: Duration) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(format!("127.0.0.1:{OAUTH_CALLBACK_PORT}"))
        .await
        .context("Failed to bind OAuth callback server on port 51121")?;

    let accept = async {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);

        let (code, state) = parse_callback_request(&request)?;

        if state != expected_state {
            bail!("OAuth state mismatch — possible CSRF attack. Please try again.");
        }

        let html = "<!DOCTYPE html><html><body>\
                     <h1>Authentication complete</h1>\
                     <p>You can close this tab and return to the terminal.</p>\
                     </body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html,
        );
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await?;

        Ok::<String, anyhow::Error>(code)
    };

    tokio::time::timeout(timeout, accept)
        .await
        .context("OAuth callback timed out (5 minutes)")?
}

/// Parse the HTTP GET request from the OAuth callback to extract `code` and
/// `state` query parameters.
fn parse_callback_request(request: &str) -> Result<(String, String)> {
    // Request line: GET /oauth-callback?code=XXX&state=YYY HTTP/1.1
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");

    let mut code = None;
    let mut state = None;

    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "code" => code = Some(v.to_string()),
                "state" => state = Some(v.to_string()),
                _ => {}
            }
        }
    }

    let code = code.context("Missing 'code' parameter in OAuth callback")?;
    let state = state.context("Missing 'state' parameter in OAuth callback")?;
    Ok((code, state))
}

// ── Token exchange ──────────────────────────────────────────────────

async fn exchange_code_for_tokens(code: &str, code_verifier: &str) -> Result<TokenResponse> {
    let client = reqwest::Client::new();

    let params = [
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("code", code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", code_verifier),
    ];

    let response = client
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .await
        .context("Failed to reach Google token endpoint")?;

    if !response.status().is_success() {
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "(unreadable)".into());
        bail!("Token exchange failed: {text}");
    }

    let token: TokenResponse = response
        .json()
        .await
        .context("Failed to parse token response")?;

    if token.access_token.is_empty() {
        bail!("Token exchange returned empty access_token");
    }

    Ok(token)
}

// ── Auxiliary data fetchers ─────────────────────────────────────────

async fn fetch_user_email(access_token: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://www.googleapis.com/oauth2/v1/userinfo?alt=json")
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let info: UserInfo = resp.json().await.ok()?;
    info.email
}

async fn fetch_project_id(access_token: &str) -> String {
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
        }
    });

    for endpoint in CODE_ASSIST_ENDPOINTS {
        let url = format!("{endpoint}/v1internal:loadCodeAssist");
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", "application/json")
            .header("User-Agent", "google-api-rust-client/0.1")
            .header(
                "X-Goog-Api-Client",
                "google-cloud-sdk vscode_cloudshelleditor/0.1",
            )
            .header(
                "Client-Metadata",
                r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#,
            )
            .json(&body)
            .send()
            .await;

        let resp = match resp {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };

        if let Ok(data) = resp.json::<LoadCodeAssistResponse>().await {
            if let Some(ref val) = data.cloudaicompanion_project {
                if let Some(id) = extract_project_id(val) {
                    return id;
                }
            }
        }
    }

    DEFAULT_PROJECT_ID.to_string()
}

fn extract_project_id(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
        let id = id.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

// ── Browser helper ──────────────────────────────────────────────────

fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
}

// ── Main entry point ────────────────────────────────────────────────

/// Run the full Google Antigravity OAuth login flow.
///
/// 1. Generate PKCE credentials.
/// 2. Open browser to Google OAuth consent screen.
/// 3. Start localhost callback server to receive the authorization code.
/// 4. Exchange code for access/refresh tokens.
/// 5. Fetch user email and project ID in parallel.
pub async fn login_google_antigravity() -> Result<GoogleAntigravityCredential> {
    let pkce = generate_pkce();
    let auth_url = build_auth_url(&pkce);

    println!();
    println!("  Google Antigravity OAuth Login");
    println!();
    println!("  Opening browser for authorization...");
    println!("  If the browser does not open, visit:");
    println!("  {auth_url}");
    println!();

    open_url_in_browser(&auth_url);

    println!("  Waiting for authorization callback on localhost:{OAUTH_CALLBACK_PORT}...");

    let code =
        wait_for_oauth_callback(&pkce.state, Duration::from_secs(CALLBACK_TIMEOUT_SECS)).await?;

    let token = exchange_code_for_tokens(&code, &pkce.verifier).await?;

    let (email, project_id) = tokio::join!(
        fetch_user_email(&token.access_token),
        fetch_project_id(&token.access_token),
    );

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Subtract 5 minutes buffer, same as the TypeScript original.
    let expires_at = now_secs + token.expires_in.saturating_sub(300);

    Ok(GoogleAntigravityCredential {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at,
        email,
        project_id,
    })
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_generates_valid_values() {
        let pkce = generate_pkce();
        assert!(!pkce.verifier.is_empty());
        assert!(!pkce.challenge.is_empty());
        assert!(!pkce.state.is_empty());

        // Verify challenge = base64url(SHA256(verifier))
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn pkce_is_unique_each_call() {
        let a = generate_pkce();
        let b = generate_pkce();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn build_auth_url_contains_required_params() {
        let pkce = generate_pkce();
        let url = build_auth_url(&pkce);
        assert!(url.starts_with(AUTH_URL));
        assert!(url.contains("client_id="));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("redirect_uri="));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state="));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
    }

    #[test]
    fn extract_project_id_from_string() {
        let val = serde_json::json!("my-project-123");
        assert_eq!(extract_project_id(&val), Some("my-project-123".into()));
    }

    #[test]
    fn extract_project_id_from_object() {
        let val = serde_json::json!({"id": "proj-456"});
        assert_eq!(extract_project_id(&val), Some("proj-456".into()));
    }

    #[test]
    fn extract_project_id_missing_returns_none() {
        assert_eq!(extract_project_id(&serde_json::Value::Null), None);
        assert_eq!(extract_project_id(&serde_json::json!({})), None);
        assert_eq!(extract_project_id(&serde_json::json!("")), None);
    }

    #[test]
    fn parse_callback_request_valid() {
        let req = "GET /oauth-callback?code=AUTH_CODE_123&state=STATE_ABC HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let (code, state) = parse_callback_request(req).unwrap();
        assert_eq!(code, "AUTH_CODE_123");
        assert_eq!(state, "STATE_ABC");
    }

    #[test]
    fn parse_callback_request_missing_code() {
        let req = "GET /oauth-callback?state=ABC HTTP/1.1\r\n\r\n";
        assert!(parse_callback_request(req).is_err());
    }

    #[test]
    fn parse_callback_request_missing_state() {
        let req = "GET /oauth-callback?code=XYZ HTTP/1.1\r\n\r\n";
        assert!(parse_callback_request(req).is_err());
    }
}
