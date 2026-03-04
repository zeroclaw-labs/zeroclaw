//! OAuth gateway routes for third-party service authentication.
//!
//! Provides browser-based OAuth connect flows for Google (Gmail, Calendar)
//! and DocuSign JWT credential storage. Tokens are stored in the ZeroClaw
//! workspace directory under `oauth/`.
//!
//! Routes:
//! - `GET  /auth/{service}`          — start OAuth flow (redirect to provider)
//! - `GET  /auth/{service}/callback` — exchange code, store tokens
//! - `GET  /auth/status`             — list connected services + expiry
//! - `DELETE /auth/{service}`        — revoke + delete tokens

use super::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Redirect, Response},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_SCOPES: &str = "https://www.googleapis.com/auth/gmail.modify \
    https://www.googleapis.com/auth/calendar \
    https://www.googleapis.com/auth/userinfo.email";

/// Token file stored on disk for a connected service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokenFile {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix timestamp when access_token expires.
    pub expires_at: Option<i64>,
    /// Authenticated user email (informational).
    pub email: Option<String>,
}

/// Query params for OAuth callback.
#[derive(Debug, Deserialize)]
pub struct OAuthCallback {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// Query params for initiating OAuth.
#[derive(Debug, Deserialize)]
pub struct OAuthStartQuery {
    /// Optional redirect URL after successful auth (must be same-origin).
    pub redirect: Option<String>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn oauth_dir(state: &AppState) -> PathBuf {
    let cfg = state.config.lock();
    cfg.config_path
        .parent()
        .map(|p| p.join("oauth"))
        .unwrap_or_else(|| PathBuf::from(".zeroclaw/oauth"))
}

fn token_path(dir: &PathBuf, service: &str) -> PathBuf {
    dir.join(format!("{service}.json"))
}

fn pkce_path(dir: &PathBuf, service: &str, state: &str) -> PathBuf {
    dir.join(format!("{service}_{state}_pkce.txt"))
}

async fn ensure_oauth_dir(dir: &PathBuf) -> anyhow::Result<()> {
    // On Unix, use DirBuilder::mode() so the directory is created with restrictive
    // permissions atomically, eliminating the create-then-chmod TOCTOU window.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking panicked creating oauth dir: {e}"))??;
    }
    #[cfg(not(unix))]
    tokio::fs::create_dir_all(dir).await?;
    Ok(())
}

async fn read_token(path: &PathBuf) -> Option<OAuthTokenFile> {
    let data = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&data).ok()
}

async fn write_token(path: &PathBuf, token: &OAuthTokenFile) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let json = serde_json::to_string_pretty(token)?;
    // Open with mode(0o600) at creation time to avoid the write-then-chmod TOCTOU
    // window.  On non-Unix platforms, fall back to a plain write.
    let mut opts = tokio::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).await?;
    file.write_all(json.as_bytes()).await?;
    Ok(())
}

fn callback_url(state: &AppState, service: &str) -> String {
    let cfg = state.config.lock();
    let host = &cfg.gateway.host;
    let port = cfg.gateway.port;
    // Use 127.0.0.1 for localhost variants to avoid redirect_uri mismatch
    let display_host = if host == "0.0.0.0" || host == "::" {
        "127.0.0.1"
    } else {
        host.as_str()
    };
    format!("http://{display_host}:{port}/auth/{service}/callback")
}

fn auth_check(state: &AppState, headers: &HeaderMap) -> bool {
    if !state.pairing.require_pairing() {
        return true;
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .unwrap_or("");
    state.pairing.is_authenticated(token)
}

/// Generate a PKCE code verifier (43-128 chars from unreserved chars).
fn pkce_verifier() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Derive PKCE code challenge from verifier (S256 method).
fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Shared HTTP client for all OAuth provider calls with a 30-second timeout.
fn oauth_http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build OAuth HTTP client: {e}"))
}

// ── Route handlers ────────────────────────────────────────────────────────────

