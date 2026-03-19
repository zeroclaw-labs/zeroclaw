//! Remote device access via web chat.
//!
//! Allows users to access their MoA device (running on a phone/computer)
//! from any browser (e.g., a public computer) without installing anything.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────┐           ┌────────────────────────┐
//! │  Web Browser (public)│           │  MoA Device (phone)    │
//! │  /ws/remote          │◄──────────►  /ws/device-link       │
//! │  User types message  │   Device  │  Agent processes msg   │
//! │  Gets AI response    │   Router  │  Returns response      │
//! └──────────────────────┘           └────────────────────────┘
//! ```
//!
//! ## Flow
//!
//! 1. Web user navigates to MoA homepage, clicks "Remote Access"
//! 2. Enters: username + password → validates via AuthStore
//! 3. Selects target device from their registered devices
//! 4. Enters device pairing code → validates via AuthStore.verify_device_pairing_code
//! 5. WebSocket connection established at `/ws/remote`
//! 6. Messages are routed to the target device's agent via DeviceRouter
//! 7. Device agent processes message, response routed back to web client
//!
//! ## Security
//!
//! - Two-factor auth: account password + device pairing code
//! - Session tokens with TTL (30 days default)
//! - Device must be online (connected via `/ws/device-link`)
//! - All messages transit through the server but are ephemeral (not stored)
//! - Rate limiting on login attempts

use super::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        RawQuery, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// ── Constants ─────────────────────────────────────────────────────

/// Maximum pending messages per device connection.
const DEVICE_CHANNEL_BUFFER: usize = 256;

/// How long to wait for a device response before timing out.
const DEVICE_RESPONSE_TIMEOUT_SECS: u64 = 120;

/// Public alias for device relay timeout (used by ws.rs auto-relay).
pub const DEVICE_RESPONSE_TIMEOUT_SECS_PUB: u64 = DEVICE_RESPONSE_TIMEOUT_SECS;

/// Maximum login attempts per IP before lockout.
const MAX_LOGIN_ATTEMPTS: u32 = 10;

/// Lockout duration after max login attempts.
const LOGIN_LOCKOUT_SECS: u64 = 300;

/// Stale login-attempt record retention.
const LOGIN_ATTEMPT_RETENTION_SECS: u64 = 900;

/// Maximum tracked login IPs.
const MAX_TRACKED_LOGIN_IPS: usize = 10_000;

/// Device online threshold for last_seen check (seconds).
const DEVICE_ONLINE_THRESHOLD_SECS: u64 = 120;

// ── Types ─────────────────────────────────────────────────────────

/// A message routed between web client and device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutedMessage {
    /// Unique message ID for correlation.
    pub id: String,
    /// Direction: "to_device" or "to_web".
    pub direction: String,
    /// Message content (user prompt or agent response).
    pub content: String,
    /// Message type: "message", "chunk", "done", "error".
    #[serde(rename = "type")]
    pub msg_type: String,
}

/// Tracks a connected device's agent WebSocket.
struct DeviceConnection {
    /// Send messages TO the device agent.
    to_device_tx: mpsc::Sender<RoutedMessage>,
    /// Device metadata.
    device_name: String,
    /// When this connection was established.
    connected_at: Instant,
}

/// Login attempt tracking for brute-force protection.
#[derive(Debug)]
struct LoginAttemptState {
    failed_count: u32,
    first_failure: Instant,
    lockout_until: Option<Instant>,
}

/// Central router for cross-device message routing.
///
/// Maintains a registry of connected device agents and routes
/// messages between web clients and the appropriate device.
pub struct DeviceRouter {
    /// device_id -> DeviceConnection
    connections: Mutex<HashMap<String, DeviceConnection>>,
    /// Per-IP login attempt tracking.
    login_attempts: Mutex<(HashMap<String, LoginAttemptState>, Instant)>,
}

