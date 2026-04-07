//! JSON API handlers for user authentication (Tauri/web client).
//!
//! Endpoints:
//! - POST /api/auth/register
//! - POST /api/auth/login
//! - POST /api/auth/logout
//! - GET  /api/auth/devices
//! - POST /api/auth/devices
//! - DELETE /api/auth/devices/{device_id}
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
    pub fingerprint: Option<String>,
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

    // Register device if provided; fingerprint-based dedup may return an existing device_id
    let resolved_device_id = if let (Some(ref did), Some(ref dname)) =
        (&body.device_id, &body.device_name)
    {
        match auth_store.register_device(&user.id, did, dname, None, body.fingerprint.as_deref()) {
            Ok(actual_id) => Some(actual_id),
            Err(_) => Some(did.clone()),
        }
    } else {
        None
    };

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

    let mut response = serde_json::json!({
        "status": "ok",
        "token": token,
        "user_id": user.id,
        "username": user.username,
        "devices": devices,
    });

    // If the server resolved a different device_id (fingerprint match), inform the client
    if let Some(ref actual_id) = resolved_device_id {
        if body.device_id.as_deref() != Some(actual_id.as_str()) {
            response["resolved_device_id"] = serde_json::json!(actual_id);
        }
    }

    (StatusCode::OK, Json(response)).into_response()
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

    let Some(auth_store) = state.auth_store.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Auth not configured"})),
        )
            .into_response();
    };
    // Auto-cleanup: remove devices offline > 30 days to prevent stale records accumulating.
    const STALE_DEVICE_THRESHOLD_SECS: u64 = 30 * 24 * 60 * 60;
    let _ = auth_store.cleanup_stale_devices(&user_id, STALE_DEVICE_THRESHOLD_SECS);

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

// ── DELETE /api/auth/devices/{device_id} ────────────────────────

pub async fn handle_auth_device_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> impl IntoResponse {
    let user_id = match extract_session_user(&state, &headers) {
        Ok(id) => id,
        Err(resp) => return resp.into_response(),
    };

    let Some(auth_store) = state.auth_store.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Auth not configured"})),
        )
            .into_response();
    };

    match auth_store.remove_device(&user_id, &device_id) {
        Ok(true) => {
            tracing::info!("Device {device_id} removed by user {user_id}");
            Json(serde_json::json!({"status": "ok", "deleted": true})).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Device not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to remove device {device_id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Failed to remove device"})),
            )
                .into_response()
        }
    }
}

// ── POST /api/auth/devices ──────────────────────────────────────

#[derive(Deserialize)]
pub struct RegisterDeviceBody {
    pub device_id: String,
    pub device_name: String,
    pub platform: Option<String>,
    pub fingerprint: Option<String>,
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

    let Some(auth_store) = state.auth_store.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Auth not configured"})),
        )
            .into_response();
    };
    match auth_store.register_device(
        &user_id,
        &body.device_id,
        &body.device_name,
        body.platform.as_deref(),
        body.fingerprint.as_deref(),
    ) {
        Ok(actual_device_id) => {
            let mut resp = serde_json::json!({ "status": "ok" });
            // If fingerprint matched an existing device, return the resolved device_id
            if actual_device_id != body.device_id {
                resp["resolved_device_id"] = serde_json::json!(actual_device_id);
            }
            Json(resp).into_response()
        }
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

    let Some(auth_store) = state.auth_store.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Auth not configured"})),
        )
            .into_response();
    };
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

    let Some(auth_store) = state.auth_store.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Auth not configured"})),
        )
            .into_response();
    };
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
    /// Optional fingerprint for device identity verification.
    /// If provided and the server finds an existing device with this fingerprint,
    /// the heartbeat is applied to that device (prevents stale device_id references).
    pub fingerprint: Option<String>,
}

