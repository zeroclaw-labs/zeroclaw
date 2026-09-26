//! In-process RPC connections over an in-memory duplex.
//!
//! The daemon's own gateway generation dials the dispatcher through this
//! module instead of through the socket, which is the strangler seam for the
//! gateway split: routes move onto the RPC client one at a time while the
//! gateway still runs in the daemon process, and the cut-over later swaps the
//! duplex for the real socket without touching the routes.
//!
//! An in-process connection is classified [`TransportKind::Inproc`] and
//! presents [`Credential::None`]. Unlike a socket client, it is never eligible
//! for the local compatibility path: with or without a roster, and whatever
//! `require_pairing` says, an `initialize` that carries no explicit credential
//! is refused with `AUTH_REQUIRED`. Running inside the daemon process vouches
//! for nothing, and no peer uid exists for `security.trust_daemon_uid` to
//! match, so an anonymous in-process caller can neither become the shared
//! operator nor the trusted daemon uid. The credential the gateway presents
//! arrives with later work (its service key, or a forwarded user bearer).
//!
//! In-process connections are counted separately from socket clients, so an
//! `--ephemeral` daemon still exits when its last real client leaves.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader, DuplexStream};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::context::RpcContext;
use super::dispatch::RpcDispatcher;
use super::local::{MAX_FRAME_BYTES, SHUTDOWN_TIMEOUT, run_writer};
use super::transport::{RpcTransport, TransportKind};
use crate::security::auth_provider::Credential;

/// Bytes buffered in each direction of the duplex before a writer parks.
const DUPLEX_BUFFER_BYTES: usize = 64 * 1024;

/// Peer label reported for every in-process connection. The `proto:` prefix
/// is what the TUI registry reads as the connection's transport name.
pub const PEER_LABEL: &str = "inproc:gateway";

/// Server side of one in-process connection.
pub struct InprocTransport {
    reader: BufReader<tokio::io::ReadHalf<DuplexStream>>,
    writer_tx: mpsc::Sender<String>,
}

impl InprocTransport {
    /// Wrap the daemon's half of a duplex. The writer task ends on `cancel`.
    pub fn new(stream: DuplexStream, cancel: CancellationToken) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
        let (writer_tx, writer_rx) = mpsc::channel::<String>(64);
        zeroclaw_spawn::spawn!(run_writer(write_half, writer_rx, cancel, SHUTDOWN_TIMEOUT));
        Self {
            reader: BufReader::new(read_half),
            writer_tx,
        }
    }
}

#[async_trait]
impl RpcTransport for InprocTransport {
    fn writer(&self) -> mpsc::Sender<String> {
        self.writer_tx.clone()
    }

    async fn next_frame(&mut self) -> Option<String> {
        let mut buf: Vec<u8> = Vec::new();
        let mut limited = (&mut self.reader).take(MAX_FRAME_BYTES + 1);
        match limited.read_until(b'\n', &mut buf).await {
            Ok(0) => None,
            Ok(_) => {
                if buf.len() as u64 > MAX_FRAME_BYTES {
                    return None;
                }
                Some(String::from_utf8_lossy(&buf).into_owned())
            }
            Err(_) => None,
        }
    }

    fn peer_label(&self) -> String {
        PEER_LABEL.to_string()
    }

    fn kind(&self) -> TransportKind {
        // Its own class, not `Local`: the authenticator refuses the
        // no-credential compatibility path for it.
        TransportKind::Inproc
    }

    fn credential(&self) -> Credential {
        // Deliberately no peer credential: nothing about running inside the
        // daemon process makes the caller the daemon's uid.
        Credential::None
    }
}

/// Hands out in-process connections to the current daemon generation.
///
/// The daemon creates the connector before it has a [`RpcContext`], gives it
/// to the gateway starter, and calls [`InprocConnector::bind`] once the
/// generation's context exists; [`InprocConnector::connect`] waits for that
/// bind. Cancelling the generation ends every connection and every waiter.
#[derive(Clone)]
pub struct InprocConnector {
    inner: Arc<ConnectorInner>,
}

struct ConnectorInner {
    ctx: watch::Sender<Option<Arc<RpcContext>>>,
    cancel: CancellationToken,
    connections: Arc<AtomicUsize>,
}

impl std::fmt::Debug for InprocConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InprocConnector")
            .field("bound", &self.is_bound())
            .field("connections", &self.connection_count())
            .finish()
    }
}