impl DeviceRouter {
    /// Create a new device router.
    pub fn new() -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            login_attempts: Mutex::new((HashMap::new(), Instant::now())),
        }
    }

    /// Register a device connection. Returns a receiver for messages
    /// TO the device.
    pub fn register_device(
        &self,
        device_id: &str,
        device_name: &str,
    ) -> mpsc::Receiver<RoutedMessage> {
        let (tx, rx) = mpsc::channel(DEVICE_CHANNEL_BUFFER);
        let mut conns = self.connections.lock();
        conns.insert(
            device_id.to_string(),
            DeviceConnection {
                to_device_tx: tx,
                device_name: device_name.to_string(),
                connected_at: Instant::now(),
            },
        );
        tracing::info!(
            device_id = device_id,
            device_name = device_name,
            "Device registered with router"
        );
        rx
    }

    /// Unregister a device connection.
    pub fn unregister_device(&self, device_id: &str) {
        let mut conns = self.connections.lock();
        if conns.remove(device_id).is_some() {
            tracing::info!(device_id = device_id, "Device unregistered from router");
        }
    }

    /// Check if a device is currently connected.
    pub fn is_device_online(&self, device_id: &str) -> bool {
        let conns = self.connections.lock();
        conns.contains_key(device_id)
    }

    /// Send a message to a connected device. Returns error if device is offline.
    pub async fn send_to_device(
        &self,
        device_id: &str,
        message: RoutedMessage,
    ) -> Result<(), String> {
        let tx = {
            let conns = self.connections.lock();
            match conns.get(device_id) {
                Some(conn) => conn.to_device_tx.clone(),
                None => return Err("Device is not connected".to_string()),
            }
        };
        tx.send(message)
            .await
            .map_err(|_| "Device connection channel closed".to_string())
    }

    /// List connected devices.
    pub fn list_connected_devices(&self) -> Vec<(String, String, Instant)> {
        let conns = self.connections.lock();
        conns
            .iter()
            .map(|(id, conn)| (id.clone(), conn.device_name.clone(), conn.connected_at))
            .collect()
    }

    /// Check login rate limit. Returns Ok(()) if allowed, Err(remaining_secs) if locked out.
    pub fn check_login_rate(&self, client_key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut guard = self.login_attempts.lock();
        let (attempts, last_sweep) = &mut *guard;

        // Periodic sweep of stale entries
        if last_sweep.elapsed() >= Duration::from_secs(LOGIN_ATTEMPT_RETENTION_SECS) {
            attempts.retain(|_, state| {
                if let Some(lockout) = state.lockout_until {
                    lockout > now
                } else {
                    state.first_failure.elapsed()
                        < Duration::from_secs(LOGIN_ATTEMPT_RETENTION_SECS)
                }
            });
            *last_sweep = now;
        }

        if let Some(state) = attempts.get(client_key) {
            if let Some(lockout) = state.lockout_until {
                if lockout > now {
                    let remaining = (lockout - now).as_secs().max(1);
                    return Err(remaining);
                }
            }
        }

        Ok(())
    }

    /// Record a failed login attempt.
    pub fn record_login_failure(&self, client_key: &str) {
        let now = Instant::now();
        let mut guard = self.login_attempts.lock();
        let (attempts, _) = &mut *guard;

        // Cap tracked IPs
        if !attempts.contains_key(client_key) && attempts.len() >= MAX_TRACKED_LOGIN_IPS {
            // Evict oldest entry
            let evict = attempts
                .iter()
                .min_by_key(|(_, state)| state.first_failure)
                .map(|(k, _)| k.clone());
            if let Some(evict_key) = evict {
                attempts.remove(&evict_key);
            }
        }

        let state = attempts.entry(client_key.to_string()).or_insert_with(|| {
            LoginAttemptState {
                failed_count: 0,
                first_failure: now,
                lockout_until: None,
            }
        });

        state.failed_count += 1;
        if state.failed_count >= MAX_LOGIN_ATTEMPTS {
            state.lockout_until = Some(now + Duration::from_secs(LOGIN_LOCKOUT_SECS));
        }
    }

    /// Clear login attempts on successful login.
    pub fn clear_login_attempts(&self, client_key: &str) {
        let mut guard = self.login_attempts.lock();
        guard.0.remove(client_key);
    }
}

// ── REST Endpoints ────────────────────────────────────────────────

