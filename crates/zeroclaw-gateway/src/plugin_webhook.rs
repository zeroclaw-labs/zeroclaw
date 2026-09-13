//! HTTP admission and responses for channel-plugin webhooks.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Bytes,
    extract::{ConnectInfo, Path, RawQuery, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Json, Response},
    routing::get,
};

use crate::{AppState, IdempotencyStore, RATE_LIMIT_WINDOW_SECS, client_key_from_request};

const PLUGIN_WEBHOOK_TIMEOUT_SECS: u64 = 10;

fn plugin_webhook_idempotency_key(path: &str, message_id: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    digest.update(b"zeroclaw-plugin-webhook\0");
    digest.update(path.as_bytes());
    digest.update(b"\0");
    digest.update(message_id.as_bytes());
    format!("plugin-webhook:{}", hex::encode(digest.finalize()))
}

fn plugin_webhook_idempotency(
    store: Arc<IdempotencyStore>,
    path: &str,
) -> zeroclaw_api::webhook::WebhookIdempotency {
    let begin_store = Arc::clone(&store);
    let commit_store = Arc::clone(&store);
    let path = path.to_string();
    zeroclaw_api::webhook::WebhookIdempotency::new(
        move |message_id| {
            begin_store.begin_reservation(&plugin_webhook_idempotency_key(&path, message_id))
        },
        move |token| commit_store.commit_reservation(token),
        move |token| store.rollback_reservation(token),
    )
}

pub(super) fn routes(
    registry: Arc<zeroclaw_api::webhook::PluginWebhookRegistry>,
) -> Router<AppState> {
    Router::new()
        .route(
            "/plugin/{path}",
            get(handle_plugin_webhook)
                .post(handle_plugin_webhook)
                .head(unsupported_method)
                .fallback(unsupported_method),
        )
        .layer(axum::Extension(registry))
}

async fn unsupported_method() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET, POST")],
    )
}

/// GET/POST `/plugin/{path}` — forward exact request bytes to the channel plugin
/// that atomically claimed `path` during this daemon generation.
async fn handle_plugin_webhook(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    axum::Extension(registry): axum::Extension<Arc<zeroclaw_api::webhook::PluginWebhookRegistry>>,
    Path(path): Path<String>,
    method: Method,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    use zeroclaw_api::webhook::{
        MAX_WEBHOOK_RESPONSE_BODY_BYTES, RawWebhook, WebhookOutcome, WebhookReject,
    };

    // Apply the same trusted-forwarded-aware client key and limiter as the
    // built-in webhook before route lookup or guest work.
    let rate_key =
        client_key_from_request(Some(peer_addr), &headers, state.trust_forwarded_headers);
    if !state.rate_limiter.allow_webhook(&rate_key) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "path": path,
                    "error_key": "plugin_webhook_rate_limited",
                })),
            "Plugin webhook rate limit exceeded"
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "Too many webhook requests. Please retry later.",
                "retry_after": RATE_LIMIT_WINDOW_SECS,
            })),
        )
            .into_response();
    }

    let Some(sink) = registry.get(&path) else {
        return (StatusCode::NOT_FOUND, "webhook not found").into_response();
    };
    let headers = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect();
    let cancellation = zeroclaw_api::webhook::WebhookCancellation::new();
    let _cancel_on_exit = cancellation.clone().drop_guard();
    let (reply, outcome) = tokio::sync::oneshot::channel();
    let request = RawWebhook {
        method: method.to_string(),
        query: query.unwrap_or_default(),
        headers,
        body: body.to_vec(),
        cancellation,
        idempotency: Some(plugin_webhook_idempotency(
            Arc::clone(&state.idempotency_store),
            &path,
        )),
        reply,
    };
    match sink.try_send(request) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            return (StatusCode::TOO_MANY_REQUESTS, "webhook queue full").into_response();
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "webhook unavailable").into_response();
        }
    }

    match tokio::time::timeout(Duration::from_secs(PLUGIN_WEBHOOK_TIMEOUT_SECS), outcome).await {
        Ok(Ok(Ok(WebhookOutcome::Ack))) => StatusCode::OK.into_response(),
        Ok(Ok(Ok(WebhookOutcome::Body(body)))) => {
            if body.len() > MAX_WEBHOOK_RESPONSE_BODY_BYTES {
                (StatusCode::BAD_GATEWAY, "invalid webhook response").into_response()
            } else {
                body.into_response()
            }
        }
        Ok(Ok(Err(WebhookReject::InvalidResponse))) => {
            (StatusCode::BAD_GATEWAY, "invalid webhook response").into_response()
        }
        Ok(Ok(Err(WebhookReject::Unauthorized(_)))) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "error_key": "plugin_webhook_unauthorized",
                    })),
                "Channel plugin rejected webhook authentication"
            );
            (StatusCode::UNAUTHORIZED, "unauthorized webhook").into_response()
        }
        Ok(Ok(Err(WebhookReject::BadRequest(_)))) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "error_key": "plugin_webhook_invalid",
                    })),
                "Channel plugin rejected malformed webhook"
            );
            (StatusCode::BAD_REQUEST, "invalid webhook").into_response()
        }
        Ok(Ok(Err(WebhookReject::Unavailable(_)))) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "error_key": "plugin_webhook_unavailable",
                    })),
                "Channel plugin webhook processing unavailable"
            );
            (StatusCode::SERVICE_UNAVAILABLE, "webhook unavailable").into_response()
        }
        Ok(Ok(Err(WebhookReject::Timeout))) | Err(_) => {
            (StatusCode::GATEWAY_TIMEOUT, "webhook processing timed out").into_response()
        }
        Ok(Err(_)) => (StatusCode::SERVICE_UNAVAILABLE, "webhook unavailable").into_response(),
    }
}

#[cfg(test)]
mod tests;
