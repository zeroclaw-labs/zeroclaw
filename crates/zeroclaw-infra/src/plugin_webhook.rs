//! Core-owned admission for channel-plugin webhooks.
//!
//! One ingress per daemon generation owns the route table the channel
//! supervisor publishes into, per-route queue admission, the request deadline,
//! and message dedup. Transports adapt their requests to
//! [`PluginWebhookIngress::dispatch`] and map its outcome.

use std::sync::Arc;

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::oneshot;
use zeroclaw_api::webhook::{
    MAX_WEBHOOK_RESPONSE_BODY_BYTES, PLUGIN_WEBHOOK_DEADLINE, PluginWebhookOutcome,
    PluginWebhookOwner, PluginWebhookRegistry, PluginWebhookRequest, WebhookCancellation,
    WebhookIdempotency, WebhookOutcome, WebhookReject, WebhookReservationStore,
    is_valid_plugin_webhook_path,
};

/// Route admission, the request deadline, and message dedup for one daemon
/// generation.
pub struct PluginWebhookIngress {
    registry: Arc<PluginWebhookRegistry>,
    reservations: Arc<WebhookReservationStore>,
}

impl PluginWebhookIngress {
    /// `dedup_ttl_secs` and `dedup_max_keys` are the raw
    /// `gateway.idempotency_ttl_secs` and `gateway.idempotency_max_keys`
    /// values.
    #[must_use]
    pub fn new(dedup_ttl_secs: u64, dedup_max_keys: usize) -> Self {
        Self {
            registry: Arc::new(PluginWebhookRegistry::new()),
            reservations: Arc::new(WebhookReservationStore::new(
                crate::effective_idempotency_ttl(dedup_ttl_secs),
                crate::normalize_max_keys(dedup_max_keys, crate::IDEMPOTENCY_MAX_KEYS_DEFAULT),
            )),
        }
    }

    /// The route table the channel supervisor publishes into.
    #[must_use]
    pub fn registry(&self) -> &Arc<PluginWebhookRegistry> {
        &self.registry
    }

    /// Deliver `request` to the worker that owns its path and wait for the
    /// outcome, at most [`PLUGIN_WEBHOOK_DEADLINE`] after enqueue.
    ///
    /// The ingress does not rate-limit. A transport applies the shared
    /// webhook rate limit before calling, and refuses a body over its ceiling
    /// before buffering it, as the gateway's HTTP adapter does;
    /// [`PluginWebhookRequest::new`] then re-checks the request bounds.
    ///
    /// Cancelling `cancel`, or dropping the returned future, cancels the
    /// worker's copy of the request.
    pub async fn dispatch(
        &self,
        request: PluginWebhookRequest,
        cancel: &WebhookCancellation,
    ) -> PluginWebhookOutcome {
        if cancel.is_cancelled() {
            return PluginWebhookOutcome::Cancelled;
        }
        if !is_valid_plugin_webhook_path(request.path()) {
            return PluginWebhookOutcome::NotFound;
        }
        let Some(route) = self.registry.get(request.path()) else {
            return PluginWebhookOutcome::NotFound;
        };
        let owner = route.owner().clone();
        let path = request.path().to_owned();
        let request_cancel = cancel.child_token();
        let _cancel_on_exit = request_cancel.clone().drop_guard();
        let idempotency = idempotency_bridge(&self.reservations, &owner, &path);
        let (reply, answer) = oneshot::channel();
        let raw = request.into_raw_webhook(request_cancel, Some(idempotency), reply);
        let sent = route.sink().try_send(raw);
        // A waiting request must not keep a retiring worker's queue open.
        drop(route);
        match sent {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return PluginWebhookOutcome::QueueFull,
            Err(TrySendError::Closed(_)) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(route_attrs(&owner, &path, "plugin_webhook_route_closed")),
                    "Channel plugin webhook route stopped accepting requests"
                );
                return PluginWebhookOutcome::Unavailable;
            }
        }

        let received = tokio::select! {
            biased;
            received = answer => received,
            () = cancel.cancelled() => return PluginWebhookOutcome::Cancelled,
            () = tokio::time::sleep(PLUGIN_WEBHOOK_DEADLINE) => {
                return PluginWebhookOutcome::Timeout;
            }
        };
        worker_outcome(received, cancel, &owner, &path)
    }
}

