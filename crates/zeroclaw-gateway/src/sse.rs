//! Server-Sent Events (SSE) stream for real-time event delivery.
//! Wraps the broadcast channel in AppState to deliver events to web dashboard clients.

use super::AppState;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use std::convert::Infallible;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

pub use zeroclaw_runtime::observability::broadcast::{BroadcastObserver, EventBuffer};
use zeroclaw_runtime::observability::broadcast::{history_events, is_public_event};

/// GET /api/events — SSE event stream.
///
/// Pairing credentials (QR payloads, one-shot pair codes) are **broadcast-only
/// and delivery-once**: they ride the live `event_tx` fan-out and are never
/// written to the history buffer or the persisted JSONL. A subscriber must be
/// connected *before* pairing to observe them; a client that connects late,
/// reconnects, or lags past the broadcast ring (the discarded
/// `BroadcastStreamRecvError` below) deliberately cannot recover the credential
/// — that is the non-persistent boundary, not a bug. Recovery would require
/// buffering the secret, which the credential boundary forbids.
pub async fn handle_sse_events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Auth check. When pairing is enabled every subscriber that reaches the
    // stream below has passed the bearer check, so the stream is authenticated;
    // when it is disabled no subscriber is authenticated. That posture decides
    // whether broadcast-only pairing secrets may ride the stream (see
    // `sse_frame_for_stream`).
    let auth_enforced = state.pairing.require_pairing();
    if auth_enforced {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .unwrap_or("");

        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization: Bearer <token>",
            )
                .into_response();
        }
    }

    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(
        move |result: Result<
            serde_json::Value,
            tokio_stream::wrappers::errors::BroadcastStreamRecvError,
        >| {
            match result {
                Ok(value) => sse_frame_for_stream(value, auth_enforced)
                    .map(|v| Ok::<_, Infallible>(Event::default().data(v.to_string()))),
                Err(_) => None, // Skip lagged messages
            }
        },
    );

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Decide the deliverable form of a broadcast frame for an SSE stream with the
/// given authentication posture. Returns `None` to withhold the frame.
///
/// Fail-closed contract: a frame carrying broadcast-only pairing secrets
/// (stamped [`zeroclaw_log::EPHEMERAL_BROADCAST_MARKER`] by the log layer when
/// it merges `ephemeral_attributes` — QR payloads, pair codes) is withheld
/// entirely unless `auth_enforced` is true. This keeps the credential off any
/// unauthenticated `/api/events` stream even though the pre-existing handler
/// skips the bearer check when pairing is disabled. The internal marker is
/// stripped before delivery so the public event shape is unchanged.
fn sse_frame_for_stream(
    mut value: serde_json::Value,
    auth_enforced: bool,
) -> Option<serde_json::Value> {
    if !is_public_sse_event(&value) {
        return None;
    }
    if zeroclaw_log::frame_carries_ephemeral_credentials(&value) && !auth_enforced {
        return None;
    }
    // Strip the internal marker so the delivered public shape is unchanged.
    // Shared with every other broadcast consumer (RPC `logs/subscribe`) so the
    // credential boundary is enforced identically across the bus.
    zeroclaw_log::strip_ephemeral_broadcast_marker(&mut value);
    Some(value)
}

/// GET /api/events/history — return buffered recent events as JSON.
pub async fn handle_events_history(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = super::api::require_auth(&state, &headers) {
        return e.into_response();
    }
    Json(history_events_payload(&state.event_buffer)).into_response()
}

fn history_events_payload(buffer: &EventBuffer) -> serde_json::Value {
    serde_json::json!({ "events": history_events(buffer) })
}