/// GET /auth/{service} — initiate OAuth flow.
pub async fn handle_auth_start(
    State(state): State<AppState>,
    Path(service): Path<String>,
    headers: HeaderMap,
    Query(_query): Query<OAuthStartQuery>,
) -> Response {
    if !auth_check(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    match service.as_str() {
        "google" => start_google_oauth(&state).await,
        _ => (
            StatusCode::NOT_FOUND,
            format!("Unknown service: {service}. Supported: google"),
        )
            .into_response(),
    }
}

async fn start_google_oauth(state: &AppState) -> Response {
    let (client_id, client_secret) = {
        let cfg = state.config.lock();
        (
            cfg.oauth.google.client_id.clone(),
            cfg.oauth.google.client_secret.clone(),
        )
    };

    if client_id.is_empty() || client_secret.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Google OAuth not configured. Set [oauth.google] client_id and client_secret in config.toml",
        )
            .into_response();
    }

    let dir = oauth_dir(state);
    if let Err(e) = ensure_oauth_dir(&dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create oauth directory: {e}"),
        )
            .into_response();
    }

    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    // Generate a one-time CSRF state token; stored with the verifier so the
    // callback can validate it and reject replayed/forged callbacks.
    let state_token = pkce_verifier();

    // Store "state\nverifier" keyed by state in the filename so concurrent
    // /auth/google starts each get their own session file and cannot clobber
    // each other. Use mode(0o600) at creation time to avoid TOCTOU.
    let pkce_file = pkce_path(&dir, "google", &state_token);
    let pkce_payload = format!("{state_token}\n{verifier}");
    {
        use tokio::io::AsyncWriteExt as _;
        let mut opts = tokio::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        match opts.open(&pkce_file).await {
            Ok(mut f) => {
                if let Err(e) = f.write_all(pkce_payload.as_bytes()).await {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to store PKCE verifier: {e}"),
                    )
                        .into_response();
                }
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to store PKCE verifier: {e}"),
                )
                    .into_response();
            }
        }
    }

    let redirect_uri = callback_url(state, "google");
    let params = [
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("response_type", "code"),
        ("scope", GOOGLE_SCOPES),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state_token.as_str()),
    ];

    let url = format!(
        "{GOOGLE_AUTH_URL}?{}",
        params
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&")
    );

    Redirect::temporary(&url).into_response()
}

/// GET /auth/{service}/callback — handle OAuth provider callback.
pub async fn handle_auth_callback(
    State(state): State<AppState>,
    Path(service): Path<String>,
    Query(query): Query<OAuthCallback>,
) -> Response {
    match service.as_str() {
        "google" => handle_google_callback(&state, query).await,
        _ => (StatusCode::NOT_FOUND, format!("Unknown service: {service}")).into_response(),
    }
}