/// POST /api/remote/login
///
/// Authenticate user and verify device pairing code.
///
/// Request body:
/// ```json
/// {
///   "username": "user_a",
///   "password": "securepassword123",
///   "device_id": "device_abc",
///   "pairing_code": "123456"
/// }
/// ```
///
/// Response:
/// ```json
/// {
///   "session_token": "hex_token",
///   "user_id": "uuid",
///   "device_id": "device_abc",
///   "device_name": "My Phone",
///   "device_online": true
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct RemoteLoginRequest {
    pub username: String,
    pub password: String,
    pub device_id: String,
    pub pairing_code: String,
}

#[derive(Debug, Serialize)]
pub struct RemoteLoginResponse {
    pub session_token: Option<String>,
    pub user_id: String,
    pub device_id: String,
    pub device_name: String,
    pub device_online: bool,
    /// When true, the client must call POST /api/remote/verify-email
    /// with the code sent to the user's registered email.
    #[serde(default)]
    pub requires_email_verification: bool,
    /// Masked email address hint (e.g. "u***@example.com") for display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_hint: Option<String>,
}

pub async fn handle_remote_login(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer_addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<RemoteLoginRequest>,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Authentication service is not enabled"
                })),
            )
                .into_response();
        }
    };

    let device_router = match state.device_router.as_ref() {
        Some(r) => r,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Remote device access is not enabled"
                })),
            )
                .into_response();
        }
    };

    // Rate limit check
    let client_key =
        super::client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if let Err(remaining) = device_router.check_login_rate(&client_key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "Too many login attempts. Please try again later.",
                "retry_after": remaining,
            })),
        )
            .into_response();
    }

    // 1. Authenticate user
    let user = match auth_store.authenticate(&body.username, &body.password) {
        Ok(u) => u,
        Err(_) => {
            device_router.record_login_failure(&client_key);
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Invalid username or password"
                })),
            )
                .into_response();
        }
    };

    // 2. Verify device belongs to user
    let devices = match auth_store.list_devices(&user.id) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Failed to list devices: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to verify device"
                })),
            )
                .into_response();
        }
    };

    let device = match devices.iter().find(|d| d.device_id == body.device_id) {
        Some(d) => d,
        None => {
            device_router.record_login_failure(&client_key);
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "Device not found or does not belong to this account"
                })),
            )
                .into_response();
        }
    };

    // 3. Verify device pairing code
    match auth_store.verify_device_pairing_code(&body.device_id, &body.pairing_code) {
        Ok(true) => {} // Pairing code valid
        Ok(false) => {
            device_router.record_login_failure(&client_key);
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Invalid device pairing code"
                })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("Failed to verify pairing code: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to verify pairing code"
                })),
            )
                .into_response();
        }
    }

    // 4. Check if email verification is required
    if let Some(ref email_svc) = state.email_verify_service {
        // Email verification is enabled — send code instead of granting session
        let email = match user.email.as_deref() {
            Some(e) if !e.trim().is_empty() => e.trim().to_string(),
            _ => {
                return (
                    StatusCode::PRECONDITION_FAILED,
                    Json(serde_json::json!({
                        "error": "이메일 주소가 등록되어 있지 않습니다. 설정에서 이메일을 등록해주세요. / No email address registered. Please set your email in Settings.",
                        "code": "NO_EMAIL"
                    })),
                )
                    .into_response();
            }
        };

        // Send verification code
        if let Err(e) = email_svc.send_verification_code(
            &user.id,
            &body.device_id,
            &email,
            &user.username,
        ) {
            tracing::error!("Failed to send email verification: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "인증코드 발송에 실패했습니다. 잠시 후 다시 시도해주세요. / Failed to send verification code. Please try again."
                })),
            )
                .into_response();
        }

        // Clear rate limit (credentials were valid)
        device_router.clear_login_attempts(&client_key);

        let device_online = device_router.is_device_online(&body.device_id);
        let email_hint = mask_email(&email);

        return Json(RemoteLoginResponse {
            session_token: None,
            user_id: user.id,
            device_id: body.device_id.clone(),
            device_name: device.device_name.clone(),
            device_online,
            requires_email_verification: true,
            email_hint: Some(email_hint),
        })
        .into_response();
    }

    // 4b. No email verification — create session token with 24h TTL for web access
    let session_token = match auth_store.create_session_with_ttl(
        &user.id,
        Some(&body.device_id),
        Some(&device.device_name),
        crate::auth::store::WEB_SESSION_TTL_SECS,
    ) {
        Ok(token) => token,
        Err(e) => {
            tracing::error!("Failed to create session: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to create session"
                })),
            )
                .into_response();
        }
    };

    // Clear rate limit on success
    device_router.clear_login_attempts(&client_key);

    // Check if device is online
    let device_online = device_router.is_device_online(&body.device_id);

    Json(RemoteLoginResponse {
        session_token: Some(session_token),
        user_id: user.id,
        device_id: body.device_id.clone(),
        device_name: device.device_name.clone(),
        device_online,
        requires_email_verification: false,
        email_hint: None,
    })
    .into_response()
}

