//! HTTP adapter for channel-plugin webhooks.
//!
//! Rate limiting and request bounds happen here; route admission, the
//! deadline, and dedup belong to the core ingress, reached in process or, from
//! the standalone gateway on Unix, over RPC.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{ConnectInfo, Path, RawQuery, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Json, Response},
    routing::get,
};
use zeroclaw_api::webhook::{
    MAX_PLUGIN_WEBHOOK_BODY_BYTES, PluginWebhookOutcome, PluginWebhookRequest, WebhookCancellation,
};
use zeroclaw_infra::plugin_webhook::PluginWebhookIngress;

use crate::{AppState, MAX_BODY_SIZE, RATE_LIMIT_WINDOW_SECS, client_key_from_request};

#[cfg(unix)]
mod forward;

// A body the gateway admits must never be one the ingress refuses: that would
// turn a 413 into a 400.
const _: () = assert!(MAX_BODY_SIZE <= MAX_PLUGIN_WEBHOOK_BODY_BYTES);

/// Where admitted requests are resolved. Chosen once at startup; a request
/// never falls back from one to the other.
#[derive(Clone)]
pub(crate) enum PluginWebhookBackend {
    /// This process's ingress: the supervised gateway inside the daemon.
    InProcess(Arc<PluginWebhookIngress>),
    /// The daemon's ingress over its local RPC socket: the standalone
    /// gateway. This connection is the forwarder's own, never the one other
    /// routes reach through the `CoreRpc` extension.
    #[cfg(unix)]
    Core(crate::core_rpc::CoreRpc),
}

impl PluginWebhookBackend {
    /// The public response for an admitted request.
    async fn respond(&self, request: PluginWebhookRequest) -> Response {
        match self {
            Self::InProcess(ingress) => {
                let cancellation = WebhookCancellation::new();
                let _cancel_on_exit = cancellation.clone().drop_guard();
                outcome_response(ingress.dispatch(request, &cancellation).await)
            }
            #[cfg(unix)]
            Self::Core(core) => forward::respond(core, request).await,
        }
    }
}

pub(super) fn routes(backend: PluginWebhookBackend) -> Router<AppState> {
    Router::new()
        .route(
            "/plugin/{path}",
            get(handle_plugin_webhook)
                .post(handle_plugin_webhook)
                .head(unsupported_method)
                .fallback(unsupported_method),
        )
        .layer(axum::Extension(backend))
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
    axum::Extension(backend): axum::Extension<PluginWebhookBackend>,
    Path(path): Path<String>,
    method: Method,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
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

    // Drop, rather than refuse the request over, a value outside visible
    // ASCII, space, and tab: the ingress would reject the whole request.
    let headers = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect();
    let request = match PluginWebhookRequest::new(
        path.as_str(),
        method.as_str(),
        query.unwrap_or_default(),
        headers,
        body.to_vec(),
    ) {
        Ok(request) => request,
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "reason": error.reason(),
                        "error_key": "plugin_webhook_request_invalid",
                    })),
                "Plugin webhook request exceeds the ingress bounds"
            );
            return (StatusCode::BAD_REQUEST, "invalid webhook").into_response();
        }
    };
    backend.respond(request).await
}

/// The fixed public status and body for each ingress outcome. Guest and host
/// detail never reaches this unauthenticated surface.
fn outcome_response(outcome: PluginWebhookOutcome) -> Response {
    match outcome {
        PluginWebhookOutcome::Ack => StatusCode::OK.into_response(),
        PluginWebhookOutcome::Reply(body) => body.into_response(),
        PluginWebhookOutcome::NotFound => {
            (StatusCode::NOT_FOUND, "webhook not found").into_response()
        }
        PluginWebhookOutcome::QueueFull => {
            (StatusCode::TOO_MANY_REQUESTS, "webhook queue full").into_response()
        }
        PluginWebhookOutcome::InvalidResponse => {
            (StatusCode::BAD_GATEWAY, "invalid webhook response").into_response()
        }
        PluginWebhookOutcome::Unauthorized => {
            (StatusCode::UNAUTHORIZED, "unauthorized webhook").into_response()
        }
        PluginWebhookOutcome::BadRequest => {
            (StatusCode::BAD_REQUEST, "invalid webhook").into_response()
        }
        PluginWebhookOutcome::Unavailable | PluginWebhookOutcome::Cancelled => {
            (StatusCode::SERVICE_UNAVAILABLE, "webhook unavailable").into_response()
        }
        PluginWebhookOutcome::Timeout => {
            (StatusCode::GATEWAY_TIMEOUT, "webhook processing timed out").into_response()
        }
    }
}

#[cfg(test)]
mod tests;
