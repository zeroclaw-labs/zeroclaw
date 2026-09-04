//! Transport trait for RPC connections.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::security::auth_provider::Credential;

/// Which transport class a connection arrived on. Drives the handshake's
/// authentication rules: local IPC keeps a compatibility path, remote WSS
/// requires an explicit credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    /// Local IPC (Unix socket / Windows named pipe): same-host, gated by
    /// filesystem mode / pipe ACLs.
    Local,
    /// The remote WSS listener.
    Wss,
}

#[async_trait]
pub trait RpcTransport: Send + 'static {
    fn writer(&self) -> mpsc::Sender<String>;
    async fn next_frame(&mut self) -> Option<String>;
    fn peer_label(&self) -> String;

    /// Which transport class this connection arrived on.
    fn kind(&self) -> TransportKind;

    /// The transport-intrinsic credential: the kernel-reported peer uid on
    /// Unix sockets. [`Credential::None`] when the transport supplies
    /// nothing (remote WSS, Windows named pipes) — those connections
    /// authenticate by presenting an explicit credential in `initialize`.
    fn credential(&self) -> Credential {
        Credential::None
    }
}
