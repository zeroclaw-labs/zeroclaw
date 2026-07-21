//! Local IPC transport for the RPC layer.

use super::context::RpcContext;
use super::dispatch::RpcDispatcher;
use super::transport::RpcTransport;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::Config;

use platform::LocalStream;

const MAX_FRAME_BYTES: u64 = 8 * 1024 * 1024;

/// Backoff after a transient `accept()` error so the serve loop does not
/// hot-spin while the condition (e.g. fd exhaustion) clears.
const ACCEPT_ERROR_BACKOFF_MS: u64 = 50;

/// File-descriptor exhaustion errno values, stable across the Unix targets
/// we support (Linux, macOS, BSD).
#[cfg(unix)]
const EMFILE: i32 = 24; // too many open files (this process)
#[cfg(unix)]
const ENFILE: i32 = 23; // too many open files (system-wide)

fn is_recoverable_accept_error(e: &std::io::Error) -> bool {
    if matches!(
        e.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::Interrupted | ErrorKind::WouldBlock
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(e.raw_os_error(), Some(EMFILE) | Some(ENFILE)) {
        return true;
    }
    false
}

pub fn socket_path(config: &Config) -> PathBuf {
    if let Ok(p) = std::env::var("ZEROCLAW_SOCKET") {
        return PathBuf::from(p);
    }
    platform::default_endpoint(&config.data_dir)
}

// ── Transport ────────────────────────────────────────────────────

/// Platform-neutral half-write type produced by `tokio::io::split`.
type LocalWriteHalf = tokio::io::WriteHalf<LocalStream>;
/// Platform-neutral half-read type produced by `tokio::io::split`.
type LocalReadHalf = tokio::io::ReadHalf<LocalStream>;

pub struct LocalTransport {
    reader: BufReader<LocalReadHalf>,
    writer_tx: mpsc::Sender<String>,
    peer_label: String,
}

impl LocalTransport {
    pub fn new(stream: LocalStream) -> Self {
        let peer_label = platform::peer_label_from(&stream);
        let (read_half, write_half) = tokio::io::split(stream);

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(64);
        zeroclaw_spawn::spawn!(async move {
            let mut writer: LocalWriteHalf = write_half;
            while let Some(mut line) = writer_rx.recv().await {
                if !line.ends_with('\n') {
                    line.push('\n');
                }
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        Self {
            reader: BufReader::new(read_half),
            writer_tx,
            peer_label,
        }
    }
}

#[async_trait]
impl RpcTransport for LocalTransport {
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
        self.peer_label.clone()
    }
}

// ── Listener ─────────────────────────────────────────────────────

/// Run the local IPC RPC listener as a daemon subsystem.
/// `client_count` is incremented on connect, decremented on disconnect.
/// The daemon uses it for `--ephemeral` shutdown logic.
pub async fn run_local_listener(
    ctx: Arc<RpcContext>,
    cancel: CancellationToken,
    client_count: Arc<AtomicUsize>,
) -> Result<()> {
    let path = {
        let config = ctx.config.read();
        socket_path(&config)
    };

    platform::prepare_parent(&path).await?;
    platform::remove_stale(&path).await?;

    let mut listener = platform::bind(&path).context("binding local IPC endpoint")?;

    platform::secure_endpoint(&path).await;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"path": path.display().to_string()})),
        "RPC local IPC listening"
    );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "RPC local IPC shutting down"
                );
                break;
            }
            accept = platform::accept(&mut listener, &path) => {
                let stream = match accept {
                    Ok(v) => v,
                    Err(e) => {
                        if e.downcast_ref::<std::io::Error>().is_some_and(is_recoverable_accept_error) {
                            // Transient (e.g. EMFILE under fd pressure):
                            // the listener is still valid. Back off briefly
                            // to avoid hot-spinning, then keep serving
                            // rather than killing the daemon
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("local IPC accept() transient error: {e}")
                            );
                            tokio::time::sleep(Duration::from_millis(ACCEPT_ERROR_BACKOFF_MS)).await;
                            continue;
                        }
                        return Err(e).context("local IPC accept error");
                    }
                };

                let ctx = ctx.clone();
                let count = client_count.clone();

                count.fetch_add(1, Ordering::Relaxed);

                zeroclaw_spawn::spawn!(async move {
                    let mut transport = LocalTransport::new(stream);
                    let peer = transport.peer_label();
                    let writer_tx = transport.writer();
                    let mut dispatcher = RpcDispatcher::new(ctx.clone(), writer_tx, peer);
                    dispatcher.run(&mut transport).await;

                    if let Some(tui_id) = dispatcher.tui_id() {
                        ctx.tui_registry.unregister(tui_id);
                        use ::zeroclaw_log::Instrument as _;
                        let span = ::zeroclaw_log::info_span!(
                            target: "zeroclaw_log_internal_scope",
                            "zeroclaw_scope",
                            owner_tui_id = %tui_id,
                            channel = "rpc",
                        );
                        async {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_category(::zeroclaw_log::EventCategory::Agent),
                                "TUI disconnected; sessions retained (persistent)"
                            );
                        }
                        .instrument(span)
                        .await;
                    }

                    count.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }

    platform::cleanup(&path).await;
    Ok(())
}

