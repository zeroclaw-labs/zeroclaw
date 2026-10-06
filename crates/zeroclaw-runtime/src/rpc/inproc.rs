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
use super::dispatch::{RpcAccessPolicy, RpcDispatcher};
use super::local::{
    LOCAL_PEER_WRITE_TIMEOUT, MAX_FRAME_BYTES, SHUTDOWN_TIMEOUT, TERMINAL_FRAME_TIMEOUT,
    TerminalFrame, WriterTimeouts, frame_too_large_line, run_writer,
};
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
    /// A last frame for the peer, written ahead of the ordinary queue.
    terminal_tx: mpsc::Sender<TerminalFrame>,
}

impl InprocTransport {
    /// Wrap the daemon's half of a duplex. The writer is the local socket's
    /// hardened writer: a per-frame deadline that cancels a peer which stops
    /// reading, and a terminal-frame lane for the reason a connection closes.
    /// The writer task ends on `cancel`.
    pub fn new(stream: DuplexStream, cancel: CancellationToken) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
        let (writer_tx, writer_rx) = mpsc::channel::<String>(64);
        let (terminal_tx, terminal_rx) = mpsc::channel::<TerminalFrame>(1);
        zeroclaw_spawn::spawn!(run_writer(
            write_half,
            writer_rx,
            terminal_rx,
            cancel,
            WriterTimeouts {
                write: LOCAL_PEER_WRITE_TIMEOUT,
                shutdown: SHUTDOWN_TIMEOUT,
            },
            PEER_LABEL.to_string(),
        ));
        Self {
            reader: BufReader::new(read_half),
            writer_tx,
            terminal_tx,
        }
    }

    /// Tell the peer why its connection is closing, ahead of the ordinary
    /// queue; the writer stops after it. Bounded by `TERMINAL_FRAME_TIMEOUT`
    /// like the local socket's, so a peer that will not read it is closed
    /// anyway.
    async fn send_terminal(&self, line: String) {
        let (written, written_rx) = tokio::sync::oneshot::channel();
        let _ = tokio::time::timeout(TERMINAL_FRAME_TIMEOUT, async {
            if self
                .terminal_tx
                .send(TerminalFrame { line, written })
                .await
                .is_err()
            {
                return;
            }
            let _ = written_rx.await;
        })
        .await;
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
                    // Same refusal as the local socket: the peer learns why
                    // before it sees end of stream.
                    self.send_terminal(frame_too_large_line()).await;
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
    /// Live in-process activity: one for each accepted connection task plus
    /// every task it started, released only when that task has ended. This is
    /// the connector's contribution to the daemon's generation-drain proof.
    connections: Arc<AtomicUsize>,
    /// Admission and retirement share this lock: a connection is admitted
    /// (final cancellation check, activity registered, task inserted) in one
    /// critical section, and retirement seals admission and takes the task
    /// set in another, so every connection either belongs to the drain or is
    /// refused. A detached task, or a check outside the lock, would leave a
    /// window in which the retired generation could still acquire work.
    registry: std::sync::Mutex<Registry>,
    /// Test hook: a `connect` parks here, after it has the context and before
    /// it takes the registry lock, until the gate reads `true`.
    #[cfg(test)]
    admission_gate: Option<watch::Receiver<bool>>,
}