fn is_public_sse_event(event: &serde_json::Value) -> bool {
    is_public_event(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn session_scoped_events_are_not_public_sse_events() {
        let session_event = serde_json::json!({
            "type": "message",
            "session_id": "operator-1",
            "content": "private session notification"
        });
        let global_event = serde_json::json!({
            "type": "tool_call",
            "tool": "shell"
        });

        assert!(!is_public_sse_event(&session_event));
        assert!(is_public_sse_event(&global_event));
    }

    #[test]
    fn history_payload_returns_only_public_events() {
        let buffer = EventBuffer::new(8);
        buffer.push(serde_json::json!({
            "type": "message",
            "session_id": "operator-1",
            "content": "private session notification"
        }));
        buffer.push(serde_json::json!({
            "type": "agent_start",
            "source": "observability",
            "model_provider": "test",
            "model": "test-model"
        }));
        buffer.push(serde_json::json!({
            "type": "gateway_lifecycle",
            "phase": "ready"
        }));

        let payload = history_events_payload(&buffer);
        let events = payload["events"].as_array().expect("events array");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "agent_start");
        assert_eq!(events[1]["type"], "gateway_lifecycle");
    }

    /// Build a broadcast frame stamped the way `zeroclaw_log::record_event`
    /// stamps a credential-bearing login event (ephemeral attrs merged into
    /// `attributes.login`, plus the fail-closed marker).
    fn credential_login_frame() -> serde_json::Value {
        serde_json::json!({
            "source": "observability",
            "attributes": { "login": { "state": "qr", "qr_payload": "SECRET-QR-PAYLOAD" } },
            zeroclaw_log::EPHEMERAL_BROADCAST_MARKER: true,
        })
    }

    #[test]
    fn ephemeral_credential_frame_is_withheld_from_unauthenticated_stream() {
        // Pairing disabled ⇒ the `/api/events` handler skips the bearer check,
        // so the stream is unauthenticated. The credential frame must be
        // withheld entirely rather than fanned out to an anonymous subscriber.
        let frame = credential_login_frame();
        assert!(
            sse_frame_for_stream(frame, /* auth_enforced */ false).is_none(),
            "pairing secret must never ride an unauthenticated /api/events stream"
        );
    }

    #[test]
    fn ephemeral_credential_frame_reaches_authenticated_stream_without_marker() {
        // Pairing enabled ⇒ every subscriber passed the bearer check, so the
        // credential may be delivered; the internal marker is stripped first.
        let delivered =
            sse_frame_for_stream(credential_login_frame(), /* auth_enforced */ true)
                .expect("authenticated stream should receive the credential frame");
        assert_eq!(
            delivered["attributes"]["login"]["qr_payload"], "SECRET-QR-PAYLOAD",
            "authenticated stream still renders the QR payload"
        );
        assert!(
            delivered
                .get(zeroclaw_log::EPHEMERAL_BROADCAST_MARKER)
                .is_none(),
            "internal fail-closed marker must be stripped before delivery"
        );
    }

    #[test]
    fn credential_free_frame_flows_on_unauthenticated_stream() {
        // A lifecycle frame with no ephemeral secret is unmarked and still
        // flows when auth is disabled (unchanged behavior for non-secret data).
        let frame = serde_json::json!({
            "source": "observability",
            "attributes": { "login": { "state": "connected" } },
        });
        assert!(
            sse_frame_for_stream(frame, /* auth_enforced */ false).is_some(),
            "credential-free lifecycle frames are unaffected"
        );
    }

    #[test]
    fn session_scoped_frame_is_withheld_regardless_of_auth() {
        let frame = serde_json::json!({ "type": "message", "session_id": "operator-1" });
        assert!(sse_frame_for_stream(frame.clone(), true).is_none());
        assert!(sse_frame_for_stream(frame, false).is_none());
    }

    #[test]
    fn observability_tagged_events_are_public_even_without_session_id() {
        // After observability frames keep the SSE pathway open even
        // though they would not otherwise carry a session_id discriminator.
        let obs = serde_json::json!({
            "type": "tool_call",
            "source": "observability",
            "tool": "shell",
        });
        assert!(is_public_sse_event(&obs));
    }

    /// Warning follow-up (non-persistent boundary): even if a credential-marked
    /// login frame reached the replay buffer, `/api/events/history` must
    /// withhold it. Pairing secrets are delivery-once — a client that connects
    /// after pairing cannot recover them from history.
    #[test]
    fn history_payload_never_recovers_pairing_credentials() {
        let buffer = EventBuffer::new(8);
        buffer.push(credential_login_frame());
        buffer.push(serde_json::json!({
            "type": "agent_start",
            "source": "observability",
            "model_provider": "test",
            "model": "test-model",
        }));

        let payload = history_events_payload(&buffer);
        let events = payload["events"].as_array().expect("events array");
        assert_eq!(
            events.len(),
            1,
            "credential-bearing frame must be withheld from history: {events:?}"
        );
        assert_eq!(events[0]["type"], "agent_start");
        let dump = payload.to_string();
        assert!(
            !dump.contains("SECRET-QR-PAYLOAD"),
            "history must not expose a pairing secret: {dump}"
        );
        assert!(!dump.contains(zeroclaw_log::EPHEMERAL_BROADCAST_MARKER));
    }

    // ── Route-level `/api/events` credential-boundary regressions ─────────
    //
    // These drive the real axum handler through the router — PairingGuard,
    // bearer parsing, the early 401, subscription wiring, and the routed SSE
    // response — rather than calling `sse_frame_for_stream` in isolation, so
    // they catch the auth check being removed or disconnected from the filter.

    fn events_app(state: AppState) -> axum::Router {
        axum::Router::new()
            .route("/api/events", axum::routing::get(handle_sse_events))
            .with_state(state)
    }

    /// Drive the SSE response body, accumulating delivered bytes until `needle`
    /// is seen or the budget elapses (avoids blocking on the keep-alive).
    async fn read_stream_until(
        body: axum::body::Body,
        needle: &str,
        budget: std::time::Duration,
    ) -> String {
        use http_body_util::BodyExt;
        let mut body = body;
        let mut acc = String::new();
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(bytes) = frame.into_data() {
                        acc.push_str(&String::from_utf8_lossy(&bytes));
                        if acc.contains(needle) {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        acc
    }

    fn pairing_guard(
        require: bool,
        tokens: &[&str],
    ) -> Arc<zeroclaw_runtime::security::pairing::PairingGuard> {
        let owned: Vec<String> = tokens.iter().map(|t| (*t).to_string()).collect();
        Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
            require,
            &owned,
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ))
    }

    /// Pairing disabled ⇒ the handler skips the bearer check, so the stream is
    /// unauthenticated. A credential frame must be withheld end-to-end while a
    /// credential-free frame still flows (proving the stream is live).
    #[tokio::test]
    async fn route_withholds_credential_frame_on_unauthenticated_events_stream() {
        use tower::ServiceExt as _;

        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.pairing = pairing_guard(false, &[]);
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        state.event_tx = tx.clone();

        let response = events_app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let _ = tx.send(credential_login_frame());
        let _ = tx.send(serde_json::json!({
            "source": "observability",
            "type": "tool_call",
            "tool": "SENTINEL-LIVE",
        }));

        let body = read_stream_until(
            response.into_body(),
            "SENTINEL-LIVE",
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(
            body.contains("SENTINEL-LIVE"),
            "the stream must stay live for non-secret frames: {body:?}"
        );
        assert!(
            !body.contains("SECRET-QR-PAYLOAD"),
            "pairing secret must never reach an unauthenticated /api/events client: {body:?}"
        );
    }

    /// Pairing enabled ⇒ the handler enforces the bearer check and returns 401
    /// before any subscription for a missing or bad token.
    #[tokio::test]
    async fn route_returns_401_when_pairing_enabled_without_valid_token() {
        use tower::ServiceExt as _;

        // No token.
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.pairing = pairing_guard(true, &["valid-token"]);
        let response = events_app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);

        // Bad token.
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.pairing = pairing_guard(true, &["valid-token"]);
        let response = events_app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .header(axum::http::header::AUTHORIZATION, "Bearer wrong-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    /// Pairing enabled + valid bearer ⇒ every subscriber is authenticated, so
    /// the credential is delivered — with the internal marker stripped.
    #[tokio::test]
    async fn route_delivers_credential_without_marker_to_authenticated_client() {
        use tower::ServiceExt as _;

        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.pairing = pairing_guard(true, &["valid-token"]);
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        state.event_tx = tx.clone();

        let response = events_app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .header(axum::http::header::AUTHORIZATION, "Bearer valid-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let _ = tx.send(credential_login_frame());

        let body = read_stream_until(
            response.into_body(),
            "SECRET-QR-PAYLOAD",
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(
            body.contains("SECRET-QR-PAYLOAD"),
            "an authenticated client should receive the QR payload: {body:?}"
        );
        assert!(
            !body.contains(zeroclaw_log::EPHEMERAL_BROADCAST_MARKER),
            "the internal fail-closed marker must be stripped before delivery: {body:?}"
        );
    }
}