// ── Platform shims ───────────────────────────────────────────────

#[cfg(unix)]
mod platform {
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};
    use tokio::net::{UnixListener, UnixStream};

    pub type LocalListener = UnixListener;
    pub type LocalStream = UnixStream;

    pub fn default_endpoint(data_dir: &Path) -> PathBuf {
        data_dir.join("daemon.sock")
    }

    pub async fn prepare_parent(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .await
                .ok();
        }
        Ok(())
    }

    pub async fn remove_stale(path: &Path) -> Result<()> {
        if path.exists() {
            tokio::fs::remove_file(path)
                .await
                .context("removing stale socket")?;
        }
        Ok(())
    }

    pub fn bind(path: &Path) -> Result<LocalListener> {
        UnixListener::bind(path).context("binding unix socket")
    }

    pub async fn secure_endpoint(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .ok();
    }

    pub async fn accept(listener: &mut LocalListener, _path: &Path) -> Result<LocalStream> {
        let (stream, _addr) = listener
            .accept()
            .await
            .context("accepting local connection")?;
        Ok(stream)
    }

    pub async fn cleanup(path: &Path) {
        tokio::fs::remove_file(path).await.ok();
    }

    pub fn peer_label_from(stream: &LocalStream) -> String {
        #[cfg(target_os = "linux")]
        {
            if let Ok(cred) = stream.peer_cred() {
                return format!("unix:pid={},uid={}", cred.pid().unwrap_or(0), cred.uid());
            }
        }
        let _ = stream;
        "unix:unknown".to_string()
    }
}

#[cfg(windows)]
mod platform {
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    /// On Windows the "listener" is a single pending server instance. After
    /// each accept the caller creates a new pending instance for the next
    /// client; see `accept`.
    pub type LocalListener = NamedPipeServer;
    pub type LocalStream = NamedPipeServer;

    pub fn default_endpoint(data_dir: &Path) -> PathBuf {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        data_dir.hash(&mut hasher);
        PathBuf::from(format!(r"\\.\pipe\zeroclaw-{:x}", hasher.finish()))
    }

    pub async fn prepare_parent(_path: &Path) -> Result<()> {
        // Named pipes live in the kernel object namespace, not the
        // filesystem — no parent directory to create.
        Ok(())
    }

    pub async fn remove_stale(_path: &Path) -> Result<()> {
        // Named pipes are cleaned up when the last handle closes; there is
        // no "stale" file equivalent.
        Ok(())
    }

    pub fn bind(path: &Path) -> Result<LocalListener> {
        let name = path_to_pipe_name(path);
        ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .with_context(|| format!("creating named pipe {name}"))
    }

    pub async fn secure_endpoint(_path: &Path) {
        // The default ServerOptions ACL grants access to the creating user
        // and SYSTEM, matching the spirit of Unix 0o600. Stricter SDDL is
        // a separate hardening pass.
    }

    pub async fn accept(listener: &mut LocalListener, path: &Path) -> Result<LocalStream> {
        listener
            .connect()
            .await
            .context("awaiting named-pipe client")?;
        // Take the now-connected pipe and replace `listener` with a fresh
        // pending instance so the next accept call can wait on it.
        let next = ServerOptions::new()
            .create(path_to_pipe_name(path))
            .context("creating next named-pipe instance")?;
        let connected = std::mem::replace(listener, next);
        Ok(connected)
    }

    pub async fn cleanup(_path: &Path) {
        // Pipe handles drop with the server instance; nothing to remove.
    }

