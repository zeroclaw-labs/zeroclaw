//! JSON API handlers for user authentication (Tauri/web client).
//!
//! Endpoints:
//! - POST /api/auth/register
//! - POST /api/auth/login
//! - POST /api/auth/logout
//! - GET  /api/auth/devices
//! - POST /api/auth/devices
//! - PUT  /api/auth/devices/{device_id}/pairing-code
//! - POST /api/auth/devices/{device_id}/verify-pairing
//! - POST /api/auth/heartbeat
//! - GET  /api/agent/info

use super::AppState;
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

// ── Helpers ──────────────────────────────────────────────────────

type AuthError = (StatusCode, Json<serde_json::Value>);

/// Extract user_id from a Bearer session token.
fn extract_session_user(state: &AppState, headers: &HeaderMap) -> Result<String, AuthError> {
    let auth_store = state.auth_store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "Auth not configured" })),
        )
    })?;

    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|a| a.strip_prefix("Bearer "))
        .unwrap_or("");

    let session = auth_store.validate_session(token).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "Invalid or expired session" })),
        )
    })?;

    Ok(session.user_id)
}

// ── POST /api/auth/register ─────────────────────────────────────

#[derive(Deserialize)]
pub struct RegisterBody {
    pub username: String,
    pub password: String,
}

pub async fn handle_auth_register(
    State(state): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Auth not configured" })),
            )
                .into_response();
        }
    };

    if !state.auth_allow_registration {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Registration is currently disabled" })),
        )
            .into_response();
    }

    match auth_store.register(&body.username, &body.password) {
        Ok(user_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "status": "ok",
                "user_id": user_id,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── POST /api/auth/login ────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
}

pub async fn handle_auth_login(
    State(state): State<AppState>,
    Json(body): Json<LoginBody>,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Auth not configured" })),
            )
                .into_response();
        }
    };

    let user = match auth_store.authenticate(&body.username, &body.password) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    // Create session
    let token = match auth_store.create_session(
        &user.id,
        body.device_id.as_deref(),
        body.device_name.as_deref(),
    ) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Session error: {e}") })),
            )
                .into_response();
        }
    };

    // Register device if provided
    if let (Some(ref did), Some(ref dname)) = (&body.device_id, &body.device_name) {
        let _ = auth_store.register_device(&user.id, did, dname, None);
    }

    // List devices
    let devices = auth_store
        .list_devices_with_status(&user.id, 300)
        .unwrap_or_default()
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "device_id": d.device_id,
                "device_name": d.device_name,
                "platform": d.platform,
                "last_seen": d.last_seen,
                "is_online": d.is_online,
                "has_pairing_code": d.has_pairing_code,
            })
        })
        .collect::<Vec<_>>();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "token": token,
            "user_id": user.id,
            "username": user.username,
            "devices": devices,
        })),
    )
        .into_response()
}

// ── POST /api/auth/logout ───────────────────────────────────────

pub async fn handle_auth_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Auth not configured" })),
            )
                .into_response();
        }
    };

    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|a| a.strip_prefix("Bearer "))
        .unwrap_or("");

    let _ = auth_store.revoke_session(token);

    Json(serde_json::json!({ "status": "ok" })).into_response()
}

// ── GET /api/auth/devices ───────────────────────────────────────

pub async fn handle_auth_devices_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let user_id = match extract_session_user(&state, &headers) {
        Ok(id) => id,
        Err(resp) => return resp.into_response(),
    };

    let auth_store = state.auth_store.as_ref().unwrap();
    let devices = auth_store
        .list_devices_with_status(&user_id, 300)
        .unwrap_or_default()
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "device_id": d.device_id,
                "device_name": d.device_name,
                "platform": d.platform,
                "last_seen": d.last_seen,
                "is_online": d.is_online,
                "has_pairing_code": d.has_pairing_code,
            })
        })
        .collect::<Vec<_>>();

    Json(serde_json::json!({ "devices": devices })).into_response()
}

// ── POST /api/auth/devices ──────────────────────────────────────

#[derive(Deserialize)]
pub struct RegisterDeviceBody {
    pub device_id: String,
    pub device_name: String,
    pub platform: Option<String>,
}