/// GET /api/remote/devices
///
/// List user's devices with online status.
/// Requires session token in Authorization header.
pub async fn handle_remote_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Authentication service is not enabled"
                })),
            )
                .into_response();
        }
    };

    let device_router = match state.device_router.as_ref() {
        Some(r) => r,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Remote device access is not enabled"
                })),
            )
                .into_response();
        }
    };

    // Validate session token
    let token = extract_bearer_token(&headers).unwrap_or("");
    let session = match auth_store.validate_session(token) {
        Some(s) => s,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Invalid or expired session token"
                })),
            )
                .into_response();
        }
    };

    // List devices with status
    let devices = match auth_store.list_devices_with_status(
        &session.user_id,
        DEVICE_ONLINE_THRESHOLD_SECS,
    ) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Failed to list devices: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to list devices"
                })),
            )
                .into_response();
        }
    };

    // Augment with real-time online status from router
    let device_list: Vec<serde_json::Value> = devices
        .iter()
        .map(|d| {
            let router_online = device_router.is_device_online(&d.device_id);
            serde_json::json!({
                "device_id": d.device_id,
                "device_name": d.device_name,
                "platform": d.platform,
                "last_seen": d.last_seen,
                "is_online": router_online || d.is_online,
                "has_pairing_code": d.has_pairing_code,
            })
        })
        .collect();

    Json(serde_json::json!({
        "devices": device_list,
    }))
    .into_response()
}

/// POST /api/remote/logout
///
/// Revoke the current session token.
pub async fn handle_remote_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Authentication service is not enabled"
                })),
            )
                .into_response();
        }
    };

    let token = extract_bearer_token(&headers).unwrap_or("");
    if token.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "No session token provided"})),
        )
            .into_response();
    }

    match auth_store.revoke_session(token) {
        Ok(true) => Json(serde_json::json!({"status": "ok"})).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to revoke session: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Failed to revoke session"})),
            )
                .into_response()
        }
    }
}

// ── WebSocket: Remote client (web browser) ────────────────────────

/// GET /ws/remote — WebSocket for remote device access from web browser.
///
/// Query params:
/// - `token` — session token (from /api/remote/login)
/// - `device_id` — target device ID
///
/// Protocol:
/// ```text
/// Client -> Server: {"type":"message","content":"Hello"}
/// Server -> Client: {"type":"chunk","content":"partial..."}
/// Server -> Client: {"type":"done","full_response":"complete response"}
/// Server -> Client: {"type":"error","message":"..."}
/// Server -> Client: {"type":"device_status","online":true/false}
/// ```
pub async fn handle_ws_remote(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let query_params = parse_remote_query_params(query.as_deref());

    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Authentication service not enabled")
                .into_response();
        }
    };

    let device_router = match state.device_router.as_ref() {
        Some(r) => r.clone(),
        None => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Remote access not enabled").into_response();
        }
    };

    // Authenticate session
    let token = extract_ws_bearer_token(&headers, query_params.token.as_deref()).unwrap_or_default();
    let session = match auth_store.validate_session(&token) {
        Some(s) => s,
        None => {
            return (StatusCode::UNAUTHORIZED, "Invalid or expired session token").into_response();
        }
    };

    // Validate device_id
    let device_id = match query_params.device_id {
        Some(ref d) if !d.is_empty() => d.clone(),
        _ => {
            // If session has a device_id from login, use that
            match session.device_id {
                Some(ref d) => d.clone(),
                None => {
                    return (StatusCode::BAD_REQUEST, "Missing device_id parameter").into_response();
                }
            }
        }
    };

    // Verify device belongs to user
    let devices = match auth_store.list_devices(&session.user_id) {
        Ok(d) => d,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to verify device").into_response();
        }
    };

    if !devices.iter().any(|d| d.device_id == device_id) {
        return (StatusCode::FORBIDDEN, "Device does not belong to this account").into_response();
    }

    // Check device is online
    if !device_router.is_device_online(&device_id) {
        return (
            StatusCode::CONFLICT,
            "Device is not online. Please ensure MoA is running on the target device.",
        )
            .into_response();
    }

    ws.on_upgrade(move |socket| handle_remote_socket(socket, state, device_router, device_id))
        .into_response()
}