async fn handle_google_callback(state: &AppState, query: OAuthCallback) -> Response {
    if let Some(err) = query.error {
        return (
            StatusCode::BAD_REQUEST,
            Html(error_page("Google OAuth Error", &err)),
        )
            .into_response();
    }

    let code = match query.code {
        Some(c) => c,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Html(error_page(
                    "Missing code",
                    "No authorization code in callback",
                )),
            )
                .into_response()
        }
    };

    let dir = oauth_dir(state);
    // Validate state format before using it in the session filename to prevent
    // path traversal. Only accept alphanumeric + dash + underscore.
    let query_state = match query.state.as_deref() {
        Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => s,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Html(error_page("OAuth Error", "Missing or invalid OAuth state")),
            )
                .into_response()
        }
    };
    let pkce_file = pkce_path(&dir, "google", query_state);

    // Read the stored "state\nverifier" payload. Do NOT delete the file yet —
    // we must validate state first to prevent a forged callback from destroying
    // a legitimate in-flight OAuth session.
    let (expected_state, verifier) = match tokio::fs::read_to_string(&pkce_file).await {
        Ok(payload) => {
            let mut parts = payload.splitn(2, '\n');
            let st = parts.next().unwrap_or("").to_string();
            let vf = parts.next().unwrap_or("").to_string();
            (st, vf)
        }
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Html(error_page(
                    "OAuth Error",
                    "No pending OAuth session. Start the flow at /auth/google",
                )),
            )
                .into_response()
        }
    };

    // Validate state to prevent CSRF / callback-injection attacks.
    if query.state.as_deref() != Some(expected_state.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Html(error_page(
                "OAuth Error",
                "State mismatch — possible CSRF. Start the flow again at /auth/google",
            )),
        )
            .into_response();
    }

    // State validated — consume the PKCE session file to prevent replay.
    let _ = tokio::fs::remove_file(&pkce_file).await;

    let (client_id, client_secret) = {
        let cfg = state.config.lock();
        (
            cfg.oauth.google.client_id.clone(),
            cfg.oauth.google.client_secret.clone(),
        )
    };

    let redirect_uri = callback_url(state, "google");

    // Exchange code for tokens
    let client = match oauth_http_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("OAuth client init failed: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(error_page("Server Error", "Unexpected server error")),
            )
                .into_response();
        }
    };
    let resp = client
        .post(GOOGLE_TOKEN_URL)
        .form(&[
            ("code", code.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Html(error_page("Token Exchange Failed", &e.to_string())),
            )
                .into_response()
        }
    };

    let token_json: serde_json::Value = match resp.json().await {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Html(error_page("Token Parse Failed", &e.to_string())),
            )
                .into_response()
        }
    };

    if let Some(err) = token_json.get("error") {
        let msg = token_json
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return (
            StatusCode::BAD_GATEWAY,
            Html(error_page("Token Exchange Error", &format!("{err}: {msg}"))),
        )
            .into_response();
    }

    let access_token = match token_json.get("access_token").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None => {
            return (
                StatusCode::BAD_GATEWAY,
                Html(error_page("Missing Token", "No access_token in response")),
            )
                .into_response()
        }
    };

    let refresh_token = token_json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let expires_in = token_json
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);

    let expires_at = chrono::Utc::now().timestamp() + expires_in;

    // Fetch email to label the connection
    let email = fetch_google_email(&access_token).await;

    let token = OAuthTokenFile {
        access_token,
        refresh_token,
        expires_at: Some(expires_at),
        email: email.clone(),
    };

    if let Err(e) = ensure_oauth_dir(&dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(error_page("Storage Error", &e.to_string())),
        )
            .into_response();
    }

    let path = token_path(&dir, "google");
    if let Err(e) = write_token(&path, &token).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(error_page("Token Save Failed", &e.to_string())),
        )
            .into_response();
    }

    let display_email = email.as_deref().unwrap_or("unknown");
    Html(success_page(
        "Google Connected",
        &format!("Successfully connected Google account: {display_email}"),
    ))
    .into_response()
}

async fn fetch_google_email(access_token: &str) -> Option<String> {
    let client = oauth_http_client().ok()?;
    let resp = client
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .bearer_auth(access_token)
        .send()
        .await
        .ok()?;

    let info: serde_json::Value = resp.json().await.ok()?;
    info.get("email")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// GET /auth/status — list connected services.
pub async fn handle_auth_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !auth_check(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let dir = oauth_dir(&state);
    let now = chrono::Utc::now().timestamp();

    let mut services = Vec::new();

    // Check Google
    let google_path = token_path(&dir, "google");
    if let Some(token) = read_token(&google_path).await {
        let expired = token.expires_at.map(|e| e < now).unwrap_or(false);
        services.push(json!({
            "service": "google",
            "connected": true,
            "email": token.email,
            "expires_at": token.expires_at,
            "expired": expired,
        }));
    } else {
        services.push(json!({
            "service": "google",
            "connected": false,
        }));
    }

    Json(json!({ "services": services })).into_response()
}

/// DELETE /auth/{service} — revoke and delete tokens.
pub async fn handle_auth_revoke(
    State(state): State<AppState>,
    Path(service): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !auth_check(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    // Allowlist service names before constructing any filesystem path.
    let service = match service.as_str() {
        "google" => "google",
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Unknown service: {service}") })),
            )
                .into_response()
        }
    };

    let dir = oauth_dir(&state);
    let path = token_path(&dir, service);

    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("{service} is not connected") })),
        )
            .into_response();
    }

    // Revoke Google token if applicable
    if service == "google" {
        if let Some(token) = read_token(&path).await {
            let _ = revoke_google_token(&token.access_token).await;
        }
    }

    match tokio::fs::remove_file(&path).await {
        Ok(_) => Json(json!({ "success": true, "service": service })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to delete token: {e}") })),
        )
            .into_response(),
    }
}