impl InprocConnector {
    /// A connector for one daemon generation; `cancel` is that generation's
    /// token.
    pub fn new(cancel: CancellationToken) -> Self {
        let (ctx, _initial_rx) = watch::channel(None);
        Self {
            inner: Arc::new(ConnectorInner {
                ctx,
                cancel,
                connections: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }

    /// Attach the generation's RPC context. Waiters in
    /// [`InprocConnector::connect`] proceed once this is called.
    pub fn bind(&self, ctx: Arc<RpcContext>) {
        let _previous = self.inner.ctx.send_replace(Some(ctx));
    }

    /// Whether [`InprocConnector::bind`] has happened.
    pub fn is_bound(&self) -> bool {
        self.inner.ctx.borrow().is_some()
    }

    /// Live in-process connections served by this connector.
    pub fn connection_count(&self) -> usize {
        self.inner.connections.load(Ordering::Relaxed)
    }

    /// Open one in-process connection and return the client's half. `None`
    /// when the generation is cancelled before a context is bound.
    pub async fn connect(&self) -> Option<DuplexStream> {
        let ctx = {
            let mut rx = self.inner.ctx.subscribe();
            loop {
                let bound: Option<Arc<RpcContext>> = (*rx.borrow_and_update()).clone();
                if let Some(ctx) = bound {
                    break ctx;
                }
                tokio::select! {
                    () = self.inner.cancel.cancelled() => return None,
                    changed = rx.changed() => {
                        if changed.is_err() {
                            return None;
                        }
                    }
                }
            }
        };
        if self.inner.cancel.is_cancelled() {
            return None;
        }
        let (client_half, server_half) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
        let conn_cancel = self.inner.cancel.child_token();
        let activity = super::ConnectionActivity::new(Arc::clone(&self.inner.connections));
        zeroclaw_spawn::spawn!(serve(ctx, server_half, conn_cancel, activity));
        Some(client_half)
    }
}

/// One connection's lifetime. Mirrors the local listener's connection task
/// on purpose: the sequencing (run, cancel, drain, unregister) stays in the
/// task body itself because the dispatcher future is large enough that an
/// extra nested async frame can overflow a Tokio worker stack.
async fn serve(
    ctx: Arc<RpcContext>,
    stream: DuplexStream,
    conn_cancel: CancellationToken,
    activity: super::ConnectionActivity,
) {
    let _count_guard = activity.clone();
    let mut transport = InprocTransport::new(stream, conn_cancel.clone());
    let writer_tx = transport.writer();
    let mut dispatcher = RpcDispatcher::new_with_connection_cancel(
        Arc::clone(&ctx),
        writer_tx,
        transport.peer_label(),
        conn_cancel.clone(),
    )
    .with_connection_activity(activity)
    .with_transport(transport.kind(), transport.credential());
    tokio::select! {
        () = dispatcher.run(&mut transport) => {}
        () = conn_cancel.cancelled() => {}
    }
    dispatcher.shutdown().await;
    if let Some((tui_id, tui_epoch)) = dispatcher.tui_registration() {
        ctx.tui_registry.unregister(tui_id, tui_epoch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::session::SessionStore;
    use crate::rpc::types::{InitializeParams, InitializeResult, StatusResult};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_api::jsonrpc::JsonRpcRequest;
    use zeroclaw_config::schema::{Config, PermissionProfileConfig, UserConfig};
    use zeroclaw_infra::session_queue::SessionActorQueue;
    use zeroclaw_rpc_proto::Method;

    fn base_config(tmp: &std::path::Path) -> Config {
        Config {
            data_dir: tmp.to_path_buf(),
            config_path: tmp.join("config.toml"),
            ..Config::default()
        }
    }

    fn ctx_for(config: Config) -> Arc<RpcContext> {
        let session_queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(SessionStore::new(64, session_queue));
        RpcContext::minimal(config, sessions)
    }

    fn rpc_request<T: serde::Serialize>(method: Method, params: &T, id: u64) -> String {
        let req = JsonRpcRequest::new(
            method.wire_name(),
            serde_json::to_value(params).unwrap(),
            serde_json::Value::Number(id.into()),
        );
        let mut s = serde_json::to_string(&req).unwrap();
        s.push('\n');
        s
    }

    fn initialize_params() -> InitializeParams {
        InitializeParams {
            protocol_version: 1,
            tui_id: None,
            tui_sig: None,
            env: Default::default(),
            client_capabilities: None,
            auth_token: None,
            auth_provider: None,
        }
    }

    async fn read_frame(
        reader: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>,
    ) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[tokio::test]
    async fn duplex_initialize_without_a_credential_is_refused_even_with_no_roster() {
        // A socket client with no roster takes the local compatibility path
        // and becomes the shared operator. The duplex must not: an anonymous
        // in-process caller gets AUTH_REQUIRED whatever `require_pairing` says.
        let tmp = tempfile::tempdir().unwrap();
        let mut config = base_config(tmp.path());
        config.gateway.require_pairing = false;
        assert!(
            config.users.is_empty(),
            "this test is about the no-roster case"
        );
        let ctx = ctx_for(config);
        let cancel = CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        assert!(!connector.is_bound());
        connector.bind(ctx);
        assert!(connector.is_bound());

        let stream = connector.connect().await.expect("bound connector connects");
        assert_eq!(connector.connection_count(), 1);
        let (read_half, mut writer) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        writer
            .write_all(rpc_request(Method::Initialize, &initialize_params(), 1).as_bytes())
            .await
            .unwrap();
        let frame = read_frame(&mut reader).await;
        assert!(
            frame["error"].is_object(),
            "an anonymous duplex initialize must be refused: {frame}"
        );
        assert_eq!(
            frame["error"]["code"],
            zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED
        );
        cancel.cancel();
        drop(writer);
    }

    #[tokio::test]
    async fn duplex_initialize_with_a_paired_token_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = base_config(tmp.path());
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec!["zc_inproc_test_token".to_string()];
        let ctx = ctx_for(config);
        let cancel = CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(ctx);

        let stream = connector.connect().await.expect("bound connector connects");
        let (read_half, mut writer) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let params = InitializeParams {
            auth_token: Some("zc_inproc_test_token".to_string()),
            ..initialize_params()
        };
        writer
            .write_all(rpc_request(Method::Initialize, &params, 1).as_bytes())
            .await
            .unwrap();
        let frame = read_frame(&mut reader).await;
        assert!(frame["error"].is_null(), "unexpected RPC error: {frame}");
        let init: InitializeResult = serde_json::from_value(frame["result"].clone()).unwrap();
        assert_eq!(init.protocol_version, 1);
        assert!(!init.server_version.is_empty());

        writer
            .write_all(rpc_request(Method::Status, &serde_json::json!({}), 2).as_bytes())
            .await
            .unwrap();
        let frame = read_frame(&mut reader).await;
        assert!(frame["error"].is_null(), "unexpected RPC error: {frame}");
        let status: StatusResult = serde_json::from_value(frame["result"].clone()).unwrap();
        assert_eq!(status.active_sessions, 0);

        cancel.cancel();
        drop(writer);
    }

    #[tokio::test]
    async fn duplex_with_a_roster_denies_a_tokenless_initialize() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = base_config(tmp.path());
        config.permission_profiles.insert(
            "operator".into(),
            PermissionProfileConfig {
                grants: std::collections::HashMap::from([(Resource::Sessions, vec![Verb::Read])]),
                ..PermissionProfileConfig::default()
            },
        );
        config.users.insert(
            "alice".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4242),
                permission_profiles: vec!["operator".into()],
            },
        );
        let ctx = ctx_for(config);
        let cancel = CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(ctx);

        let stream = connector.connect().await.expect("bound connector connects");
        let (read_half, mut writer) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        writer
            .write_all(rpc_request(Method::Initialize, &initialize_params(), 1).as_bytes())
            .await
            .unwrap();
        let frame = read_frame(&mut reader).await;
        assert!(
            frame["error"].is_object(),
            "a roster must close the no-credential path: {frame}"
        );
        assert_eq!(
            frame["error"]["code"],
            zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn duplex_transport_has_its_own_class_and_presents_no_peer_credential() {
        let (_client_half, server_half) = tokio::io::duplex(1024);
        let cancel = CancellationToken::new();
        let transport = InprocTransport::new(server_half, cancel.clone());
        assert_eq!(transport.kind(), TransportKind::Inproc);
        assert!(matches!(transport.credential(), Credential::None));
        assert_eq!(transport.peer_label(), PEER_LABEL);
        assert!(
            PEER_LABEL.starts_with("inproc:"),
            "the TUI registry derives the transport name from the label prefix"
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn connect_waits_for_bind_and_gives_up_on_cancel() {
        let cancel = CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        let waiter = connector.clone();
        let pending = zeroclaw_spawn::spawn!(async move { waiter.connect().await.is_some() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !pending.is_finished(),
            "connect must wait for a bound context"
        );
        cancel.cancel();
        let connected = tokio::time::timeout(std::time::Duration::from_secs(2), pending)
            .await
            .expect("waiter ends on cancel")
            .expect("waiter task did not panic");
        assert!(!connected, "a cancelled generation hands out no connection");
    }
}
