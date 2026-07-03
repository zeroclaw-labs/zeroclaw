//! Transport trait for RPC connections.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::security::auth_provider::Credential;

/// Which listener produced a connection. Drives the RFC #7141 enforcement
/// posture in `initialize`: WSS connections are always resolved through the
/// provider registry when one is configured; local IPC connections fall back
/// to legacy trust when the platform surfaces no peer credential, because the
/// endpoint itself is already filesystem-permission gated (0o600).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    Local,
    Wss,
}

#[async_trait]
pub trait RpcTransport: Send + 'static {
    fn writer(&self) -> mpsc::Sender<String>;
    async fn next_frame(&mut self) -> Option<String>;
    fn peer_label(&self) -> String;
    fn kind(&self) -> TransportKind;

    /// The transport-level credential this connection carries intrinsically
    /// (e.g. the Unix-socket peer uid). Consulted by the `initialize` auth
    /// handshake when the client presents no explicit credential. Defaults to
    /// [`Credential::None`] for transports with no intrinsic identity.
    fn credential(&self) -> Credential {
        Credential::None
    }
}
