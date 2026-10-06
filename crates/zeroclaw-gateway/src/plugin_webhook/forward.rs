//! Forwards admitted plugin webhooks to the core's ingress over RPC.
//!
//! The request is validated by the same constructor the in-process path uses
//! before it gets here, and the core validates it again when it arrives. The
//! outcome comes back as the ingress reported it, so both paths map to the
//! same public responses.
//!
//! Unix only: the standalone gateway on Windows does not forward, because it
//! cannot verify which process serves the daemon's named pipe.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use zeroclaw_api::jsonrpc::error_codes::{AUTH_REQUIRED, FORBIDDEN, METHOD_NOT_FOUND};
use zeroclaw_api::webhook::{PLUGIN_WEBHOOK_DEADLINE, PluginWebhookOutcome, PluginWebhookRequest};
use zeroclaw_rpc_client::{ClientError, ConnectionState, Method, RpcClient};
use zeroclaw_rpc_proto::types::{
    PluginWebhookCancelParams, PluginWebhookDispatchParams, PluginWebhookDispatchResult,
    PluginWebhookHeader,
};

use super::outcome_response;
use crate::core_rpc::{CoreLink, CoreRpc};

/// How long past the ingress deadline the forwarder waits for the core's own
/// `timeout` outcome before it gives up on the core itself.
const CORE_TIMEOUT_MARGIN: Duration = Duration::from_secs(1);
/// Ceiling for the best-effort cancel of an abandoned dispatch.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(1);

/// What the forwarder learned: the ingress outcome, or why the core could not
/// be asked for one.
#[derive(Debug, PartialEq, Eq)]
enum Dispatched {
    Outcome(PluginWebhookOutcome),
    /// No connection to the core, or it dropped before answering.
    CoreUnavailable,
    /// The core refused the connection or the call.
    CoreRefused {
        code: i32,
    },
    /// The core does not serve plugin webhook dispatch.
    CoreUnsupported,
    /// The core did not answer within the ingress deadline plus a margin.
    CoreTimedOut,
    /// The core answered with something that is not a dispatch outcome.
    CoreUnexpected {
        reason: &'static str,
        code: Option<i32>,
    },
}

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(0);

/// A `request_id` unique within this process, so unique among the forwarder
/// connection's in-flight dispatches: printable ASCII and far under the
/// 128-byte limit.
fn next_request_id() -> String {
    format!(
        "gw{}-{}",
        std::process::id(),
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    )
}

/// The wire form of `request`. Headers keep their order and repeated names;
/// the body travels as standard base64.
fn dispatch_params(
    request_id: &str,
    request: &PluginWebhookRequest,
) -> PluginWebhookDispatchParams {
    PluginWebhookDispatchParams {
        request_id: request_id.to_owned(),
        path: request.path().to_owned(),
        method: request.method().to_owned(),
        query: request.query().to_owned(),
        headers: request
            .headers()
            .iter()
            .map(|(name, value)| PluginWebhookHeader {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        body_b64: base64::engine::general_purpose::STANDARD.encode(request.body()),
    }
}

/// The public response for `request`, as the core's ingress decides it.
pub(super) async fn respond(core: &CoreRpc, request: PluginWebhookRequest) -> Response {
    let path = request.path().to_owned();
    dispatched_response(&path, dispatch(core, request).await)
}

/// The public response for a forwarded dispatch. An ingress outcome maps
/// through [`outcome_response`]; a core that could not be asked fails fast
/// with a 503, or a 504 when it never answered, and the reason is logged here
/// because it is a gateway-side event.
fn dispatched_response(path: &str, dispatched: Dispatched) -> Response {
    let unavailable = || (StatusCode::SERVICE_UNAVAILABLE, "webhook unavailable").into_response();
    match dispatched {
        Dispatched::Outcome(outcome) => outcome_response(outcome),
        Dispatched::CoreUnavailable => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "error_key": "plugin_webhook_core_unavailable",
                    })),
                "Plugin webhook not forwarded: the core is not connected"
            );
            unavailable()
        }
        Dispatched::CoreRefused { code } => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "code": code,
                        "error_key": "plugin_webhook_core_refused",
                    })),
                "Core refused the plugin webhook dispatch"
            );
            unavailable()
        }
        Dispatched::CoreUnsupported => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "error_key": "plugin_webhook_core_unsupported",
                    })),
                "Core does not support plugin webhook dispatch"
            );
            unavailable()
        }
        Dispatched::CoreUnexpected { reason, code } => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "reason": reason,
                        "code": code,
                        "error_key": "plugin_webhook_core_unexpected",
                    })),
                "Core returned an unusable plugin webhook result"
            );
            unavailable()
        }
        Dispatched::CoreTimedOut => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path,
                        "error_key": "plugin_webhook_core_timeout",
                    })),
                "Core did not answer the plugin webhook dispatch in time"
            );
            (StatusCode::GATEWAY_TIMEOUT, "webhook processing timed out").into_response()
        }
    }
}

