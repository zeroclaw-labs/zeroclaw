//! MiniMax Portal OAuth 2.0 Device Code + PKCE authentication flow.
//!
//! Ports the OAuth flow from OpenClaw's minimax-portal-auth TypeScript plugin.
//! Supports global region (api.minimax.io).

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::Duration;

pub const MINIMAX_PROVIDER: &str = "minimax";
const MINIMAX_PORTAL_BASE_URL: &str = "https://api.minimax.io";

const CLIENT_ID: &str = "78257093-7e40-4613-99e0-527b14b39113";
const SCOPE: &str = "group_id profile model.completion";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:user_code";

/// PKCE parameters.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

/// Generate PKCE code verifier, challenge (S256), and state.
pub fn generate_pkce() -> Pkce {
    let mut rng = rand::thread_rng();

    // code_verifier: 32 random bytes → base64url
    let mut verifier_bytes = [0u8; 32];
    rng.fill_bytes(&mut verifier_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    // code_challenge: SHA256(verifier) → base64url
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    // state: 16 random bytes → base64url
    let mut state_bytes = [0u8; 16];
    rng.fill_bytes(&mut state_bytes);
    let state = URL_SAFE_NO_PAD.encode(state_bytes);

    Pkce {
        verifier,
        challenge,
        state,
    }
}

/// Response from the OAuth code request.
#[derive(Debug, serde::Deserialize)]
pub struct OAuthCodeResponse {
    pub user_code: String,
    pub verification_uri: String,
    /// Unix timestamp when the code expires.
    #[serde(default)]
    pub expired_in: u64,
    #[serde(default)]
    pub interval: Option<u64>,
    #[serde(default)]
    pub state: Option<String>,
}

/// Request a device code from the MiniMax OAuth server.
pub async fn request_oauth_code(pkce: &Pkce) -> Result<OAuthCodeResponse> {
    let url = format!("{MINIMAX_PORTAL_BASE_URL}/oauth/code");
    let client = reqwest::Client::new();

    let resp = client
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(format!(
            "response_type=code&client_id={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
            CLIENT_ID,
            urlencod(SCOPE),
            pkce.challenge,
            pkce.state,
        ))
        .send()
        .await
        .context("Failed to request OAuth code")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("OAuth code request failed (HTTP {status}): {body}");
    }

    let code_resp: OAuthCodeResponse = resp
        .json()
        .await
        .context("Failed to parse OAuth code response")?;

    // Verify state to prevent CSRF
    if let Some(ref resp_state) = code_resp.state {
        if resp_state != &pkce.state {
            bail!("OAuth state mismatch: possible CSRF attack or session corruption");
        }
    }

    Ok(code_resp)
}

/// Successful token from the OAuth server.
#[derive(Debug, Clone)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    /// Expiration (unix timestamp or seconds, as returned by server).
    pub expired_in: u64,
    pub resource_url: Option<String>,
    pub notification_message: Option<String>,
}

/// Raw JSON token response from MiniMax.
#[derive(Debug, serde::Deserialize)]
struct RawTokenResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expired_in: Option<u64>,
    #[serde(default)]
    resource_url: Option<String>,
    #[serde(default)]
    notification_message: Option<String>,
    #[serde(default)]
    base_resp: Option<BaseResp>,
}

#[derive(Debug, serde::Deserialize)]
struct BaseResp {
    #[serde(default)]
    status_msg: Option<String>,
}

/// Token polling result.
#[derive(Debug)]
pub enum TokenPollResult {
    Success(OAuthTokenResponse),
    Pending,
    Error(String),
}

/// Poll for the OAuth token.
pub async fn poll_oauth_token(pkce: &Pkce, user_code: &str) -> Result<TokenPollResult> {
    let url = format!("{MINIMAX_PORTAL_BASE_URL}/oauth/token");
    let client = reqwest::Client::new();

    let resp = client
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(format!(
            "grant_type={}&client_id={}&user_code={}&code_verifier={}",
            urlencod(GRANT_TYPE),
            CLIENT_ID,
            user_code,
            pkce.verifier,
        ))
        .send()
        .await
        .context("Failed to poll OAuth token")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Ok(TokenPollResult::Error(format!("HTTP error: {body}")));
    }

    let body = resp.text().await.unwrap_or_default();
    parse_token_response(&body)
}