/// Handle the remote web client WebSocket connection.
///
/// Uses a channel-based approach to avoid Send issues with WebSocket split sinks.
/// An mpsc channel bridges the async boundary between the response relay task
/// and the WebSocket sender.
async fn handle_remote_socket(
    mut socket: WebSocket,
    _state: AppState,
    device_router: Arc<DeviceRouter>,
    device_id: String,
) {
    tracing::info!(
        device_id = %device_id,
        "Remote web client connected"
    );

    let remote_session_id = uuid::Uuid::new_v4().to_string();

    // Create a channel for responses from the device
    let (response_tx, mut response_rx) = mpsc::channel::<RoutedMessage>(DEVICE_CHANNEL_BUFFER);

    // Create a channel for outbound WebSocket messages (avoids Send issues with SplitSink)
    let (ws_out_tx, mut ws_out_rx) = mpsc::channel::<String>(DEVICE_CHANNEL_BUFFER);

    // Notify client that we're connected
    let status_msg = serde_json::json!({
        "type": "device_status",
        "online": true,
        "device_id": device_id,
    });
    let _ = socket
        .send(Message::Text(status_msg.to_string().into()))
        .await;

    // Spawn response relay task: device responses -> outbound channel
    let ws_out_tx_clone = ws_out_tx.clone();
    let response_relay = tokio::spawn(async move {
        while let Some(msg) = response_rx.recv().await {
            let json = serde_json::json!({
                "type": msg.msg_type,
                "content": msg.content,
                "full_response": if msg.msg_type == "done" { Some(&msg.content) } else { None },
                "message": if msg.msg_type == "error" { Some(&msg.content) } else { None },
            });
            if ws_out_tx_clone.send(json.to_string()).await.is_err() {
                break;
            }
        }
    });

    // Main loop: handle both inbound messages and outbound sends
    loop {
        tokio::select! {
            // Outbound: send queued messages to the web client
            Some(out_msg) = ws_out_rx.recv() => {
                if socket
                    .send(Message::Text(out_msg.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // Inbound: receive messages from the web client
            msg = socket.recv() => {
                let msg = match msg {
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    _ => continue,
                };

                let parsed: serde_json::Value = match serde_json::from_str(&msg) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let msg_type = parsed["type"].as_str().unwrap_or("");
                if msg_type != "message" {
                    continue;
                }

                let content = parsed["content"].as_str().unwrap_or("").to_string();
                if content.is_empty() {
                    continue;
                }

                let routed_msg = RoutedMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    direction: "to_device".to_string(),
                    content,
                    msg_type: "message".to_string(),
                };

                let msg_id = routed_msg.id.clone();

                // Register response channel for this message
                REMOTE_RESPONSE_CHANNELS
                    .lock()
                    .insert(msg_id.clone(), response_tx.clone());

                // Route to device
                if let Err(e) = device_router.send_to_device(&device_id, routed_msg).await {
                    REMOTE_RESPONSE_CHANNELS.lock().remove(&msg_id);

                    let err_json = serde_json::json!({
                        "type": "error",
                        "message": format!("Failed to reach device: {e}"),
                    });
                    let _ = socket
                        .send(Message::Text(err_json.to_string().into()))
                        .await;

                    if !device_router.is_device_online(&device_id) {
                        let offline_json = serde_json::json!({
                            "type": "device_status",
                            "online": false,
                            "device_id": device_id,
                        });
                        let _ = socket
                            .send(Message::Text(offline_json.to_string().into()))
                            .await;
                        break;
                    }
                }
            }
        }
    }

    // Cleanup
    response_relay.abort();
    REMOTE_RESPONSE_CHANNELS.lock().retain(|_, _| true); // Cleanup handled by Drop

    tracing::info!(
        device_id = %device_id,
        session_id = %remote_session_id,
        "Remote web client disconnected"
    );
}

// ── WebSocket: Device link (MoA app on phone/computer) ────────────

/// GET /ws/device-link — WebSocket for device agent to register with router.
///
/// Query params:
/// - `token` — session token (from device's auth session)
/// - `device_id` — this device's ID
///
/// Protocol:
/// ```text
/// Server -> Device: {"type":"remote_message","id":"msg-uuid","content":"user prompt"}
/// Device -> Server: {"type":"remote_response","id":"msg-uuid","content":"agent response","msg_type":"done"}
/// Device -> Server: {"type":"remote_chunk","id":"msg-uuid","content":"partial..."}
/// Device -> Server: {"type":"heartbeat"}
/// ```
pub async fn handle_ws_device_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let query_params = parse_remote_query_params(query.as_deref());

    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Authentication service not enabled")
                .into_response();
        }
    };

    let device_router = match state.device_router.as_ref() {
        Some(r) => r.clone(),
        None => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Remote access not enabled").into_response();
        }
    };

    // Authenticate
    let token = extract_ws_bearer_token(&headers, query_params.token.as_deref()).unwrap_or_default();
    let session = match auth_store.validate_session(&token) {
        Some(s) => s,
        None => {
            return (StatusCode::UNAUTHORIZED, "Invalid or expired session token").into_response();
        }
    };

    let device_id = match query_params.device_id {
        Some(ref d) if !d.is_empty() => d.clone(),
        _ => {
            return (StatusCode::BAD_REQUEST, "Missing device_id parameter").into_response();
        }
    };

    // Verify device belongs to user
    let devices = match auth_store.list_devices(&session.user_id) {
        Ok(d) => d,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to verify device").into_response();
        }
    };

    let device = match devices.iter().find(|d| d.device_id == device_id) {
        Some(d) => d.clone(),
        None => {
            return (StatusCode::FORBIDDEN, "Device does not belong to this account").into_response();
        }
    };

    // Update last_seen
    let _ = auth_store.touch_device(&device_id);

    let device_name = device.device_name.clone();

    ws.on_upgrade(move |socket| {
        handle_device_link_socket(socket, device_router, device_id, device_name)
    })
    .into_response()
}

