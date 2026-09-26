//! Device management and pairing API handlers.

use super::AppState;
use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;
use std::net::SocketAddr;

pub use zeroclaw_runtime::devices::{DeviceInfo, DeviceRegistry};

/// Store for pending pairing requests.
#[derive(Debug, Default)]
pub struct PairingStore {
    pending: Mutex<Vec<PendingPairing>>,
}

#[derive(Debug, Clone, Serialize)]
struct PendingPairing {
    code: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    client_ip: Option<String>,
    attempts: u32,
}

impl PairingStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending_count(&self) -> usize {
        let mut pending = self.pending.lock();
        pending.retain(|p| p.expires_at > Utc::now());
        pending.len()
    }
}

fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
}

fn require_auth(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    if state.pairing.require_pairing() {
        let token = extract_bearer(headers).unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return Err((StatusCode::UNAUTHORIZED, "Unauthorized"));
        }
    }
    Ok(())
}

/// POST /api/pairing/initiate — initiate a new pairing session
pub async fn initiate_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match state
        .pairing
        .generate_new_pairing_code(crate::live_pairing_code_policy(&state))
    {
        Some(code) => Json(serde_json::json!({
            "pairing_code": code,
            "message": "New pairing code generated"
        }))
        .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Pairing is disabled or not available",
        )
            .into_response(),
    }
}

/// POST /api/pair — submit pairing code (for new device pairing)
pub async fn submit_pairing_enhanced(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let code = body["code"].as_str().unwrap_or("");
    let device_name = body["device_name"].as_str().map(String::from);
    let device_type = body["device_type"].as_str().map(String::from);

    // Derive the brute-force lockout key from the real connection peer, only trusting
    // forwarded headers behind a configured proxy. Reading it straight from
    // `X-Forwarded-For` let an unauthenticated client vary the header to dodge the
    // per-client lockout entirely. Mirrors the legacy `/pair` handler.
    let client_id =
        super::client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);

    // Brute-force protection, mirroring the legacy `/pair` handler: a coarse
    // per-key request cap plus the shared auth rate limiter. Both are keyed on
    // the connection-derived client id (not a spoofable header), so this handler
    // cannot bypass rate limiting by rotating untrusted forwarding headers.
    if !state.rate_limiter.allow_pair(&client_id) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "paired": false,
                "error": "Too many pairing requests. Please retry later.",
                "retry_after": super::RATE_LIMIT_WINDOW_SECS,
            })),
        )
            .into_response();
    }
    if let Err(e) = state.auth_limiter.check_rate_limit(&client_id) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "paired": false,
                "error": format!("Too many auth attempts. Try again in {}s.", e.retry_after_secs),
                "retry_after": e.retry_after_secs,
            })),
        )
            .into_response();
    }

    match state.pairing.try_pair(code, &client_id).await {
        Ok(Some(token)) => {
            let token_hash = {
                use sha2::{Digest, Sha256};
                let hash = Sha256::digest(token.as_bytes());
                hex::encode(hash)
            };

            if let Some(ref registry) = state.device_registry {
                if let Err(e) = registry.register(
                    token_hash.clone(),
                    DeviceInfo {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: device_name,
                        device_type,
                        paired_at: Utc::now(),
                        last_seen: Utc::now(),
                        ip_address: Some(client_id),
                        capabilities: None,
                    },
                ) {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                        "device registry insert failed after successful pairing; rolling back in-process token"
                    );
                    state.pairing.revoke_token_hash(&token_hash);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "paired": false,
                            "persisted": false,
                            "error": format!("Device registry error: {e}"),
                            "message": "Pairing failed; the in-process token was not retained.",
                        })),
                    )
                        .into_response();
                }
            }
            if let Err(e) = super::persist_pairing_tokens(
                state.config.clone(),
                &state.pairing,
                state.config_write_lock.clone(),
            )
            .await
            {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "pairing token persistence failed; rolling back in-process token"
                );
                state.pairing.revoke_token_hash(&token_hash);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "paired": false,
                        "persisted": false,
                        "error": format!("Token persistence error: {e}"),
                        "message": "Pairing failed; the in-process token was not retained.",
                    })),
                )
                    .into_response();
            }
            Json(serde_json::json!({
                "paired": true,
                "persisted": true,
                "token": token,
                "message": "Pairing successful"
            }))
            .into_response()
        }
        Ok(None) => {
            // Feed the shared auth limiter so repeated invalid codes trip the
            // cross-request lockout, exactly as the legacy `/pair` handler does.
            state.auth_limiter.record_attempt(&client_id);
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "paired": false,
                    "error": "Invalid or expired pairing code",
                })),
            )
                .into_response()
        }
        Err(lockout_secs) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "paired": false,
                "error": format!("Too many attempts. Locked out for {lockout_secs}s"),
                "retry_after": lockout_secs,
            })),
        )
            .into_response(),
    }
}