    pub fn peer_label_from(_stream: &LocalStream) -> String {
        "pipe:local".to_string()
    }

    fn path_to_pipe_name(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::dispatch::Method;
    use crate::rpc::session::SessionStore;
    use crate::rpc::types::InitializeParams;
    #[cfg(unix)]
    use crate::rpc::types::StatusResult;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use zeroclaw_api::jsonrpc::{JSONRPC_VERSION, JsonRpcRequest};
    use zeroclaw_infra::session_queue::SessionActorQueue;

    fn test_ctx(tmp: &std::path::Path) -> Arc<RpcContext> {
        let config = Config {
            data_dir: tmp.to_path_buf(),
            config_path: tmp.join("config.toml"),
            ..Config::default()
        };
        let session_queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(SessionStore::new(64, session_queue));
        RpcContext::minimal(config, sessions)
    }

    fn test_client_count() -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(0))
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

    #[cfg(unix)]
    async fn read_result<T: serde::de::DeserializeOwned>(
        reader: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    ) -> (serde_json::Value, T) {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert!(frame["error"].is_null(), "unexpected RPC error: {frame}");
        let result: T = serde_json::from_value(frame["result"].clone()).unwrap();
        (frame, result)
    }

    #[cfg(unix)]
    async fn do_initialize(
        sock_path: &std::path::Path,
    ) -> (
        tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    ) {
        let stream = tokio::net::UnixStream::connect(sock_path).await.unwrap();
        let (read_half, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);

        let params = InitializeParams {
            protocol_version: 1,
            tui_id: None,
            tui_sig: None,
            env: Default::default(),
            client_capabilities: None,
        };
        writer
            .write_all(rpc_request(Method::Initialize, &params, 1).as_bytes())
            .await
            .unwrap();

        let (_frame, _result): (_, serde_json::Value) = read_result(&mut reader).await;
        (reader, writer)
    }