/// Handle the device agent WebSocket connection.
async fn handle_device_link_socket(
    socket: WebSocket,
    device_router: Arc<DeviceRouter>,
    device_id: String,
    device_name: String,
) {
    use futures_util::{SinkExt, StreamExt};

    // Register this device with the router
    let mut from_web_rx = device_router.register_device(&device_id, &device_name);

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Spawn task: web messages -> device WebSocket
    let device_id_clone = device_id.clone();
    let forward_task = tokio::spawn(async move {
        while let Some(routed_msg) = from_web_rx.recv().await {
            // Preserve the original msg_type so the device can distinguish
            // between regular messages, check_key probes, and hybrid relay
            // messages (message_with_llm_key).
            let wire_type = match routed_msg.msg_type.as_str() {
                "check_key" => "check_key",
                "hybrid_relay" => "hybrid_relay",
                "channel_relay" => "channel_relay",
                _ => "remote_message",
            };
            let json = serde_json::json!({
                "type": wire_type,
                "id": routed_msg.id,
                "content": routed_msg.content,
                "msg_type": routed_msg.msg_type,
            });
            if ws_sender
                .send(Message::Text(json.to_string().into()))
                .await
                .is_err()
            {
                tracing::debug!(
                    device_id = %device_id_clone,
                    "Device link send error, closing"
                );
                break;
            }
        }
    });

    // Main loop: device responses -> route back to web client
    while let Some(msg) = ws_receiver.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t.to_string(),
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        let parsed: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = parsed["type"].as_str().unwrap_or("");

        match msg_type {
            "heartbeat" => {
                // Device keepalive — no action needed
            }
            "check_key_response" => {
                // Device responding to a check_key probe (has key / key missing).
                let msg_id = parsed["id"].as_str().unwrap_or("").to_string();
                let has_key = parsed["has_key"].as_bool().unwrap_or(false);
                let response_type = if has_key { "key_ok" } else { "key_missing" };

                let maybe_tx = {
                    let guard = REMOTE_RESPONSE_CHANNELS.lock();
                    guard.get(&msg_id).cloned()
                };

                if let Some(tx) = maybe_tx {
                    let routed_response = RoutedMessage {
                        id: msg_id.clone(),
                        direction: "to_web".to_string(),
                        content: String::new(),
                        msg_type: response_type.to_string(),
                    };
                    let _ = tx.send(routed_response).await;
                    REMOTE_RESPONSE_CHANNELS.lock().remove(&msg_id);
                }
            }
            "remote_response" | "remote_chunk" | "remote_error" => {
                let msg_id = parsed["id"].as_str().unwrap_or("").to_string();
                let content = parsed["content"].as_str().unwrap_or("").to_string();
                let response_type = match msg_type {
                    "remote_response" => "done",
                    "remote_chunk" => "chunk",
                    "remote_error" => "error",
                    _ => continue,
                };

                // Route response back to the web client.
                // Clone the sender and drop the guard before awaiting.
                let maybe_tx = {
                    let guard = REMOTE_RESPONSE_CHANNELS.lock();
                    guard.get(&msg_id).cloned()
                };

                if let Some(tx) = maybe_tx {
                    let routed_response = RoutedMessage {
                        id: msg_id.clone(),
                        direction: "to_web".to_string(),
                        content,
                        msg_type: response_type.to_string(),
                    };
                    let _ = tx.send(routed_response).await;

                    // Clean up response channel on "done" or "error"
                    if response_type == "done" || response_type == "error" {
                        REMOTE_RESPONSE_CHANNELS.lock().remove(&msg_id);
                    }
                }
            }
            _ => {
                tracing::debug!(
                    device_id = %device_id,
                    msg_type = msg_type,
                    "Unknown message type from device"
                );
            }
        }
    }

    // Cleanup
    forward_task.abort();
    device_router.unregister_device(&device_id);
    tracing::info!(
        device_id = %device_id,
        "Device link disconnected"
    );
}

