//! Client for the ZeroClaw daemon RPC.
//!
//! The daemon serves NDJSON JSON-RPC 2.0 over a local socket, a Windows
//! named pipe, or (inside the daemon process) an in-memory duplex. This
//! crate is the client half of that contract: it dials, runs the
//! `initialize` handshake, multiplexes requests and their responses,
//! delivers notifications and server-initiated requests, and knows how to
//! back off between reconnect attempts. It depends on the wire contract in
//! `zeroclaw-rpc-proto` and the envelope types in `zeroclaw-api`, and on
//! nothing from the runtime.
//!
//! [`RpcClient::connect_over`] accepts any byte stream, so the same client
//! serves the gateway's in-process seam today and the separate gateway
//! process later; [`RpcClient::connect_local`] dials the daemon endpoint
//! that [`endpoint::resolve_socket_path`] names.

// Like `apps/zerocode`, this is a standalone RPC client: it must not link
// `zeroclaw-log`, so it cannot use `::zeroclaw_spawn::spawn!`, and its two
// tasks (transport reader and writer) carry no daemon attribution span to
// inherit. The workspace ban on `tokio::spawn` exists for daemon paths; see
// the exemption list in `clippy.toml`.
#![allow(clippy::disallowed_methods)]

pub mod backoff;
pub mod client;
pub mod endpoint;

pub use backoff::Backoff;
pub use client::{
    ClientError, ConnectOptions, ConnectionState, DEFAULT_HANDSHAKE_TIMEOUT,
    DEFAULT_REQUEST_TIMEOUT, InboundRequest, Notification, RpcClient,
};
pub use zeroclaw_rpc_proto::{Method, RPC_PROTOCOL_VERSION};