fn parse_token_response(body: &str) -> Result<TokenPollResult> {
    let raw: RawTokenResponse =
        serde_json::from_str(body).context("Failed to parse token response JSON")?;

    let status = raw.status.as_deref().unwrap_or("");

    if status == "error" {
        let msg = raw
            .base_resp
            .and_then(|b| b.status_msg)
            .unwrap_or_else(|| "An error occurred. Please try again later".into());
        return Ok(TokenPollResult::Error(msg));
    }

    if status != "success" {
        // Any non-success, non-error status means pending
        return Ok(TokenPollResult::Pending);
    }

    // status == "success"
    match (raw.access_token, raw.refresh_token, raw.expired_in) {
        (Some(access), Some(refresh), Some(expired_in)) => {
            Ok(TokenPollResult::Success(OAuthTokenResponse {
                access_token: access,
                refresh_token: refresh,
                expired_in,
                resource_url: raw.resource_url,
                notification_message: raw.notification_message,
            }))
        }
        _ => Ok(TokenPollResult::Error(
            "MiniMax OAuth returned incomplete token payload".into(),
        )),
    }
}

/// Simple percent-encoding for URL form values.
fn urlencod(s: &str) -> String {
    s.replace(' ', "+").replace(':', "%3A").replace('/', "%2F")
}

/// Run the full MiniMax Portal OAuth login flow.
///
/// Returns the access token on success.
pub async fn login_minimax_portal_oauth() -> Result<OAuthTokenResponse> {
    let pkce = generate_pkce();

    // Step 1: Request device code
    let code_resp = request_oauth_code(&pkce).await?;

    // Step 2: Show user instructions
    println!();
    println!("🔐 MiniMax Portal OAuth Login (Global)");
    println!();
    println!("  Please open the following URL in your browser:");
    println!("  {}", code_resp.verification_uri);
    println!();
    println!("  And enter code: {}", code_resp.user_code);
    println!();

    // Try to open browser automatically
    open_url_in_browser(&code_resp.verification_uri);

    println!("  Waiting for authorization...");
    println!();

    // Step 3: Poll for token
    let poll_interval_ms = code_resp.interval.unwrap_or(2000);
    let max_wait = Duration::from_secs(if code_resp.expired_in > 0 {
        // expired_in is a unix timestamp; compute remaining seconds
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        code_resp.expired_in.saturating_sub(now).max(30)
    } else {
        300
    });
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > max_wait {
            bail!(
                "OAuth authorization timed out after {}s",
                max_wait.as_secs()
            );
        }

        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;

        match poll_oauth_token(&pkce, &code_resp.user_code).await? {
            TokenPollResult::Success(token) => {
                println!("  ✅ Authorization successful!");
                return Ok(token);
            }
            TokenPollResult::Pending => {
                // Continue polling
            }
            TokenPollResult::Error(e) => {
                bail!("OAuth token error: {e}");
            }
        }
    }
}

/// Open a URL in the default browser (best-effort, non-blocking).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_generates_valid_values() {
        let pkce = generate_pkce();

        // verifier should be base64url encoded 32 bytes (~43 chars)
        assert!(!pkce.verifier.is_empty());
        assert!(pkce.verifier.len() >= 40);

        // challenge should be base64url encoded SHA256 (~43 chars)
        assert!(!pkce.challenge.is_empty());
        assert!(pkce.challenge.len() >= 40);

        // Verify challenge = SHA256(verifier)
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);

        // state should be base64url encoded 16 bytes (~22 chars)
        assert!(!pkce.state.is_empty());
        assert!(pkce.state.len() >= 20);
    }

    #[test]
    fn pkce_is_unique_each_call() {
        let a = generate_pkce();
        let b = generate_pkce();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn parse_pending_response() {
        let result = parse_token_response(r#"{"status":"pending"}"#).unwrap();
        assert!(matches!(result, TokenPollResult::Pending));
    }

    #[test]
    fn parse_success_response() {
        let result = parse_token_response(
            r#"{"status":"success","access_token":"tok_abc","refresh_token":"ref_abc","expired_in":1700000000,"resource_url":"https://example.com","notification_message":"Welcome"}"#,
        )
        .unwrap();
        match result {
            TokenPollResult::Success(t) => {
                assert_eq!(t.access_token, "tok_abc");
                assert_eq!(t.refresh_token, "ref_abc");
                assert_eq!(t.expired_in, 1700000000);
                assert_eq!(t.resource_url.as_deref(), Some("https://example.com"));
                assert_eq!(t.notification_message.as_deref(), Some("Welcome"));
            }
            _ => panic!("Expected Success"),
        }
    }

    #[test]
    fn parse_error_response() {
        let result = parse_token_response(
            r#"{"status":"error","base_resp":{"status_msg":"access denied"}}"#,
        )
        .unwrap();
        assert!(matches!(result, TokenPollResult::Error(_)));
    }

    #[test]
    fn parse_success_incomplete_returns_error() {
        // success status but missing required fields
        let result = parse_token_response(r#"{"status":"success","access_token":"tok"}"#).unwrap();
        assert!(matches!(result, TokenPollResult::Error(_)));
    }

    #[test]
    fn region_urls() {
        assert_eq!(MINIMAX_PORTAL_BASE_URL, "https://api.minimax.io");
        assert_eq!(MINIMAX_PROVIDER, "minimax");
    }
}
