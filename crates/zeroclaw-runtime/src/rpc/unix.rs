//! Unix socket transport for the RPC layer.
//!
//! Binds at `<config.data_dir>/daemon.sock` so each `--data-dir` gets its own
//! socket. `$ZEROCLAW_SOCKET` overrides the path.

use super::context::RpcContext;
use super::dispatch::RpcDispatcher;
use super::transport::RpcTransport;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::Config;

/// Resolve socket path: `$ZEROCLAW_SOCKET` or `<data_dir>/daemon.sock`.
pub fn socket_path(config: &Config) -> PathBuf {
    if let Ok(p) = std::env::var("ZEROCLAW_SOCKET") {
        return PathBuf::from(p);
    }
    config.data_dir.join("daemon.sock")
}

// ── Transport ────────────────────────────────────────────────────

pub struct UnixSocketTransport {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer_tx: mpsc::Sender<String>,
    peer_label: String,
}

impl UnixSocketTransport {
    pub fn new(stream: UnixStream) -> Self {
        let peer_label = peer_label_from(&stream);
        let (read_half, write_half) = stream.into_split();

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            let mut writer = write_half;
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
impl RpcTransport for UnixSocketTransport {
    fn writer(&self) -> mpsc::Sender<String> {
        self.writer_tx.clone()
    }

    async fn next_frame(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line).await {
            Ok(0) => None,
            Ok(_) => Some(line),
            Err(_) => None,
        }
    }

    fn peer_label(&self) -> String {
        self.peer_label.clone()
    }
}

fn peer_label_from(stream: &UnixStream) -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(cred) = stream.peer_cred() {
            return format!("unix:pid={},uid={}", cred.pid().unwrap_or(0), cred.uid());
        }
    }
    let _ = stream;
    "unix:unknown".to_string()
}

// ── Listener ─────────────────────────────────────────────────────

/// Run the Unix socket RPC listener as a daemon subsystem.
///
/// `client_count` is incremented on connect, decremented on disconnect.
/// The daemon uses it for `--ephemeral` shutdown logic.
pub async fn run_unix_socket(
    ctx: Arc<RpcContext>,
    cancel: CancellationToken,
    client_count: Arc<AtomicUsize>,
) -> Result<()> {
    let path = {
        let config = ctx.config.read();
        socket_path(&config)
    };

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .await
                .ok();
        }
    }

    // Remove stale socket.
    if path.exists() {
        tokio::fs::remove_file(&path)
            .await
            .context("removing stale socket")?;
    }

    let listener = UnixListener::bind(&path).context("binding unix socket")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .await
            .ok();
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"path": path.display().to_string()})),
        "RPC unix socket listening"
    );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "RPC unix socket shutting down"
                );
                break;
            }
            accept = listener.accept() => {
                let (stream, _addr) = match accept {
                    Ok(v) => v,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            &format!("unix socket accept error: {e}")
                        );
                        continue;
                    }
                };

                let ctx = ctx.clone();
                let count = client_count.clone();

                count.fetch_add(1, Ordering::Relaxed);

                tokio::spawn(async move {
                    let mut transport = UnixSocketTransport::new(stream);
                    let writer_tx = transport.writer();
                    let mut dispatcher = RpcDispatcher::new(ctx, writer_tx);
                    dispatcher.run(&mut transport).await;
                    count.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }

    tokio::fs::remove_file(&path).await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::dispatch::Method;
    use crate::rpc::session::SessionStore;
    use crate::rpc::types::{InitializeParams, StatusResult};
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

    /// Read a single NDJSON response and deserialize the `result` field.
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

    /// Send initialize handshake and return the authenticated reader/writer.
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
        };
        writer
            .write_all(rpc_request(Method::Initialize, &params, 1).as_bytes())
            .await
            .unwrap();

        let (_frame, _result): (_, serde_json::Value) = read_result(&mut reader).await;
        (reader, writer)
    }

    /// Poll until the socket file appears.
    async fn wait_for_socket(path: &std::path::Path) {
        for _ in 0..50 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("socket never appeared at {}", path.display());
    }

    #[tokio::test]
    async fn socket_initialize_handshake() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        let handle = tokio::spawn(async move {
            run_unix_socket(server_ctx, server_cancel, test_client_count()).await
        });

        wait_for_socket(&sock_path).await;

        // Connect and send initialize.
        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (read_half, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);

        let init_params = InitializeParams {
            protocol_version: 1,
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

        // Send status (should work after auth).
        // Status takes no meaningful params — use empty object.
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

    #[tokio::test]
    async fn socket_rejects_before_initialize() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        tokio::spawn(async move {
            let _ = run_unix_socket(server_ctx, server_cancel, test_client_count()).await;
        });

        wait_for_socket(&sock_path).await;

        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        // Send status without initialize first.
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

    #[tokio::test]
    async fn socket_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        tokio::spawn(async move {
            let _ = run_unix_socket(server_ctx, server_cancel, test_client_count()).await;
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

    #[tokio::test]
    async fn stale_socket_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");

        // Pre-create a stale socket file.
        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(&sock_path, b"stale").unwrap();
        assert!(sock_path.exists());

        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        tokio::spawn(async move {
            let _ = run_unix_socket(server_ctx, server_cancel, test_client_count()).await;
        });

        // Wait for the listener to start (it should remove the stale file and bind).
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            // Try connecting -- if we can, the listener is up.
            if tokio::net::UnixStream::connect(&sock_path).await.is_ok() {
                break;
            }
        }

        // Verify we can actually connect (stale file was replaced by real socket).
        let stream = tokio::net::UnixStream::connect(&sock_path).await;
        assert!(
            stream.is_ok(),
            "should be able to connect after stale cleanup"
        );

        cancel.cancel();
    }

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
        tokio::spawn(async move {
            let _ = run_unix_socket(server_ctx, server_cancel, server_count).await;
        });

        wait_for_socket(&sock_path).await;

        assert_eq!(count.load(Ordering::Relaxed), 0);

        // Connect two clients.
        let s1 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let s2 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::Relaxed), 2);

        // Drop one — count should go to 1.
        drop(s1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::Relaxed), 1);

        // Drop the other — count should go to 0.
        drop(s2);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::Relaxed), 0);

        cancel.cancel();
    }
}