pub async fn handle_auth_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<HeartbeatBody>,
) -> impl IntoResponse {
    let user_id = match extract_session_user(&state, &headers) {
        Ok(id) => id,
        Err(resp) => return resp.into_response(),
    };

    let Some(auth_store) = state.auth_store.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Auth not configured"})),
        )
            .into_response();
    };

    // Resolve actual device via fingerprint if available (handles reinstall scenarios)
    let actual_device_id = if let Some(ref fp) = body.fingerprint {
        auth_store
            .find_device_by_fingerprint(&user_id, fp)
            .unwrap_or_else(|| body.device_id.clone())
    } else {
        body.device_id.clone()
    };

    if let Err(err) = auth_store.touch_device(&actual_device_id) {
        tracing::warn!("Heartbeat: failed to touch device {actual_device_id}: {err}");
    }

    let mut resp = serde_json::json!({ "status": "ok" });
    if actual_device_id != body.device_id {
        resp["resolved_device_id"] = serde_json::json!(actual_device_id);
    }
    Json(resp).into_response()
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

    // Get Kakao REST API key from config (support both prefixed and plain env var names)
    let kakao_rest_key = match std::env::var("ZEROCLAW_KAKAO_REST_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| {
            std::env::var("KAKAO_REST_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
        }) {
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
    let redirect_uri = match std::env::var("ZEROCLAW_PUBLIC_URL")
        .ok()
        .filter(|u| !u.is_empty())
    {
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
    let existing_user = auth_store
        .find_channel_link("kakao", &kakao_id)
        .ok()
        .flatten();

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
                let _ = auth_store.link_channel("kakao", &kakao_id, &uid, None);
                (uid, auto_username)
            }
            Err(e) => {
                // If username already exists (race condition), try to find the link again
                if e.to_string().contains("already taken") {
                    match auth_store
                        .find_channel_link("kakao", &kakao_id)
                        .ok()
                        .flatten()
                    {
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

    // Auto-pair the KakaoTalk channel: mark this kakao_id as paired so the
    // user can immediately send/receive messages via KakaoTalk without manual
    // allowlist configuration. This enables the "login-only" flow for regular users.
    if let Some(ref pairing_store) = state.channel_pairing {
        if !pairing_store.is_paired("kakao", &kakao_id) {
            pairing_store.mark_paired("kakao", &kakao_id, &user_id);
            // Also persist to config allowlist so pairing survives restart
            let uid_clone = kakao_id.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) =
                    crate::channels::pairing::persist_channel_allowlist("kakao", &uid_clone)
                {
                    tracing::warn!("Kakao auto-pair: failed to persist allowlist: {e}");
                }
            });
            tracing::info!(
                "Kakao auto-pair: paired kakao_id={} to user={}",
                kakao_id,
                username
            );
        }
    }

    // Sync user to Supabase (cloud user management + credits).
    // This runs best-effort: local auth is the source of truth.
    let mut supabase_credits: Option<i32> = None;
    if let Some(ref sb) = state.supabase {
        match sb.get_or_create_user(&kakao_id).await {
            Ok(sb_user) => {
                supabase_credits = Some(sb_user.credits);
                let _ = sb
                    .log_audit(&crate::integrations::supabase::AuditLogEntry {
                        user_id: kakao_id.clone(),
                        action: "kakao_login".into(),
                        details: serde_json::json!({
                            "username": username,
                            "nickname": nickname,
                        }),
                        result: "success".into(),
                    })
                    .await;
            }
            Err(e) => {
                tracing::warn!("Supabase user sync failed (non-fatal): {e}");
            }
        }
    }

    let mut response = serde_json::json!({
        "status": "ok",
        "token": session_token,
        "user_id": user_id,
        "username": username,
        "kakao_id": kakao_id,
        "kakao_nickname": nickname,
    });

    if let Some(credits) = supabase_credits {
        response["credits"] = serde_json::json!(credits);
    }

    (StatusCode::OK, Json(response)).into_response()
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

    let config = state.config.lock().clone();
    let channels: Vec<serde_json::Value> = config
        .channels_config
        .all_channel_status()
        .into_iter()
        .map(|(name, enabled)| {
            serde_json::json!({
                "name": name,
                "enabled": enabled,
            })
        })
        .collect();

    // Also return active channel names for backwards compatibility
    let active_channels: Vec<String> = config
        .channels_config
        .all_channel_status()
        .into_iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(name, _)| name.to_string())
        .collect();

    Json(serde_json::json!({
        "channels": active_channels,
        "channels_detail": channels,
        "tools": tools,
    }))
}