// ── Global response channel registry ──────────────────────────────

/// Maps message IDs to response channels for routing device responses
/// back to the correct web client WebSocket.
///
/// This is a process-global registry because messages may cross
/// multiple async task boundaries.
pub static REMOTE_RESPONSE_CHANNELS: std::sync::LazyLock<
    Mutex<HashMap<String, mpsc::Sender<RoutedMessage>>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));


// ── Email Verification Endpoint ───────────────────────────────

/// POST /api/remote/verify-email
///
/// Verify the email code sent during remote login.
///
/// Request body:
/// ```json
/// {
///   "user_id": "uuid",
///   "code": "123456"
/// }
/// ```
///
/// Response (success):
/// ```json
/// {
///   "session_token": "hex_token",
///   "user_id": "uuid",
///   "device_id": "device_abc",
///   "device_name": "My Phone",
///   "device_online": true
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct EmailVerifyRequest {
    pub user_id: String,
    pub code: String,
}

pub async fn handle_remote_verify_email(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer_addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<EmailVerifyRequest>,
) -> impl IntoResponse {
    let email_svc = match state.email_verify_service.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Email verification is not enabled"
                })),
            )
                .into_response();
        }
    };

    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Authentication service is not enabled"
                })),
            )
                .into_response();
        }
    };

    let device_router = match state.device_router.as_ref() {
        Some(r) => r,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Remote device access is not enabled"
                })),
            )
                .into_response();
        }
    };

    // Rate limit check (reuse login rate limiter to prevent brute-force on OTP)
    let client_key =
        super::client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if let Err(remaining) = device_router.check_login_rate(&client_key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "Too many verification attempts. Please try again later.",
                "retry_after": remaining,
            })),
        )
            .into_response();
    }

    // Verify the email code
    let device_id = match email_svc.verify_code(&body.user_id, &body.code) {
        Ok(did) => did,
        Err(e) => {
            device_router.record_login_failure(&client_key);
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": e.to_string()
                })),
            )
                .into_response();
        }
    };

    // Clear rate limit on successful verification
    device_router.clear_login_attempts(&client_key);

    // Look up device info
    let devices = match auth_store.list_devices(&body.user_id) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Failed to list devices after email verify: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve device information"
                })),
            )
                .into_response();
        }
    };

    let device_name = devices
        .iter()
        .find(|d| d.device_id == device_id)
        .map(|d| d.device_name.clone())
        .unwrap_or_else(|| "Unknown".to_string());

    // Create session token with 24h TTL (email code verified = full 3-factor auth complete)
    let session_token = match auth_store.create_session_with_ttl(
        &body.user_id,
        Some(&device_id),
        Some(&device_name),
        crate::auth::store::WEB_SESSION_TTL_SECS,
    ) {
        Ok(token) => token,
        Err(e) => {
            tracing::error!("Failed to create session after email verify: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to create session"
                })),
            )
                .into_response();
        }
    };

    let device_online = device_router.is_device_online(&device_id);

    Json(serde_json::json!({
        "session_token": session_token,
        "user_id": body.user_id,
        "device_id": device_id,
        "device_name": device_name,
        "device_online": device_online,
    }))
    .into_response()
}