pub async fn handle_auth_devices_register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterDeviceBody>,
) -> impl IntoResponse {
    let user_id = match extract_session_user(&state, &headers) {
        Ok(id) => id,
        Err(resp) => return resp.into_response(),
    };

    let auth_store = state.auth_store.as_ref().unwrap();
    match auth_store.register_device(&user_id, &body.device_id, &body.device_name, body.platform.as_deref()) {
        Ok(()) => Json(serde_json::json!({ "status": "ok" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── PUT /api/auth/devices/{device_id}/pairing-code ──────────────

#[derive(Deserialize)]
pub struct PairingCodeBody {
    pub pairing_code: Option<String>,
}

pub async fn handle_auth_device_set_pairing_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(body): Json<PairingCodeBody>,
) -> impl IntoResponse {
    let user_id = match extract_session_user(&state, &headers) {
        Ok(id) => id,
        Err(resp) => return resp.into_response(),
    };

    let auth_store = state.auth_store.as_ref().unwrap();
    match auth_store.set_device_pairing_code(&user_id, &device_id, body.pairing_code.as_deref()) {
        Ok(()) => Json(serde_json::json!({ "status": "ok" })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── POST /api/auth/devices/{device_id}/verify-pairing ───────────

#[derive(Deserialize)]
pub struct VerifyPairingBody {
    pub pairing_code: String,
}

pub async fn handle_auth_device_verify_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(body): Json<VerifyPairingBody>,
) -> impl IntoResponse {
    let _ = match extract_session_user(&state, &headers) {
        Ok(id) => id,
        Err(resp) => return resp.into_response(),
    };

    let auth_store = state.auth_store.as_ref().unwrap();
    match auth_store.verify_device_pairing_code(&device_id, &body.pairing_code) {
        Ok(verified) => Json(serde_json::json!({ "verified": verified })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── POST /api/auth/heartbeat ────────────────────────────────────

#[derive(Deserialize)]
pub struct HeartbeatBody {
    pub device_id: String,
}

pub async fn handle_auth_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<HeartbeatBody>,
) -> impl IntoResponse {
    let _ = match extract_session_user(&state, &headers) {
        Ok(id) => id,
        Err(resp) => return resp.into_response(),
    };

    let auth_store = state.auth_store.as_ref().unwrap();
    let _ = auth_store.touch_device(&body.device_id);

    Json(serde_json::json!({ "status": "ok" })).into_response()
}

// ── POST /api/auth/kakao/callback ────────────────────────────────

#[derive(Deserialize)]
pub struct KakaoCallbackBody {
    pub code: String,
}

pub async fn handle_auth_kakao_callback(
    State(state): State<AppState>,
    Json(body): Json<KakaoCallbackBody>,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Auth not configured" })),
            )
                .into_response();
        }
    };

    // Get Kakao REST API key from config
    let kakao_rest_key = match std::env::var("ZEROCLAW_KAKAO_REST_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
    {
        Some(k) => k,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Kakao OAuth not configured" })),
            )
                .into_response();
        }
    };

    // Exchange authorization code for access token
    let client = reqwest::Client::new();
    // Use ZEROCLAW_PUBLIC_URL env var if set, otherwise fall back to localhost
    let redirect_uri = match std::env::var("ZEROCLAW_PUBLIC_URL").ok().filter(|u| !u.is_empty()) {
        Some(base) => format!("{base}/api/auth/kakao/redirect"),
        None => {
            let port = state.config.lock().gateway.port;
            format!("http://localhost:{port}/api/auth/kakao/redirect")
        }
    };

    let token_resp = client
        .post("https://kauth.kakao.com/oauth/token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &kakao_rest_key),
            ("redirect_uri", &redirect_uri),
            ("code", &body.code),
        ])
        .send()
        .await;

    let token_resp = match token_resp {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Kakao token exchange failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "Failed to contact Kakao auth server" })),
            )
                .into_response();
        }
    };

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body_text = token_resp.text().await.unwrap_or_default();
        tracing::error!("Kakao token exchange error ({status}): {body_text}");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Kakao token exchange failed: {status}") })),
        )
            .into_response();
    }

    let token_data: serde_json::Value = match token_resp.json().await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Kakao token parse error: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "Invalid response from Kakao" })),
            )
                .into_response();
        }
    };

    let access_token = match token_data.get("access_token").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "No access token from Kakao" })),
            )
                .into_response();
        }
    };

    // Get user profile from Kakao
    let profile_resp = client
        .get("https://kapi.kakao.com/v2/user/me")
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await;

    let profile_resp = match profile_resp {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Kakao profile fetch failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "Failed to fetch Kakao profile" })),
            )
                .into_response();
        }
    };

    let profile_data: serde_json::Value = match profile_resp.json().await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Kakao profile parse error: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "Invalid Kakao profile response" })),
            )
                .into_response();
        }
    };

    let kakao_id = profile_data
        .get("id")
        .and_then(|v| v.as_i64())
        .map(|id| id.to_string())
        .unwrap_or_default();

    let nickname = profile_data
        .get("kakao_account")
        .and_then(|a| a.get("profile"))
        .and_then(|p| p.get("nickname"))
        .and_then(|n| n.as_str())
        .unwrap_or("kakao_user");

    if kakao_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Could not get Kakao user ID" })),
        )
            .into_response();
    }

    // Ensure channel_links table exists
    let _ = auth_store.ensure_channel_links_table();

    // Check if this Kakao ID is already linked to a user
    let existing_user = auth_store.find_channel_link("kakao", &kakao_id).ok().flatten();

    let (user_id, username) = if let Some(user) = existing_user {
        (user.id, user.username)
    } else {
        // Auto-register a new user with Kakao identity
        let auto_username = format!("kakao_{}", &kakao_id);
        // Generate a random password (user won't need it for Kakao login)
        let auto_password: String = (0..24)
            .map(|_| {
                let idx: u8 = rand::random::<u8>() % 62;
                match idx {
                    0..=9 => (b'0' + idx) as char,
                    10..=35 => (b'a' + idx - 10) as char,
                    _ => (b'A' + idx - 36) as char,
                }
            })
            .collect();

        match auth_store.register(&auto_username, &auto_password) {
            Ok(uid) => {
                let _ = auth_store.link_channel("kakao", &kakao_id, &uid);
                (uid, auto_username)
            }
            Err(e) => {
                // If username already exists (race condition), try to find the link again
                if e.to_string().contains("already taken") {
                    match auth_store.find_channel_link("kakao", &kakao_id).ok().flatten() {
                        Some(user) => (user.id, user.username),
                        None => {
                            return (
                                StatusCode::CONFLICT,
                                Json(serde_json::json!({ "error": "Account creation conflict. Please try again." })),
                            )
                                .into_response();
                        }
                    }
                } else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": format!("Registration failed: {e}") })),
                    )
                        .into_response();
                }
            }
        }
    };

    // Create session
    let session_token = match auth_store.create_session(&user_id, None, Some("kakao_web")) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Session error: {e}") })),
            )
                .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "token": session_token,
            "user_id": user_id,
            "username": username,
            "kakao_id": kakao_id,
            "kakao_nickname": nickname,
        })),
    )
        .into_response()
}

// ── GET /api/auth/kakao/redirect ────────────────────────────────
// Browser redirect endpoint — receives ?code=xxx from Kakao and redirects to SPA

pub async fn handle_auth_kakao_redirect(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let code = params.get("code").cloned().unwrap_or_default();
    // Redirect to the SPA Kakao callback route
    let redirect_url = format!("/auth/kakao/callback?code={}", urlencoding::encode(&code));
    axum::response::Redirect::temporary(&redirect_url).into_response()
}

// ── GET /api/agent/info ─────────────────────────────────────────

pub async fn handle_agent_info(State(state): State<AppState>) -> impl IntoResponse {
    let tools: Vec<serde_json::Value> = state
        .tools_registry
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
            })
        })
        .collect();

    let channels: Vec<String> = Vec::new();

    Json(serde_json::json!({
        "channels": channels,
        "tools": tools,
    }))
}
