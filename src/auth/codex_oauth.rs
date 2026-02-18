//! OpenAI Codex (ChatGPT OAuth) authentication flow.
//!
//! Uses OAuth 2.0 Authorization Code + PKCE with a localhost callback.
//! The user authenticates via their ChatGPT account in the browser, and
//! the token is used against the `chatgpt.com/backend-api` Codex endpoint.

use anyhow::{Context, Result};
use std::path::PathBuf;

use super::common::{
    generate_pkce, is_headless, is_token_expired, load_oauth_credentials, open_url_in_browser,
    prompt_paste_redirect_url, wait_for_oauth_callback, write_file_secure, OAuthCredentials,
};

// ── Constants ───────────────────────────────────────────────────────

/// OAuth authorization URL (ChatGPT account OAuth).
const AUTH_URL: &str = "https://auth0.openai.com/authorize";
/// OAuth token exchange URL.
const TOKEN_URL: &str = "https://auth0.openai.com/oauth/token";
/// Localhost callback port (matches openclaw convention).
const CALLBACK_PORT: u16 = 1455;
/// Redirect URI for localhost callback.
const REDIRECT_URI: &str = "http://localhost:1455/oauth-callback";
/// OAuth client ID for Codex CLI applications.
const CLIENT_ID: &str = "pdlLIX2Y72MIl2rhLhTE9VV9bN905kBh";
/// OAuth scopes requested.
const SCOPE: &str = "openid profile email offline_access";
/// OAuth audience for ChatGPT API access.
const AUDIENCE: &str = "https://api.openai.com/v1";
/// Timeout for waiting on OAuth callback (seconds).
const CALLBACK_TIMEOUT_SECS: u64 = 300;

/// Default credentials cache directory (relative to user config).
const CREDS_DIR: &str = "zeroclaw/codex";
/// Credentials file name.
const CREDS_FILE: &str = "oauth_creds.json";

// ── Public API ──────────────────────────────────────────────────────

/// Run the full OpenAI Codex OAuth login flow.
///
/// Returns the access token on success.
pub async fn login_codex_oauth() -> Result<String> {
    let pkce = generate_pkce();

    // Build authorization URL
    let auth_url = format!(
        "{AUTH_URL}?\
        response_type=code\
        &client_id={CLIENT_ID}\
        &redirect_uri={redirect}\
        &scope={scope}\
        &audience={AUDIENCE}\
        &code_challenge={challenge}\
        &code_challenge_method=S256\
        &state={state}",
        redirect = url_encode(REDIRECT_URI),
        scope = url_encode(SCOPE),
        challenge = pkce.challenge,
        state = pkce.state,
    );

    // Show user instructions
    println!();
    println!("  \u{1f510} OpenAI Codex (ChatGPT OAuth)");
    println!();
    println!("  Please open the following URL in your browser:");
    println!("  {auth_url}");
    println!();

    // Get the authorization code
    let callback = if is_headless() {
        println!("  (Headless environment detected — paste the redirect URL manually)");
        println!();
        prompt_paste_redirect_url()?
    } else {
        open_url_in_browser(&auth_url);
        println!("  Waiting for authorization...");
        println!();
        wait_for_oauth_callback(CALLBACK_PORT, CALLBACK_TIMEOUT_SECS).await?
    };

    // Verify state for CSRF protection (mandatory)
    match callback.state {
        Some(ref cb_state) if cb_state == &pkce.state => {}
        Some(_) => anyhow::bail!("OAuth state mismatch: possible CSRF attack"),
        None => anyhow::bail!("OAuth callback missing state parameter: possible CSRF attack"),
    }

    // Exchange authorization code for tokens
    let creds = exchange_code_for_token(&callback.code, &pkce.verifier).await?;

    // Cache credentials to disk
    let cache_path = credentials_path();
    let json = serde_json::to_string_pretty(&creds)?;
    write_file_secure(&cache_path, &json).await?;

    println!("  \u{2705} OpenAI Codex authentication successful!");

    Ok(creds.access_token)
}

/// Try to load cached Codex OAuth credentials.
///
/// Returns `Some(token)` if valid cached credentials exist.
pub fn try_load_cached_token() -> Option<String> {
    let path = credentials_path();
    let creds = load_oauth_credentials(&path).ok()?;

    // Check expiry (with 120s buffer)
    if let Some(ref expiry) = creds.expiry {
        if is_token_expired(expiry, 120) {
            tracing::info!("Codex OAuth token expired — re-login required");
            return None;
        }
    }

    Some(creds.access_token)
}

/// Check if cached Codex credentials exist (regardless of expiry).
pub fn has_cached_credentials() -> bool {
    credentials_path().exists()
}

// ── Internal helpers ────────────────────────────────────────────────

/// Exchange an authorization code for access and refresh tokens.
async fn exchange_code_for_token(code: &str, verifier: &str) -> Result<OAuthCredentials> {
    let client = reqwest::Client::new();

    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(format!(
            "grant_type=authorization_code\
            &client_id={CLIENT_ID}\
            &code={code}\
            &redirect_uri={redirect}\
            &code_verifier={verifier}",
            code = url_encode(code),
            redirect = url_encode(REDIRECT_URI),
            verifier = url_encode(verifier),
        ))
        .send()
        .await
        .context("failed to exchange Codex OAuth code for token")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Codex OAuth token exchange failed ({status}): {body}");
    }

    #[derive(serde::Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    }

    let token_resp: TokenResponse = resp
        .json()
        .await
        .context("failed to parse Codex OAuth token response")?;

    let expiry = token_resp.expires_in.map(|secs| {
        let expiry_time = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
        expiry_time.to_rfc3339()
    });

    Ok(OAuthCredentials {
        access_token: token_resp.access_token,
        refresh_token: token_resp.refresh_token,
        expiry,
        project_id: None,
    })
}

/// Path to the cached credentials file.
fn credentials_path() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.config_dir().join(CREDS_DIR).join(CREDS_FILE))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                .join(".config")
                .join(CREDS_DIR)
                .join(CREDS_FILE)
        })
}

/// Minimal URL percent-encoding for query parameter values.
fn url_encode(s: &str) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(s.len() * 2);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                let _ = write!(result, "%{byte:02X}");
            }
        }
    }
    result
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encode_handles_special_chars() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a+b"), "a%2Bb");
        assert_eq!(url_encode("foo@bar.com"), "foo%40bar.com");
    }

    #[test]
    fn credentials_path_is_non_empty() {
        let path = credentials_path();
        assert!(!path.as_os_str().is_empty());
        assert!(path.ends_with(CREDS_FILE));
    }
}