async fn revoke_google_token(access_token: &str) -> anyhow::Result<()> {
    let client = oauth_http_client()?;
    let resp = client
        .post("https://oauth2.googleapis.com/revoke")
        .form(&[("token", access_token)])
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        tracing::warn!("Google token revocation returned non-success status: {status}");
    }
    Ok(())
}

/// Refresh Google access token using refresh_token, update file, return new access_token.
pub async fn refresh_google_token(
    dir: &PathBuf,
    client_id: &str,
    client_secret: &str,
) -> anyhow::Result<String> {
    let path = token_path(dir, "google");
    let mut token = read_token(&path)
        .await
        .ok_or_else(|| anyhow::anyhow!("Google not connected. Visit /auth/google to connect."))?;

    let refresh = token
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("No refresh_token stored. Reconnect at /auth/google"))?;

    let client = oauth_http_client()?;
    let resp = client
        .post(GOOGLE_TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if let Some(err) = resp.get("error") {
        anyhow::bail!("Token refresh failed: {err}");
    }

    let new_token = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No access_token in refresh response"))?
        .to_string();

    let expires_in = resp
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);

    token.access_token = new_token.clone();
    token.expires_at = Some(chrono::Utc::now().timestamp() + expires_in);

    write_token(&path, &token).await?;

    Ok(new_token)
}

/// Get a valid Google access token, refreshing if needed. Returns error if not connected.
pub async fn get_google_token(
    oauth_dir: &PathBuf,
    client_id: &str,
    client_secret: &str,
) -> anyhow::Result<String> {
    let path = token_path(oauth_dir, "google");
    let token = read_token(&path)
        .await
        .ok_or_else(|| anyhow::anyhow!("Google not connected. Visit /auth/google to connect."))?;

    let now = chrono::Utc::now().timestamp();
    // Refresh 60 seconds before expiry
    let needs_refresh = token.expires_at.map(|exp| exp - now < 60).unwrap_or(false);

    if needs_refresh && token.refresh_token.is_some() {
        refresh_google_token(oauth_dir, client_id, client_secret).await
    } else if needs_refresh {
        anyhow::bail!(
            "Google OAuth token expired and no refresh token available. Re-authenticate at /auth/google"
        )
    } else {
        Ok(token.access_token)
    }
}

// ── HTML helpers ──────────────────────────────────────────────────────────────

struct Html(String);

impl IntoResponse for Html {
    fn into_response(self) -> Response {
        ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], self.0).into_response()
    }
}

fn success_page(title: &str, message: &str) -> String {
    let title_escaped = html_escape(title);
    let message_escaped = html_escape(message);
    format!(
        r#"<!DOCTYPE html><html><head><title>{title_escaped}</title></head><body>
<h2>✅ {title_escaped}</h2><p>{message_escaped}</p>
<p><a href="/auth/status">View connected services</a></p>
</body></html>"#
    )
}

fn error_page(title: &str, detail: &str) -> String {
    let title_escaped = html_escape(title);
    let detail_escaped = html_escape(detail);
    format!(
        r#"<!DOCTYPE html><html><head><title>Error: {title_escaped}</title></head><body>
<h2>❌ {title_escaped}</h2><pre>{detail_escaped}</pre>
</body></html>"#
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_is_url_safe() {
        let v = pkce_verifier();
        assert!(v.len() >= 43);
        assert!(v
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn pkce_challenge_is_deterministic_for_same_verifier() {
        let v = pkce_verifier();
        let c1 = pkce_challenge(&v);
        let c2 = pkce_challenge(&v);
        assert_eq!(c1, c2);
    }

    #[test]
    fn html_escape_sanitizes_special_chars() {
        let escaped = html_escape("<script>&\"</script>");
        // Raw angle-brackets, quotes must not appear as literal characters.
        assert!(!escaped.contains('<'));
        assert!(!escaped.contains('>'));
        assert!(!escaped.contains('"'));
        // The original `&` is replaced with `&amp;`; other entities also use `&`.
        assert!(escaped.contains("&amp;"));
        assert!(escaped.contains("&lt;"));
        assert!(escaped.contains("&gt;"));
    }
}