fn worker_outcome(
    received: Result<Result<WebhookOutcome, WebhookReject>, oneshot::error::RecvError>,
    cancel: &WebhookCancellation,
    owner: &PluginWebhookOwner,
    path: &str,
) -> PluginWebhookOutcome {
    match received {
        Ok(Ok(WebhookOutcome::Ack)) => PluginWebhookOutcome::Ack,
        Ok(Ok(WebhookOutcome::Body(body))) if body.len() > MAX_WEBHOOK_RESPONSE_BODY_BYTES => {
            PluginWebhookOutcome::InvalidResponse
        }
        Ok(Ok(WebhookOutcome::Body(body))) => PluginWebhookOutcome::Reply(body),
        Ok(Err(WebhookReject::Unauthorized(_))) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(route_attrs(owner, path, "plugin_webhook_unauthorized")),
                "Channel plugin rejected webhook authentication"
            );
            PluginWebhookOutcome::Unauthorized
        }
        Ok(Err(WebhookReject::BadRequest(_))) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(route_attrs(owner, path, "plugin_webhook_invalid")),
                "Channel plugin rejected malformed webhook"
            );
            PluginWebhookOutcome::BadRequest
        }
        Ok(Err(WebhookReject::Unavailable(_))) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(route_attrs(owner, path, "plugin_webhook_unavailable")),
                "Channel plugin webhook processing unavailable"
            );
            PluginWebhookOutcome::Unavailable
        }
        Ok(Err(WebhookReject::InvalidResponse)) => PluginWebhookOutcome::InvalidResponse,
        // The worker reports a timeout only when its request token fires, and
        // while dispatch still waits only the caller's cancellation fires it.
        Ok(Err(WebhookReject::Timeout)) if cancel.is_cancelled() => PluginWebhookOutcome::Cancelled,
        Ok(Err(WebhookReject::Timeout)) => PluginWebhookOutcome::Timeout,
        Err(_) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(route_attrs(owner, path, "plugin_webhook_reply_dropped")),
                "Channel plugin webhook worker dropped a request without an outcome"
            );
            PluginWebhookOutcome::Unavailable
        }
    }
}

fn route_attrs(owner: &PluginWebhookOwner, path: &str, error_key: &str) -> serde_json::Value {
    ::serde_json::json!({
        "plugin": owner.plugin(),
        "channel_alias": owner.channel_alias(),
        "path": path,
        "error_key": error_key,
    })
}

const DEDUP_KEY_DOMAIN: &[u8] = b"zeroclaw-plugin-webhook-dedup-v1";

/// Hex SHA-256 over the length-prefixed domain tag, owner, path, and message
/// ID, so no choice of component bytes can shift a boundary.
fn dedup_key(owner: &PluginWebhookOwner, path: &str, message_id: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    for component in [
        DEDUP_KEY_DOMAIN,
        owner.plugin().as_bytes(),
        owner.channel_alias().as_bytes(),
        path.as_bytes(),
        message_id.as_bytes(),
    ] {
        digest.update((component.len() as u64).to_be_bytes());
        digest.update(component);
    }
    format!("{:x}", digest.finalize())
}

fn idempotency_bridge(
    store: &Arc<WebhookReservationStore>,
    owner: &PluginWebhookOwner,
    path: &str,
) -> WebhookIdempotency {
    let begin = Arc::clone(store);
    let commit = Arc::clone(store);
    let rollback = Arc::clone(store);
    let owner = owner.clone();
    let path = path.to_owned();
    WebhookIdempotency::new(
        move |message_id| begin.begin(&dedup_key(&owner, &path, message_id)),
        move |token| commit.commit(token),
        move |token| rollback.rollback(token),
    )
}

#[cfg(test)]
mod tests;