    #[cfg(unix)]
    async fn wait_for_socket(path: &std::path::Path) {
        for _ in 0..50 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("socket never appeared at {}", path.display());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_initialize_handshake() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            run_local_listener(server_ctx, server_cancel, test_client_count()).await
        });

        wait_for_socket(&sock_path).await;

        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (read_half, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);

        let init_params = InitializeParams {
            protocol_version: 1,
            tui_id: None,
            tui_sig: None,
            env: Default::default(),
            client_capabilities: None,
        };
        writer
            .write_all(rpc_request(Method::Initialize, &init_params, 1).as_bytes())
            .await
            .unwrap();

        let (frame, init_result): (_, crate::rpc::types::InitializeResult) =
            read_result(&mut reader).await;

        assert_eq!(frame["jsonrpc"], JSONRPC_VERSION);
        assert_eq!(frame["id"], 1);
        assert_eq!(init_result.protocol_version, 1);
        assert!(!init_result.server_version.is_empty());

        writer
            .write_all(rpc_request(Method::Status, &serde_json::json!({}), 2).as_bytes())
            .await
            .unwrap();

        let (_frame2, status): (_, StatusResult) = read_result(&mut reader).await;
        assert_eq!(status.active_sessions, 0);

        cancel.cancel();
        drop(writer);
        let _ = handle.await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_rejects_before_initialize() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        zeroclaw_spawn::spawn!(async move {
            let _ = run_local_listener(server_ctx, server_cancel, test_client_count()).await;
        });

        wait_for_socket(&sock_path).await;

        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        writer
            .write_all(rpc_request(Method::Status, &serde_json::json!({}), 1).as_bytes())
            .await
            .unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();

        assert!(resp["error"].is_object());
        assert_eq!(
            resp["error"]["code"],
            zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED
        );

        cancel.cancel();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        zeroclaw_spawn::spawn!(async move {
            let _ = run_local_listener(server_ctx, server_cancel, test_client_count()).await;
        });

        wait_for_socket(&sock_path).await;

        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&sock_path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "socket should be owner-only (0o600), got {mode:#o}"
        );

        cancel.cancel();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_socket_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");

        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(&sock_path, b"stale").unwrap();
        assert!(sock_path.exists());

        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        zeroclaw_spawn::spawn!(async move {
            let _ = run_local_listener(server_ctx, server_cancel, test_client_count()).await;
        });

        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if tokio::net::UnixStream::connect(&sock_path).await.is_ok() {
                break;
            }
        }

        let stream = tokio::net::UnixStream::connect(&sock_path).await;
        assert!(
            stream.is_ok(),
            "should be able to connect after stale cleanup"
        );

        cancel.cancel();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_approve_resolves_pending_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        zeroclaw_spawn::spawn!(async move {
            let _ = run_local_listener(server_ctx, server_cancel, test_client_count()).await;
        });
        wait_for_socket(&sock_path).await;

        let (mut reader, mut writer) = do_initialize(&sock_path).await;

        let (pending_tx, mut pending_rx) =
            tokio::sync::oneshot::channel::<zeroclaw_api::channel::ChannelApprovalResponse>();
        ctx.approval_pending
            .insert("test-req-1".to_string(), pending_tx);

        let approve_params = serde_json::json!({
            "session_id": "unused",
            "request_id": "test-req-1",
            "decision": "allow_once",
        });
        writer
            .write_all(rpc_request(Method::SessionApprove, &approve_params, 10).as_bytes())
            .await
            .unwrap();

        let (_frame, result): (_, serde_json::Value) = read_result(&mut reader).await;
        assert_eq!(result["acknowledged"], true);

        let decision = pending_rx.try_recv().expect("decision should be resolved");
        assert_eq!(
            decision,
            zeroclaw_api::channel::ChannelApprovalResponse::Approve
        );

        cancel.cancel();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn client_count_tracks_connections() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();
        let count = Arc::new(AtomicUsize::new(0));

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        let server_count = count.clone();
        zeroclaw_spawn::spawn!(async move {
            let _ = run_local_listener(server_ctx, server_cancel, server_count).await;
        });

        wait_for_socket(&sock_path).await;

        assert_eq!(count.load(Ordering::Relaxed), 0);

        let s1 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let s2 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::Relaxed), 2);

        drop(s1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::Relaxed), 1);

        drop(s2);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::Relaxed), 0);

        cancel.cancel();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn pipe_initialize_handshake() {
        use tokio::net::windows::named_pipe::ClientOptions;
        use tokio::time::{Duration, sleep};

        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let pipe_path = socket_path(&ctx.config.read());
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            run_local_listener(server_ctx, server_cancel, test_client_count()).await
        });

        // Poll-connect until the server creates its pending instance.
        let pipe_name = pipe_path.to_string_lossy().into_owned();
        let mut client = None;
        for _ in 0..50 {
            match ClientOptions::new().open(&pipe_name) {
                Ok(c) => {
                    client = Some(c);
                    break;
                }
                Err(_) => sleep(Duration::from_millis(20)).await,
            }
        }
        let mut client = client.expect("named pipe never accepted a client");
        let (read_half, mut write_half) = tokio::io::split(&mut client);
        let mut reader = tokio::io::BufReader::new(read_half);

        let init_params = InitializeParams {
            protocol_version: 1,
            tui_id: None,
            tui_sig: None,
            env: Default::default(),
            client_capabilities: None,
        };
        write_half
            .write_all(rpc_request(Method::Initialize, &init_params, 1).as_bytes())
            .await
            .unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert!(frame["error"].is_null(), "unexpected RPC error: {frame}");
        assert_eq!(frame["jsonrpc"], JSONRPC_VERSION);
        assert_eq!(frame["id"], 1);

        cancel.cancel();
        drop(write_half);
        drop(reader);
        drop(client);
        let _ = handle.await;
    }
}

#[cfg(test)]
mod accept_error_tests {
    use super::is_recoverable_accept_error;
    use std::io::{Error, ErrorKind};

    #[cfg(unix)]
    #[test]
    fn fd_exhaustion_accept_errors_are_recoverable() {
        // EMFILE/ENFILE must not terminate the daemon.
        assert!(is_recoverable_accept_error(&Error::from_raw_os_error(24))); // EMFILE
        assert!(is_recoverable_accept_error(&Error::from_raw_os_error(23))); // ENFILE
    }

    #[test]
    fn transient_kinds_recover_but_fatal_propagates() {
        assert!(is_recoverable_accept_error(&Error::from(
            ErrorKind::ConnectionAborted
        )));
        assert!(is_recoverable_accept_error(&Error::from(
            ErrorKind::Interrupted
        )));
        // A non-transient error is not swallowed (loop will propagate it).
        assert!(!is_recoverable_accept_error(&Error::from(
            ErrorKind::InvalidInput
        )));
    }
}