/// A paired-device refusal as the dashboard sees it: the failure's status and
/// its message as a plain-text body.
fn device_failure_response(
    failure: zeroclaw_runtime::devices::DeviceFailure,
) -> axum::response::Response {
    let status =
        StatusCode::from_u16(failure.http_status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, failure.message).into_response()
}

/// GET /api/devices — list paired devices
pub async fn list_devices(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    match zeroclaw_runtime::devices::list_devices_body(state.device_registry.as_deref()) {
        Ok(body) => Json(body).into_response(),
        Err(failure) => device_failure_response(failure),
    }
}

/// DELETE /api/devices/{id} — revoke a paired device and its bearer token.
pub async fn revoke_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    match zeroclaw_runtime::devices::revoke_device(
        state.device_registry.as_deref(),
        &state.pairing,
        state.config.clone(),
        state.config_write_lock.clone(),
        &device_id,
    )
    .await
    {
        Ok(body) => Json(body).into_response(),
        Err(failure) => device_failure_response(failure),
    }
}

/// POST /api/devices/me/capabilities — the calling device replaces its capability list.
/// The "me" path means there's no separate device id in the URL — the bearer token in
/// Authorization identifies which row gets updated. Body: `{ "capabilities": ["..."] }`.
pub async fn update_my_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let token = match extract_bearer(&headers) {
        Some(t) => t,
        None => return (StatusCode::UNAUTHORIZED, "Missing bearer token").into_response(),
    };
    let token_hash = {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(token.as_bytes());
        hex::encode(hash)
    };

    let capabilities: Vec<String> = body
        .get("capabilities")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let registry = match state.device_registry.as_ref() {
        Some(r) => r,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Device registry is disabled",
            )
                .into_response();
        }
    };

    if registry.update_capabilities(&token_hash, capabilities.clone()) {
        Json(serde_json::json!({
            "message": "Capabilities updated",
            "capabilities": capabilities,
        }))
        .into_response()
    } else {
        (StatusCode::NOT_FOUND, "Device not found for this token").into_response()
    }
}

