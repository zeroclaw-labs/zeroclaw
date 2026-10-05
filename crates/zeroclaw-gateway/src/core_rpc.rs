//! The gateway's RPC connection to the core.
//!
//! This is the strangler seam for the gateway split. While the gateway still
//! runs inside the daemon, it dials the dispatcher through the daemon's
//! in-process connector and holds the resulting [`RpcClient`] here; routes
//! that migrate onto RPC reach it through the `CoreRpc` request extension.
//! The cut-over later replaces the in-process dial with the real socket
//! without changing any route.
//!
//! The in-process transport never takes the daemon's local compatibility
//! path: an `initialize` without an explicit credential is refused with
//! `AUTH_REQUIRED`. Until the gateway has a credential of its own to present
//! (its service key, or a forwarded user bearer, both later work), the seam
//! therefore stays idle by design; that outcome is logged once at INFO and
//! not retried, because the daemon's policy does not change until a reload
//! restarts this generation. Other handshake refusals are logged at WARN and
//! not retried either; transport failures retry with backoff.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use zeroclaw_rpc_client::{Backoff, ClientError, ConnectOptions, RpcClient};
use zeroclaw_runtime::rpc::inproc::InprocConnector;

/// Handle to the core connection, shared with every request as an axum
/// extension. Cheap to clone.
#[derive(Clone, Default)]
pub struct CoreRpc {
    client: Arc<RwLock<Option<Arc<RpcClient>>>>,
}

impl CoreRpc {
    /// The live client, or `None` while disconnected or unattached.
    pub async fn client(&self) -> Option<Arc<RpcClient>> {
        self.client.read().await.clone()
    }

    /// Whether a connection is currently established.
    pub async fn is_connected(&self) -> bool {
        self.client.read().await.is_some()
    }

    /// Dial the core through `connector`, presenting `options` in every
    /// handshake, and keep the handle current across reconnects until the
    /// generation is cancelled.
    pub fn attach_inproc(&self, connector: InprocConnector, options: ConnectOptions) {
        let slot = Arc::clone(&self.client);
        zeroclaw_spawn::spawn!(maintain(connector, options, slot));
    }
}

async fn maintain(
    connector: InprocConnector,
    options: ConnectOptions,
    slot: Arc<RwLock<Option<Arc<RpcClient>>>>,
) {
    let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(5));
    loop {
        let Some(stream) = connector.connect().await else {
            // The generation is over; nothing to reconnect to.
            return;
        };
        match RpcClient::connect_over(stream, options.clone()).await {
            Ok(client) => {
                backoff.reset();
                let client = Arc::new(client);
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "principal_id": client.handshake().principal_id,
                            "capabilities": client.handshake().capabilities.len(),
                        })),
                    "gateway connected to the core over the in-process RPC seam"
                );
                *slot.write().await = Some(Arc::clone(&client));
                client.closed().await;
                *slot.write().await = None;
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "gateway lost its in-process RPC connection; reconnecting"
                );
            }
            Err(ClientError::Rpc(error))
                if error.code == zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED =>
            {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({ "code": error.code })),
                    "in-process RPC seam idle: the gateway has no credential to present yet"
                );
                return;
            }
            Err(ClientError::Rpc(error)) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "code": error.code,
                            "message": error.message,
                        })),
                    "core refused the gateway's in-process RPC handshake; routes stay on the in-process path"
                );
                return;
            }
            Err(error) => {
                let delay = backoff.next_delay();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "error": error.to_string(),
                            "retry_in_ms": delay.as_millis(),
                        })),
                    "in-process RPC handshake failed; retrying"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unattached_seam_reports_no_client() {
        let core = CoreRpc::default();
        assert!(!core.is_connected().await);
        assert!(core.client().await.is_none());
    }
}
