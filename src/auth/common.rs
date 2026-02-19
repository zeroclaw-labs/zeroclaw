//! Shared utilities for OAuth and token-based authentication flows.
//!
//! Consolidates common patterns used across multiple auth providers:
//! PKCE generation, localhost callback servers, browser opening, and
//! secure file I/O.

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

// ── PKCE (Proof Key for Code Exchange, S256) ────────────────────────

/// PKCE parameters for OAuth Authorization Code + PKCE flows.
#[derive(Debug, Clone)]
pub struct Pkce {
    /// Random code verifier (base64url, 43 chars from 32 random bytes).
    pub verifier: String,
    /// SHA-256 hash of verifier, base64url-encoded (S256 challenge).
    pub challenge: String,
    /// Random state parameter for CSRF protection.
    pub state: String,
}

/// Generate a fresh PKCE parameter set (S256 method).
///
/// Each call produces unique, cryptographically random values.
pub fn generate_pkce() -> Pkce {
    let mut rng = rand::rng();

    // code_verifier: 32 random bytes → base64url (no padding)
    let mut verifier_bytes = [0u8; 32];
    rng.fill_bytes(&mut verifier_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    // code_challenge: SHA256(verifier) → base64url (S256 method)
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    // state: 16 random bytes → base64url (CSRF protection)
    let mut state_bytes = [0u8; 16];
    rng.fill_bytes(&mut state_bytes);
    let state = URL_SAFE_NO_PAD.encode(state_bytes);

    Pkce {
        verifier,
        challenge,
        state,
    }
}

// ── Localhost OAuth callback server ─────────────────────────────────

/// Result from a localhost OAuth callback.
#[derive(Debug, Clone)]
pub struct OAuthCallbackResult {
    /// The authorization code received from the OAuth provider.
    pub code: String,
    /// The state parameter echoed back (for CSRF verification).
    pub state: Option<String>,
}

/// Start a temporary localhost HTTP server that waits for an OAuth callback.
///
/// Binds to `127.0.0.1:{port}` and waits for a single GET request with
/// `?code=...&state=...` query parameters. Returns the extracted values
/// and shuts down immediately after the first request.
///
/// The server responds with a simple HTML page telling the user they
/// can close the browser tab.
pub async fn wait_for_oauth_callback(port: u16, timeout_secs: u64) -> Result<OAuthCallbackResult> {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .with_context(|| format!("failed to bind localhost:{port} for OAuth callback"))?;

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        accept_oauth_callback(&listener),
    )
    .await
    .map_err(|_| anyhow::anyhow!("OAuth callback timed out after {timeout_secs}s"))??;

    Ok(result)
}

/// Accept a single HTTP connection and extract OAuth callback parameters.
async fn accept_oauth_callback(listener: &TcpListener) -> Result<OAuthCallbackResult> {
    let (mut stream, _addr) = listener.accept().await?;

    // Read the HTTP request (we only need the first line for GET params)
    let mut buf = vec![0u8; 4096];
    let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse the request line: "GET /oauth-callback?code=...&state=... HTTP/1.1"
    let first_line = request.lines().next().unwrap_or_default();
    let path = first_line.split_whitespace().nth(1).unwrap_or_default();

    let query = path.split('?').nth(1).unwrap_or_default();

    let mut code = None;
    let mut state = None;

    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or_default();
        let value = kv.next().unwrap_or_default();
        match key {
            "code" => code = Some(url_decode(value)),
            "state" => state = Some(url_decode(value)),
            _ => {}
        }
    }

    // Send a success response to the browser
    let html = r#"<!DOCTYPE html>
<html><head><title>ZeroClaw Auth</title></head>
<body style="font-family:system-ui;text-align:center;padding:60px">
<h2>Authentication successful!</h2>
<p>You can close this tab and return to your terminal.</p>
</body></html>"#;

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );

    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;

    let code = code.ok_or_else(|| anyhow::anyhow!("OAuth callback missing 'code' parameter"))?;

    Ok(OAuthCallbackResult { code, state })
}

/// Minimal percent-decoding for URL query values.
fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'+' {
            result.push(' ');
        } else if b == b'%' {
            let h1 = chars.next().unwrap_or(b'0');
            let h2 = chars.next().unwrap_or(b'0');
            let hex = [h1, h2];
            if let Ok(decoded) = u8::from_str_radix(std::str::from_utf8(&hex).unwrap_or("00"), 16) {
                result.push(decoded as char);
            }
        } else {
            result.push(b as char);
        }
    }
    result
}

// ── Browser opener ──────────────────────────────────────────────────

