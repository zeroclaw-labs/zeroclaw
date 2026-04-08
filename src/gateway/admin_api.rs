//! Admin dashboard API endpoints.
//!
//! Provides user management, usage analytics, and real-time monitoring
//! for the MoA operator (admin).
//!
//! ## Authentication
//!
//! Admin endpoints use separate credentials (admin username/password)
//! stored in the `admins` table. Default: admin/admin (must change).
//!
//! ## Endpoints
//!
//! - POST /api/admin/login — Admin login
//! - GET  /api/admin/users — List all users with device/online status
//! - GET  /api/admin/usage — Usage analytics (category breakdown)
//! - GET  /api/admin/usage/users — Per-user usage breakdown
//! - GET  /api/admin/sessions — Active sessions
//! - GET  /api/admin/overview — Dashboard overview (all data in one call)

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::AppState;

/// Online threshold: device is "online" if last_seen within this window.
const DEVICE_ONLINE_THRESHOLD_SECS: u64 = 120;
/// Default analytics window: 30 days.
const DEFAULT_ANALYTICS_WINDOW_SECS: u64 = 30 * 86400;

/// Build the admin API router.
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/login", post(handle_admin_login))
        .route("/change-password", post(handle_admin_change_password))
        .route("/users", get(handle_admin_users))
        .route("/usage", get(handle_admin_usage))
        .route("/usage/users", get(handle_admin_usage_users))
        .route("/sessions", get(handle_admin_sessions))
        .route("/overview", get(handle_admin_overview))
}

// ── Auth ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AdminLoginBody {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct AdminLoginResponse {
    token: String,
    message: String,
}

/// POST /api/admin/login
async fn handle_admin_login(
    State(state): State<AppState>,
    Json(body): Json<AdminLoginBody>,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "Auth store not available"})),
            )
        }
    };

    match auth_store.authenticate_admin(&body.username, &body.password) {
        Ok(true) => {
            // Create a session token for admin (24h TTL)
            // We use a special "admin" user_id prefix to distinguish admin sessions.
            let token = auth_store
                .create_session_with_ttl("admin", None, Some("admin-dashboard"), 86400)
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "token": token,
                    "message": "관리자 로그인 성공"
                })),
            )
        }
        Ok(false) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "관리자 아이디 또는 비밀번호가 올바르지 않습니다."})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("인증 처리 중 문제가 발생했습니다: {e}")})),
        ),
    }
}

/// POST /api/admin/dashboard/change-password
async fn handle_admin_change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_admin_auth(&state, &headers) {
        return e;
    }
    let auth_store = state.auth_store.as_ref().unwrap();
    let new_password = body["new_password"].as_str().unwrap_or("");
    if new_password.len() < 4 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "비밀번호는 4자 이상이어야 합니다."})),
        );
    }
    match auth_store.change_admin_password("admin", new_password) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"message": "관리자 비밀번호가 변경되었습니다."})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("비밀번호 변경에 실패했습니다: {e}")})),
        ),
    }
}

/// Extract admin bearer token from headers and validate.
fn require_admin_auth(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth_header.strip_prefix("Bearer ").unwrap_or("");

    if token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "관리자 인증이 필요합니다."})),
        ));
    }

    let auth_store = state.auth_store.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": "Auth store not available"})),
    ))?;

    match auth_store.validate_session(token) {
        Some(session) if session.device_name.as_deref() == Some("admin-dashboard") => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "관리자 세션이 만료되었습니다. 다시 로그인해 주세요."})),
        )),
    }
}

// ── Users ───────────────────────────────────────────────────────────

/// GET /api/admin/users — List all users with device & online status.
async fn handle_admin_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_admin_auth(&state, &headers) {
        return e;
    }

    let auth_store = state.auth_store.as_ref().unwrap();
    match auth_store.list_all_users(DEVICE_ONLINE_THRESHOLD_SECS) {
        Ok(users) => (StatusCode::OK, Json(serde_json::to_value(users).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e}")})),
        ),
    }
}

// ── Usage Analytics ─────────────────────────────────────────────────

/// GET /api/admin/usage — Aggregate usage stats by category.
async fn handle_admin_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_admin_auth(&state, &headers) {
        return e;
    }

    let auth_store = state.auth_store.as_ref().unwrap();
    match auth_store.usage_summary(DEFAULT_ANALYTICS_WINDOW_SECS) {
        Ok(stats) => (StatusCode::OK, Json(serde_json::to_value(stats).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e}")})),
        ),
    }
}

/// GET /api/admin/usage/users — Per-user usage breakdown.
async fn handle_admin_usage_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_admin_auth(&state, &headers) {
        return e;
    }

    let auth_store = state.auth_store.as_ref().unwrap();
    match auth_store.per_user_usage(DEFAULT_ANALYTICS_WINDOW_SECS) {
        Ok(stats) => (StatusCode::OK, Json(serde_json::to_value(stats).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e}")})),
        ),
    }
}

// ── Sessions ────────────────────────────────────────────────────────

/// GET /api/admin/sessions — Active sessions.
async fn handle_admin_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_admin_auth(&state, &headers) {
        return e;
    }

    let auth_store = state.auth_store.as_ref().unwrap();
    match auth_store.active_sessions() {
        Ok(sessions) => (StatusCode::OK, Json(serde_json::to_value(sessions).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e}")})),
        ),
    }
}

// ── Overview (all-in-one) ───────────────────────────────────────────

/// GET /api/admin/overview — Dashboard overview (single call for the whole dashboard).
async fn handle_admin_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_admin_auth(&state, &headers) {
        return e;
    }

    let auth_store = state.auth_store.as_ref().unwrap();

    let users = auth_store
        .list_all_users(DEVICE_ONLINE_THRESHOLD_SECS)
        .unwrap_or_default();
    let usage = auth_store
        .usage_summary(DEFAULT_ANALYTICS_WINDOW_SECS)
        .unwrap_or_default();
    let user_usage = auth_store
        .per_user_usage(DEFAULT_ANALYTICS_WINDOW_SECS)
        .unwrap_or_default();
    let sessions = auth_store.active_sessions().unwrap_or_default();

    let total_users = users.len();
    let online_users = users.iter().filter(|u| u.online_device_count > 0).count();
    let total_devices: i64 = users.iter().map(|u| u.device_count).sum();
    let online_devices: i64 = users.iter().map(|u| u.online_device_count).sum();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "summary": {
                "total_users": total_users,
                "online_users": online_users,
                "total_devices": total_devices,
                "online_devices": online_devices,
                "active_sessions": sessions.len(),
            },
            "users": users,
            "usage_by_category": usage,
            "usage_by_user": user_usage,
            "active_sessions": sessions,
        })),
    )
}