struct Registry {
    /// `false` once retirement has begun; no further connection is admitted.
    open: bool,
    /// The accepted connection tasks, owned here so a retiring generation can
    /// join them, and abort the ones that ignore cancellation, before it hands
    /// over.
    tasks: tokio::task::JoinSet<()>,
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
                registry: std::sync::Mutex::new(Registry {
                    open: true,
                    tasks: tokio::task::JoinSet::new(),
                }),
                #[cfg(test)]
                admission_gate: None,
            }),
        }
    }

    /// A connector whose `connect` parks at the admission boundary until
    /// `gate` reads `true`, so a test can retire the generation while a
    /// connect is between its context wait and its admission.
    #[cfg(test)]
    pub(crate) fn with_admission_gate(
        cancel: CancellationToken,
        gate: watch::Receiver<bool>,
    ) -> Self {
        let (ctx, _initial_rx) = watch::channel(None);
        Self {
            inner: Arc::new(ConnectorInner {
                ctx,
                cancel,
                connections: Arc::new(AtomicUsize::new(0)),
                registry: std::sync::Mutex::new(Registry {
                    open: true,
                    tasks: tokio::task::JoinSet::new(),
                }),
                admission_gate: Some(gate),
            }),
        }
    }

    /// Connection tasks the registry currently owns (test observation).
    #[cfg(test)]
    pub(crate) fn registered_tasks(&self) -> usize {
        self.inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tasks
            .len()
    }

    /// One more unit of this generation's in-process activity, the token a
    /// prompt task spawned by an in-process connection holds until it ends
    /// (test observation of the drain boundary without a model provider).
    #[cfg(test)]
    pub(crate) fn activity_token(&self) -> super::ConnectionActivity {
        super::ConnectionActivity::new(Arc::clone(&self.inner.connections))
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

    /// Live in-process activity: accepted connections plus the tasks they
    /// started, each counted until it has ended. The daemon adds this to its
    /// socket count when deciding whether the generation has drained; it is
    /// deliberately kept out of the ephemeral external-client count.
    pub fn connection_count(&self) -> usize {
        self.inner.connections.load(Ordering::Relaxed)
    }

    /// Seal admission: from this call on, every `connect` returns `None`.
    /// Idempotent; [`InprocConnector::drain`] seals as well. The daemon calls
    /// this before it starts counting the generation's remaining activity, so
    /// a zero it observes cannot be followed by a late admission.
    pub fn close_admission(&self) {
        self.inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open = false;
    }

    /// Retire the accepted connections of this generation.
    ///
    /// Seals admission and takes the task set in one critical section, then
    /// (after cancelling the generation's token, which ends every
    /// connection's dispatcher) waits up to the listeners' connection drain grace
    /// for the connection tasks to finish unwinding on their own, and aborts
    /// and joins whatever is left, the same sequence the local socket listener
    /// applies to its accepted connections. Returns how many connection tasks
    /// had to be aborted; a nonzero count means the retiring generation could
    /// not prove those connections finished cooperatively.
    pub async fn drain(&self) -> usize {
        let mut tasks = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            registry.open = false;
            std::mem::take(&mut registry.tasks)
        };
        tokio::select! {
            () = async {
                while tasks.join_next().await.is_some() {}
            } => 0,
            () = tokio::time::sleep(super::CONNECTION_DRAIN_GRACE) => {
                let aborted = tasks.len();
                tasks.shutdown().await;
                aborted
            }
        }
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
        #[cfg(test)]
        if let Some(gate) = &self.inner.admission_gate {
            let mut gate = gate.clone();
            while !*gate.borrow_and_update() {
                if gate.changed().await.is_err() {
                    return None;
                }
            }
        }
        // Admission is one critical section shared with retirement: the
        // cancellation and seal checks, the activity registration and the
        // task insertion happen under the registry lock, so a connect that
        // gets here either lands in the set the drain will join or is
        // refused. Nothing below awaits while the lock is held.
        let mut registry = self
            .inner
            .registry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !registry.open || self.inner.cancel.is_cancelled() {
            return None;
        }
        let (client_half, server_half) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
        let conn_cancel = self.inner.cancel.child_token();
        let activity = super::ConnectionActivity::new(Arc::clone(&self.inner.connections));
        // Reap the connections that already ended so the set only holds live
        // tasks; `JoinSet` keeps finished entries until polled.
        while registry.tasks.try_join_next().is_some() {}
        registry
            .tasks
            .spawn(serve(ctx, server_half, conn_cancel, activity));
        drop(registry);
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
    // Admitted under the lock but cancelled before this task ran: do not
    // start a dispatcher for a generation that is already retiring. The
    // activity token drops here, so the drain sees this task end.
    if conn_cancel.is_cancelled() {
        return;
    }
    let _count_guard = activity.clone();
    let mut transport = InprocTransport::new(stream, conn_cancel.clone());
    let writer_tx = transport.writer();
    let mut dispatcher = RpcDispatcher::new_with_cancel_and_channel_access(
        Arc::clone(&ctx),
        writer_tx,
        transport.peer_label(),
        conn_cancel.clone(),
        RpcAccessPolicy::RemoteSessionOwner,
        None,
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
    use std::time::Duration;
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

    /// Give `ctx` a TUI identity signing key, as a daemon with a
    /// `.secret_key` has. The duplex is served under the non-local session
    /// policy, whose `initialize` refuses every caller while signing is off,
    /// so a test about the credential layer needs signing on to reach it.
    fn with_tui_signing(mut ctx: Arc<RpcContext>, key_dir: &std::path::Path) -> Arc<RpcContext> {
        std::fs::write(key_dir.join(".secret_key"), "42".repeat(32)).unwrap();
        Arc::get_mut(&mut ctx)
            .expect("a fresh context has one owner")
            .tui_registry = Arc::new(crate::rpc::tui_identity::TuiRegistry::new(key_dir));
        assert!(ctx.tui_registry.signing_is_enabled());
        ctx
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
        let ctx = with_tui_signing(ctx_for(config), tmp.path());
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
        let ctx = with_tui_signing(ctx_for(config), tmp.path());
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

        writer
            .write_all(
                rpc_request(
                    Method::SessionCancel,
                    &serde_json::json!({ "session_id": "not-owned-by-this-connection" }),
                    3,
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let frame = read_frame(&mut reader).await;
        assert_eq!(
            frame["error"]["code"],
            zeroclaw_api::jsonrpc::error_codes::SESSION_NOT_OWNED,
            "non-inspection methods retain the duplex session owner gate"
        );

        cancel.cancel();
        drop(writer);
    }

    /// The duplex is a non-local caller, so the remote identity fence applies
    /// to it as it does to WSS: while TUI identity signing is off, even a
    /// valid paired token does not open it.
    #[tokio::test]
    async fn duplex_with_a_paired_token_is_refused_while_tui_signing_is_off() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = base_config(tmp.path());
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec!["zc_inproc_test_token".to_string()];
        let ctx = ctx_for(config);
        assert!(!ctx.tui_registry.signing_is_enabled());
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
        assert_eq!(
            frame["error"]["code"],
            zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED,
            "the duplex must not bypass the remote identity fence: {frame}"
        );
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
        let ctx = with_tui_signing(ctx_for(config), tmp.path());
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

    /// The generation owns its accepted in-process connections: cancelling it
    /// and draining the connector ends every connection task, the client sees
    /// EOF, and the activity count is zero, which is what the daemon's reload
    /// decision reads.
    #[tokio::test]
    async fn generation_drain_joins_accepted_connections_and_zeroes_the_count() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(base_config(tmp.path()));
        let cancel = CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(ctx);
        let client = connector.connect().await.expect("bound connector connects");
        let (read_half, _write_half) = tokio::io::split(client);
        let mut reader = tokio::io::BufReader::new(read_half);
        // Let the connection task start so it is counted and owned.
        tokio::task::yield_now().await;
        assert_eq!(
            connector.connection_count(),
            1,
            "accepted connection is counted"
        );

        cancel.cancel();
        let aborted = tokio::time::timeout(Duration::from_secs(10), connector.drain())
            .await
            .expect("drain returns inside its budget");
        assert_eq!(aborted, 0, "a cancelled connection unwinds cooperatively");
        assert_eq!(
            connector.connection_count(),
            0,
            "no in-process activity may outlive the drained generation"
        );
        let mut line = String::new();
        let eof = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .expect("peer answers after drain")
            .expect("read");
        assert_eq!(eof, 0, "the client sees EOF once its connection is retired");
    }

    /// A connect parked at the admission boundary while the generation
    /// retires must be refused, not admitted into a drained generation: the
    /// seal and the final cancellation check live inside the admission
    /// critical section, so nothing registers after the drain took the set.
    #[tokio::test]
    async fn connect_paused_at_admission_is_refused_once_retirement_is_sealed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(base_config(tmp.path()));
        let cancel = CancellationToken::new();
        let (gate_tx, gate_rx) = watch::channel(false);
        let connector = InprocConnector::with_admission_gate(cancel.clone(), gate_rx);
        connector.bind(ctx);
        let parked = connector.clone();
        let pending = zeroclaw_spawn::spawn!(async move { parked.connect().await.is_some() });
        // Let the connect reach the gate: it has passed the bind wait and
        // the optimistic checks and is about to admit itself.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(connector.registered_tasks(), 0);

        // Retire the generation while the connect is parked.
        cancel.cancel();
        let aborted = connector.drain().await;
        assert_eq!(aborted, 0);
        assert_eq!(connector.connection_count(), 0);

        // Resume the connect: it reaches admission after the seal.
        gate_tx.send(true).unwrap();
        let admitted = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .expect("parked connect finishes")
            .expect("connect task");
        assert!(
            !admitted,
            "a connect that reaches admission after retirement must be refused"
        );
        assert_eq!(
            connector.connection_count(),
            0,
            "no activity after the drain"
        );
        assert_eq!(
            connector.registered_tasks(),
            0,
            "no task registered after the drain"
        );
    }

    /// Many connects racing retirement: each one either belongs to the drain
    /// (its connection is joined, the client sees EOF) or is refused; none
    /// leaves activity or a task behind once the drain has returned.
    #[tokio::test]
    async fn concurrent_connects_racing_retirement_are_drained_or_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_for(base_config(tmp.path()));
        let cancel = CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(ctx);
        let mut connects = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let c = connector.clone();
            connects.spawn(async move {
                tokio::task::yield_now().await;
                c.connect().await
            });
        }
        tokio::task::yield_now().await;
        cancel.cancel();
        let _aborted = connector.drain().await;
        let mut admitted = 0usize;
        while let Some(result) = connects.join_next().await {
            if let Some(client) = result.expect("connect task") {
                admitted += 1;
                let (read_half, _write_half) = tokio::io::split(client);
                let mut reader = tokio::io::BufReader::new(read_half);
                let mut line = String::new();
                let n = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
                    .await
                    .expect("an admitted connection is retired by the drain")
                    .expect("read");
                assert_eq!(n, 0, "admitted connections end with EOF");
            }
        }
        assert_eq!(connector.connection_count(), 0, "admitted={admitted}");
        assert_eq!(connector.registered_tasks(), 0, "admitted={admitted}");
        assert!(
            connector.connect().await.is_none(),
            "admission stays sealed"
        );
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