pub async fn rotate_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(registry) = state.device_registry.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Device registry is disabled",
        )
            .into_response();
    };

    let token_hash = match registry.revoke(&device_id) {
        Ok(Some(hash)) => hash,
        Ok(None) => return (StatusCode::NOT_FOUND, "Device not found").into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Device registry error: {e}"),
            )
                .into_response();
        }
    };

    state.pairing.revoke_token_hash(&token_hash);

    // Same persist-fail caveat as `revoke_device`: device row + in-memory
    // token are already gone; surfacing the persist error tells the caller
    // a restart could resurrect the token.
    if let Err(e) = super::persist_pairing_tokens(
        state.config.clone(),
        &state.pairing,
        state.config_write_lock.clone(),
    )
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Token revoked in memory but config persist failed: {e}"),
        )
            .into_response();
    }

    // Issue the new pairing code atomically against the slot. If another
    // flow holds the slot, the revoke still stands — return 200 with
    // `pairing_code: null` and a message that tells the operator what
    // happened so they do not assume rotation failed.
    match state.pairing.generate_pairing_code_if_vacant(crate::live_pairing_code_policy(&state)) {
        Ok(code) => Json(serde_json::json!({
            "device_id": device_id,
            "pairing_code": code,
            "message": "Old token revoked. Use this code to re-pair the device.",
        }))
        .into_response(),
        Err(zeroclaw_config::pairing::GeneratePairingCodeError::Pending) => {
            Json(serde_json::json!({
                "device_id": device_id,
                "pairing_code": null,
                "message": "Old token revoked. A pairing code is already pending; use it or call again after it clears.",
            }))
            .into_response()
        }
        Err(zeroclaw_config::pairing::GeneratePairingCodeError::PairingDisabled) => {
            Json(serde_json::json!({
                "device_id": device_id,
                "pairing_code": null,
                "message": "Old token revoked. Pairing is disabled; cannot issue a new code.",
            }))
            .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GatewayRateLimiter;
    use crate::api::test_state;
    use crate::auth_rate_limit::{AuthRateLimiter, MAX_ATTEMPTS};
    use axum::Json;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use std::sync::Arc;
    use zeroclaw_config::pairing::PairingGuard;
    use zeroclaw_config::schema::Config;

    /// Build an `AppState` whose device-registry points at a non-existent
    /// path so every SQLite write fails. Pairing enabled so a freshly
    /// issued code is actually consumable.
    fn unwriteable_registry_state() -> AppState {
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        state.device_registry = Some(Arc::new(DeviceRegistry::with_db_path(PathBuf::from(
            "/this/path/does/not/exist/devices.db",
        ))));
        state
    }

    async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_rolls_back_in_process_token_when_registry_register_fails() {
        let state = unwriteable_registry_state();

        // Issue a pairing code so the next `try_pair` succeeds.
        let code = state
            .pairing
            .generate_new_pairing_code(crate::live_pairing_code_policy(&state))
            .expect("pairing code must be issuable when require_pairing=true");

        let (status, body) = response_json(
            submit_pairing_enhanced(
                State(state.clone()),
                ConnectInfo("127.0.0.1:40000".parse().unwrap()),
                HeaderMap::new(),
                Json(serde_json::json!({"code": code, "device_name": "test"})),
            )
            .await
            .into_response(),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "registry.register failure path must surface as 500"
        );
        assert_eq!(body["paired"], serde_json::Value::Bool(false));
        assert_eq!(body["persisted"], serde_json::Value::Bool(false));
        assert!(
            body.get("token").is_none(),
            "5xx body MUST NOT contain the plaintext bearer token; got: {body}"
        );
        assert!(
            state.pairing.tokens().is_empty(),
            "PairingGuard::paired_tokens must be empty after a failed registry.register \
             (compensating `revoke_token_hash`); instead have {:?}",
            state.pairing.tokens()
        );
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_rolls_back_in_process_token_when_persist_fails() {
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        let tmp = tempfile::TempDir::new().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"").expect("seed blocker file");
        {
            let mut cfg = state.config.write();
            cfg.config_path = blocker.join("config.toml");
        }

        let code = state
            .pairing
            .generate_new_pairing_code(crate::live_pairing_code_policy(&state))
            .expect("pairing code must be issuable when require_pairing=true");

        let (status, body) = response_json(
            submit_pairing_enhanced(
                State(state.clone()),
                ConnectInfo("127.0.0.1:40001".parse().unwrap()),
                HeaderMap::new(),
                Json(serde_json::json!({"code": code})),
            )
            .await
            .into_response(),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "persistence failure path must surface as 500 (legacy leaked 200 + token)"
        );
        assert_eq!(body["paired"], serde_json::Value::Bool(false));
        assert!(
            body.get("token").is_none(),
            "5xx body MUST NOT contain the plaintext bearer token; got: {body}"
        );
        assert!(
            state.pairing.tokens().is_empty(),
            "PairingGuard::paired_tokens must be empty after a failed persist; have {:?}",
            state.pairing.tokens()
        );
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_keys_lockout_on_peer_not_forwarded_header() {
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        // Default config does not trust forwarded headers.
        assert!(!state.trust_forwarded_headers);

        let peer: SocketAddr = "203.0.113.7:55555".parse().unwrap();

        // Five wrong codes from one peer, each spoofing a different X-Forwarded-For.
        for i in 0..5 {
            let mut headers = HeaderMap::new();
            headers.insert("X-Forwarded-For", format!("192.0.2.{i}").parse().unwrap());
            let (status, _) = response_json(
                submit_pairing_enhanced(
                    State(state.clone()),
                    ConnectInfo(peer),
                    headers,
                    Json(serde_json::json!({"code": "wrong"})),
                )
                .await
                .into_response(),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "attempt {i} is an invalid code and must not be locked out yet"
            );
        }

        // A sixth attempt with yet another spoofed header must be locked out:
        // the real peer IP keeps every spoofed value in the same bucket.
        let mut headers = HeaderMap::new();
        headers.insert("X-Forwarded-For", "198.51.100.9".parse().unwrap());
        let (status, _) = response_json(
            submit_pairing_enhanced(
                State(state.clone()),
                ConnectInfo(peer),
                headers,
                Json(serde_json::json!({"code": "wrong"})),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "lockout must key on the peer IP so X-Forwarded-For spoofing cannot bypass it"
        );
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_honors_trusted_forwarded_client_identity() {
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        state.trust_forwarded_headers = true;
        state.rate_limiter = Arc::new(GatewayRateLimiter::new(1, 100, 100));
        let peer: SocketAddr = "10.0.0.2:55555".parse().unwrap();

        for forwarded in ["198.51.100.10", "198.51.100.11"] {
            let mut headers = HeaderMap::new();
            headers.insert("X-Forwarded-For", forwarded.parse().unwrap());
            let (status, _) = response_json(
                submit_pairing_enhanced(
                    State(state.clone()),
                    ConnectInfo(peer),
                    headers,
                    Json(serde_json::json!({"code": "wrong"})),
                )
                .await
                .into_response(),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "trusted forwarded clients must receive independent rate buckets"
            );
        }

        let mut headers = HeaderMap::new();
        headers.insert("X-Forwarded-For", "198.51.100.10".parse().unwrap());
        let (status, body) = response_json(
            submit_pairing_enhanced(
                State(state),
                ConnectInfo(peer),
                headers,
                Json(serde_json::json!({"code": "wrong"})),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            body["error"],
            "Too many pairing requests. Please retry later."
        );
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_enforces_pair_request_limiter_threshold() {
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        state.rate_limiter = Arc::new(GatewayRateLimiter::new(2, 100, 100));
        let peer: SocketAddr = "203.0.113.20:55555".parse().unwrap();

        for attempt in 0..2 {
            let (status, _) = response_json(
                submit_pairing_enhanced(
                    State(state.clone()),
                    ConnectInfo(peer),
                    HeaderMap::new(),
                    Json(serde_json::json!({"code": "wrong"})),
                )
                .await
                .into_response(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "attempt {attempt}");
        }

        let (status, body) = response_json(
            submit_pairing_enhanced(
                State(state),
                ConnectInfo(peer),
                HeaderMap::new(),
                Json(serde_json::json!({"code": "wrong"})),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            body["error"],
            "Too many pairing requests. Please retry later."
        );
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_enforces_shared_auth_limiter_threshold() {
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        state.rate_limiter = Arc::new(GatewayRateLimiter::new(100, 100, 100));
        state.auth_limiter = Arc::new(AuthRateLimiter::new());
        let peer: SocketAddr = "203.0.113.30:55555".parse().unwrap();
        let client_id = peer.ip().to_string();
        for _ in 0..MAX_ATTEMPTS {
            state.auth_limiter.record_attempt(&client_id);
        }

        let (status, body) = response_json(
            submit_pairing_enhanced(
                State(state),
                ConnectInfo(peer),
                HeaderMap::new(),
                Json(serde_json::json!({"code": "wrong"})),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("Too many auth attempts.")),
            "response must come from the shared authentication limiter: {body}"
        );
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_invalid_code_feeds_shared_auth_limiter() {
        // The handler must *record* each invalid attempt into the shared auth
        // limiter, not merely check pre-existing state. Preload the limiter to
        // one below the threshold, then a single invalid pairing call must push
        // it to the threshold so the *next* request is locked out by the shared
        // limiter. This fails if the handler's `record_attempt` call is dropped:
        // the second request would still see `MAX_ATTEMPTS - 1` and return 400.
        // PairingGuard sees only two attempts here (< its 5-attempt lockout), so
        // it never produces the 429 — the lockout can only come from the shared
        // limiter the handler fed.
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        state.rate_limiter = Arc::new(GatewayRateLimiter::new(100, 100, 100));
        state.auth_limiter = Arc::new(AuthRateLimiter::new());
        let peer: SocketAddr = "203.0.113.40:55555".parse().unwrap();
        let client_id = peer.ip().to_string();

        for _ in 0..(MAX_ATTEMPTS - 1) {
            state.auth_limiter.record_attempt(&client_id);
        }

        let (status, _) = response_json(
            submit_pairing_enhanced(
                State(state.clone()),
                ConnectInfo(peer),
                HeaderMap::new(),
                Json(serde_json::json!({"code": "wrong"})),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "an invalid code returns 400 and records the attempt into the shared limiter"
        );

        let (status, body) = response_json(
            submit_pairing_enhanced(
                State(state),
                ConnectInfo(peer),
                HeaderMap::new(),
                Json(serde_json::json!({"code": "wrong"})),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "the handler's own recording pushed the shared limiter to its threshold"
        );
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("Too many auth attempts.")),
            "the lockout must come from the shared auth limiter, not PairingGuard: {body}"
        );
    }

    /// Serve `submit_pairing_enhanced` on a real loopback listener with
    /// `ConnectInfo<SocketAddr>` and return the bound address plus the server
    /// task handle, so tests exercise the outer HTTP + proxy boundary rather
    /// than calling the handler directly.
    async fn serve_pairing(state: AppState) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new()
            .route("/api/pair", axum::routing::post(submit_pairing_enhanced))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = zeroclaw_spawn::spawn!(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });
        (addr, handle)
    }

    /// Send one wrong `POST /api/pair` over a fresh connection with the given
    /// `X-Forwarded-For`, returning the HTTP status code. A raw request keeps
    /// the test free of an HTTP-client dependency while still driving the real
    /// service (ConnectInfo + header extraction).
    async fn post_pair_status(addr: std::net::SocketAddr, forwarded_for: &str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let body = r#"{"code":"wrong"}"#;
        let request = format!(
            "POST /api/pair HTTP/1.1\r\nHost: localhost\r\nX-Forwarded-For: {forwarded_for}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        let status_line = text.lines().next().expect("HTTP status line");
        status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("HTTP status code")
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_http_boundary_rotating_forwarded_header_cannot_evade_lockout()
    {
        // Default (untrusted) forwarded headers: a single direct peer rotating
        // `X-Forwarded-For` on every request must not dodge the peer-keyed
        // lockout. After five wrong attempts the sixth is locked out (429).
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        state.rate_limiter = Arc::new(GatewayRateLimiter::new(100, 100, 100));
        state.auth_limiter = Arc::new(AuthRateLimiter::new());
        state.trust_forwarded_headers = false;

        let (addr, server) = serve_pairing(state).await;

        let mut statuses = Vec::new();
        for i in 0..6 {
            statuses.push(post_pair_status(addr, &format!("10.0.0.{i}")).await);
        }
        server.abort();

        assert_eq!(
            statuses,
            vec![400, 400, 400, 400, 400, 429],
            "rotating X-Forwarded-For from one direct peer must not evade the lockout"
        );
    }

    #[tokio::test]
    async fn submit_pairing_enhanced_http_boundary_trusted_proxy_separates_clients() {
        // Trusted-proxy mode: the forwarded client identity is honoured, so
        // client A's five failures lock only A. Client B keeps a fresh bucket,
        // and A's sixth request is the one that is locked out.
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(
            true,
            &[],
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        state.rate_limiter = Arc::new(GatewayRateLimiter::new(100, 100, 100));
        state.auth_limiter = Arc::new(AuthRateLimiter::new());
        state.trust_forwarded_headers = true;

        let (addr, server) = serve_pairing(state).await;

        let client_a = "198.51.100.7";
        let client_b = "198.51.100.8";

        let mut a_statuses = Vec::new();
        for _ in 0..5 {
            a_statuses.push(post_pair_status(addr, client_a).await);
        }
        let b_status = post_pair_status(addr, client_b).await;
        let a_sixth = post_pair_status(addr, client_a).await;
        server.abort();

        assert_eq!(
            a_statuses,
            vec![400, 400, 400, 400, 400],
            "client A's five wrong attempts"
        );
        assert_eq!(
            b_status, 400,
            "client B has an independent bucket behind the trusted proxy"
        );
        assert_eq!(a_sixth, 429, "client A is locked out on its sixth attempt");
    }
}