// ── Email Settings Endpoint ──────────────────────────────────

/// PUT /api/remote/email
///
/// Set or update the user's email address for verification.
/// Requires session token in Authorization header.
///
/// Request body:
/// ```json
/// {
///   "email": "user@example.com"
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct SetEmailRequest {
    pub email: String,
}

pub async fn handle_remote_set_email(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SetEmailRequest>,
) -> impl IntoResponse {
    let auth_store = match state.auth_store.as_ref() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Authentication service is not enabled"
                })),
            )
                .into_response();
        }
    };

    // Validate session token
    let token = extract_bearer_token(&headers).unwrap_or("");
    let session = match auth_store.validate_session(token) {
        Some(s) => s,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Invalid or expired session token"
                })),
            )
                .into_response();
        }
    };

    // Basic email format validation
    let email = body.email.trim();
    if email.is_empty() || !email.contains('@') || !email.contains('.') {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "유효한 이메일 주소를 입력해주세요. / Please enter a valid email address."
            })),
        )
            .into_response();
    }

    // Set email
    if let Err(e) = auth_store.set_user_email(&session.user_id, email) {
        tracing::error!("Failed to set user email: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to update email address"
            })),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "status": "ok",
        "email": mask_email(email),
    }))
    .into_response()
}

// ── Email Masking Helper ─────────────────────────────────────

/// Mask an email address for display (e.g., "user@example.com" → "u***@example.com").
fn mask_email(email: &str) -> String {
    let parts: Vec<&str> = email.splitn(2, '@').collect();
    if parts.len() != 2 {
        return "***".to_string();
    }
    let local = parts[0];
    let domain = parts[1];
    if local.len() <= 1 {
        format!("{}***@{}", local, domain)
    } else {
        let visible = &local[..1];
        format!("{}***@{}", visible, domain)
    }
}

// ── Query parameter parsing ───────────────────────────────────────

#[derive(Debug, Default)]
struct RemoteQueryParams {
    token: Option<String>,
    device_id: Option<String>,
}

fn parse_remote_query_params(raw_query: Option<&str>) -> RemoteQueryParams {
    let Some(query) = raw_query else {
        return RemoteQueryParams::default();
    };

    let mut params = RemoteQueryParams::default();
    for kv in query.split('&') {
        let mut parts = kv.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();
        if value.is_empty() {
            continue;
        }

        match key {
            "token" if params.token.is_none() => {
                params.token = Some(value.to_string());
            }
            "device_id" if params.device_id.is_none() => {
                params.device_id = Some(value.to_string());
            }
            _ => {}
        }
    }

    params
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
}

fn extract_ws_bearer_token(headers: &HeaderMap, query_token: Option<&str>) -> Option<String> {
    // Try Authorization header first
    if let Some(auth_header) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    {
        if let Some(token) = auth_header.strip_prefix("Bearer ") {
            if !token.trim().is_empty() {
                return Some(token.trim().to_string());
            }
        }
    }

    // Try Sec-WebSocket-Protocol header
    if let Some(offered) = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
    {
        for protocol in offered.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(token) = protocol.strip_prefix("bearer.") {
                if !token.trim().is_empty() {
                    return Some(token.trim().to_string());
                }
            }
        }
    }

    // Fallback to query param
    query_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
}