/// Ask the core's ingress for `request`'s outcome. Fails fast when there is
/// no usable connection, and never waits longer than the ingress deadline
/// plus [`CORE_TIMEOUT_MARGIN`].
async fn dispatch(core: &CoreRpc, request: PluginWebhookRequest) -> Dispatched {
    let client = match core.link().await {
        CoreLink::Connected(client) => client,
        CoreLink::Disconnected => return Dispatched::CoreUnavailable,
        CoreLink::Refused { code } => return Dispatched::CoreRefused { code },
    };
    // The link still names a client for a moment after its connection ends,
    // and a request on that client fails as a JSON-RPC error rather than as
    // a disconnect.
    if client.state() != ConnectionState::Connected {
        return Dispatched::CoreUnavailable;
    }
    if !client.supports(Method::PluginWebhookDispatch) {
        return Dispatched::CoreUnsupported;
    }
    let request_id = next_request_id();
    let Ok(params) = serde_json::to_value(dispatch_params(&request_id, &request)) else {
        return Dispatched::CoreUnexpected {
            reason: "unencodable dispatch params",
            code: None,
        };
    };
    drop(request);
    let cancel = CancelOnDrop::arm(Arc::clone(&client), request_id);
    let answered = client
        .request_with_timeout(
            Method::PluginWebhookDispatch.wire_name(),
            params,
            PLUGIN_WEBHOOK_DEADLINE + CORE_TIMEOUT_MARGIN,
        )
        .await;
    match answered {
        Ok(value) => {
            cancel.disarm();
            let Ok(result) = serde_json::from_value::<PluginWebhookDispatchResult>(value) else {
                return Dispatched::CoreUnexpected {
                    reason: "undecodable dispatch result",
                    code: None,
                };
            };
            match result.into_outcome() {
                Ok(outcome) => Dispatched::Outcome(outcome),
                Err(invalid) => Dispatched::CoreUnexpected {
                    reason: invalid.reason(),
                    code: None,
                },
            }
        }
        // The guard stays armed: the core may still hold the request, so it
        // is cancelled when the guard drops on return.
        Err(ClientError::Timeout { .. }) => Dispatched::CoreTimedOut,
        Err(ClientError::Disconnected(_)) => {
            cancel.disarm();
            Dispatched::CoreUnavailable
        }
        Err(ClientError::Rpc(error)) => {
            cancel.disarm();
            if client.state() != ConnectionState::Connected {
                return Dispatched::CoreUnavailable;
            }
            match error.code {
                AUTH_REQUIRED | FORBIDDEN => Dispatched::CoreRefused { code: error.code },
                METHOD_NOT_FOUND => Dispatched::CoreUnsupported,
                code => Dispatched::CoreUnexpected {
                    reason: "core returned an error",
                    code: Some(code),
                },
            }
        }
        Err(ClientError::Io(_) | ClientError::Handshake(_) | ClientError::Decode { .. }) => {
            cancel.disarm();
            Dispatched::CoreUnexpected {
                reason: "client error",
                code: None,
            }
        }
    }
}

/// Sends a best-effort `plugin-webhook/cancel` for a dispatch the forwarder
/// abandoned: the HTTP caller went away, or the core outlived the timeout.
/// Disarmed once the core has answered or the connection is gone.
///
/// The cancel is queued on the same connection after the dispatch frame, so
/// it never overtakes it. A cancel that races the dispatch's completion, or
/// names a dispatch whose frame was never written, is answered
/// `{"cancelled": false}`.
struct CancelOnDrop {
    client: Option<Arc<RpcClient>>,
    request_id: String,
}

impl CancelOnDrop {
    fn arm(client: Arc<RpcClient>, request_id: String) -> Self {
        Self {
            client: Some(client),
            request_id,
        }
    }

    fn disarm(mut self) {
        self.client = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let Some(client) = self.client.take() else {
            return;
        };
        // A closed connection needs no cancel: the core cancels every
        // dispatch of a connection that ends. A core without the method
        // would only refuse it. Spawning needs a runtime, which a drop during
        // runtime shutdown may not have.
        if client.state() != ConnectionState::Connected
            || !client.supports(Method::PluginWebhookCancel)
            || tokio::runtime::Handle::try_current().is_err()
        {
            return;
        }
        let request_id = std::mem::take(&mut self.request_id);
        zeroclaw_spawn::spawn!(async move {
            let Ok(params) = serde_json::to_value(PluginWebhookCancelParams { request_id }) else {
                return;
            };
            let _ = client
                .request_with_timeout(
                    Method::PluginWebhookCancel.wire_name(),
                    params,
                    CANCEL_TIMEOUT,
                )
                .await;
        });
    }
}

#[cfg(test)]
mod tests;
