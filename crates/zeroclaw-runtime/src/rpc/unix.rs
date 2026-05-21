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
    use crate::rpc::session::SessionStore;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
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

        // Wait for socket to appear.
        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(sock_path.exists(), "socket never appeared");

        // Connect and send initialize.
        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": { "protocolVersion": 1, "token": "" },
            "id": 1
        });
        writer
            .write_all(format!("{}\n", req).as_bytes())
            .await
            .unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], 1);
        assert!(resp["result"]["serverVersion"].is_string());
        assert!(resp["error"].is_null());

        // Send status (should work after auth).
        let req2 = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "status",
            "params": {},
            "id": 2
        });
        writer
            .write_all(format!("{}\n", req2).as_bytes())
            .await
            .unwrap();

        let mut line2 = String::new();
        reader.read_line(&mut line2).await.unwrap();
        let resp2: serde_json::Value = serde_json::from_str(line2.trim()).unwrap();
        assert_eq!(resp2["id"], 2);
        assert_eq!(resp2["result"]["activeSessions"], 0);

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

        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        // Send status without initialize first.
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "status",
            "params": {},
            "id": 1
        });
        writer
            .write_all(format!("{}\n", req).as_bytes())
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

        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

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

        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

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