/// Best-effort, non-blocking browser opener (platform-specific).
pub fn open_url_in_browser(url: &str) {
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

// ── Secure file I/O ─────────────────────────────────────────────────

/// Write content to a file with owner-only permissions (0o600 on Unix).
///
/// Uses `spawn_blocking` to avoid blocking the async runtime.
/// Returns an error if the directory cannot be created, the file cannot
/// be written, or the blocking task panics.
pub async fn write_file_secure(path: &Path, content: &str) -> Result<()> {
    let path = path.to_path_buf();
    let content = content.to_string();

    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        #[cfg(unix)]
        {
            use std::fs::Permissions;
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(content.as_bytes())?;
            std::fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        }

        #[cfg(not(unix))]
        {
            std::fs::write(&path, &content)?;
        }

        Ok(())
    })
    .await
    .context("credential file write task panicked")?
    .context("failed to write credential file")?;

    Ok(())
}

/// OAuth credentials stored to / loaded from disk.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthCredentials {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Expiry as RFC3339 string (e.g. "2025-12-31T23:59:59Z").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
    /// Optional project ID (used by Google providers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// Load OAuth credentials from a JSON file.
pub fn load_oauth_credentials(path: &Path) -> Result<OAuthCredentials> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read credentials from {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse credentials from {}", path.display()))
}

/// Check if an OAuth token has expired (with optional buffer seconds).
pub fn is_token_expired(expiry: &str, buffer_secs: i64) -> bool {
    if let Ok(expiry_time) = chrono::DateTime::parse_from_rfc3339(expiry) {
        let now = chrono::Utc::now();
        let buffer = chrono::Duration::seconds(buffer_secs);
        expiry_time < now + buffer
    } else {
        // If we can't parse the expiry, assume expired for safety
        true
    }
}

// ── Base64 credential decode ────────────────────────────────────────

/// Decode a base64-encoded credential string (standard alphabet, with padding).
///
/// Used to store public OAuth client IDs/secrets as base64 in source code
/// to avoid triggering GitHub push-protection false positives.
pub fn decode_b64_credential(b64: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    STANDARD
        .decode(b64)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

// ── Remote mode helper ──────────────────────────────────────────────

/// For headless/remote environments: prompt the user to manually paste
/// the redirect URL from their browser.
pub fn prompt_paste_redirect_url() -> Result<OAuthCallbackResult> {
    println!();
    println!("  Paste the full redirect URL from your browser:");
    println!("  (It should start with http://localhost:...)");
    println!();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim();

    // Parse the URL to extract code and state
    let query = input
        .split('?')
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("invalid redirect URL — missing query parameters"))?;

    let mut code = None;
    let mut state = None;

    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or_default();
        let value = kv.next().unwrap_or_default();
        match key {
            "code" => code = Some(url_decode(value)),
            "state" => state = Some(url_decode(value)),
            _ => {}
        }
    }

    let code = code.ok_or_else(|| anyhow::anyhow!("redirect URL missing 'code' parameter"))?;
    Ok(OAuthCallbackResult { code, state })
}

/// Detect if we are likely running in a headless (no-display) environment.
pub fn is_headless() -> bool {
    // Check common environment indicators
    if std::env::var("DISPLAY").is_err() && std::env::var("WAYLAND_DISPLAY").is_err() {
        // Unix without display server
        #[cfg(unix)]
        return !cfg!(target_os = "macos"); // macOS always has a display
    }
    if std::env::var("SSH_TTY").is_ok() || std::env::var("SSH_CONNECTION").is_ok() {
        return true;
    }
    false
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
        // S256: challenge should be base64url(SHA256(verifier))
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
    fn url_decode_handles_percent_encoding() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("no%2Fslash"), "no/slash");
    }

    #[test]
    fn is_token_expired_checks_correctly() {
        let future = "2099-12-31T23:59:59Z";
        assert!(!is_token_expired(future, 0));

        let past = "2020-01-01T00:00:00Z";
        assert!(is_token_expired(past, 0));

        // Invalid format → treated as expired
        assert!(is_token_expired("not-a-date", 0));
    }

    #[test]
    fn oauth_credentials_round_trip() {
        let creds = OAuthCredentials {
            access_token: "test_token".to_string(),
            refresh_token: Some("refresh".to_string()),
            expiry: Some("2099-12-31T23:59:59Z".to_string()),
            project_id: None,
        };
        let json = serde_json::to_string(&creds).unwrap();
        let parsed: OAuthCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.access_token, "test_token");
        assert_eq!(parsed.refresh_token.as_deref(), Some("refresh"));
    }
}
