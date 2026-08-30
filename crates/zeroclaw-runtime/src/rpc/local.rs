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
    readiness: Option<crate::daemon::SocketReadinessReporter>,
) -> Result<()> {
    let path = {
        let config = ctx.config.read();
        socket_path(&config)
    };

    platform::prepare_parent(&path).await?;
    let (mut listener, _endpoint_guard) = platform::bind(&path)
        .await
        .context("binding local IPC endpoint")?;

    platform::secure_endpoint(&path).await;

    if let Some(readiness) = readiness {
        readiness.report_ready();
    }

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

    Ok(())
}

// ── Platform shims ───────────────────────────────────────────────

#[cfg(unix)]
mod platform {
    use anyhow::{Context, Result};
    use std::fs::{File, Metadata, OpenOptions, TryLockError};
    use std::io::ErrorKind;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use tokio::net::{UnixListener, UnixStream};

    pub type LocalListener = UnixListener;
    pub type LocalStream = UnixStream;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct SocketIdentity {
        device: u64,
        inode: u64,
    }

    impl SocketIdentity {
        fn from_metadata(metadata: &Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }

        fn read(path: &Path) -> std::io::Result<Self> {
            std::fs::symlink_metadata(path).map(|metadata| Self::from_metadata(&metadata))
        }
    }

    /// Serializes the complete Unix endpoint lifecycle across daemon processes.
    ///
    /// The sibling lock file is intentionally persistent: unlinking it would
    /// let two processes open different lock inodes and both believe they own
    /// cleanup authority. The kernel releases the advisory lock when this file
    /// handle is dropped or the process exits.
    ///
    /// Because `ZEROCLAW_SOCKET` may place the sibling in a shared directory,
    /// acquisition fails closed unless the directory entry and the opened
    /// inode provide stable, current-user ownership; see
    /// [`require_trusted_lock_dir`] and [`require_trusted_lock_file`].
    #[derive(Debug)]
    pub(super) struct EndpointLock {
        _file: File,
    }

    /// Rejects lock directories whose entries another local user could seed
    /// or replace.
    ///
    /// A directory is trustworthy when it is owned by the current user or
    /// root and is not group/other-writable — unless the sticky bit is set,
    /// which restricts unlink and rename to the entry owner and keeps
    /// `/tmp`-style shared socket directories usable.
    fn require_trusted_lock_dir(lock_path: &Path) -> Result<()> {
        let parent = match lock_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let metadata = std::fs::metadata(parent).with_context(|| {
            format!(
                "inspecting local IPC endpoint lock directory {}",
                parent.display()
            )
        })?;
        // SAFETY: `geteuid` is a parameter-free process query with no pointer
        // arguments or memory ownership requirements.
        let euid = unsafe { libc::geteuid() };
        let mode = metadata.mode();
        if metadata.uid() != euid && metadata.uid() != 0 {
            anyhow::bail!(
                "local IPC endpoint lock directory {} is owned by uid {}; \
                 it must belong to the daemon user or root",
                parent.display(),
                metadata.uid()
            );
        }
        if mode & 0o022 != 0 && mode & 0o1000 == 0 {
            anyhow::bail!(
                "local IPC endpoint lock directory {} is writable by other \
                 users without the sticky bit; its entries could be replaced. \
                 Restrict it (chmod go-w or +t) or point ZEROCLAW_SOCKET at a \
                 private directory",
                parent.display()
            );
        }
        Ok(())
    }

    /// Rejects pre-existing lock entries that do not provide the guarantees a
    /// freshly created lock would have.
    ///
    /// The inode must be a regular file owned by the current user with no
    /// group/other access, and still linked at the time of inspection. A
    /// foreign-owned or permissive entry could be locked by another local
    /// user to block startup, or unlinked and recreated by its owner to hand
    /// two daemons different lock inodes.
    fn require_trusted_lock_file(metadata: &Metadata, lock_path: &Path) -> Result<()> {
        if !metadata.file_type().is_file() {
            anyhow::bail!(
                "local IPC endpoint lock {} is not a regular file; remove it \
                 or choose a different socket path",
                lock_path.display()
            );
        }
        // SAFETY: `geteuid` is a parameter-free process query with no pointer
        // arguments or memory ownership requirements.
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid {
            anyhow::bail!(
                "local IPC endpoint lock {} is owned by uid {}, not the \
                 daemon user; remove it or choose a different socket path",
                lock_path.display(),
                metadata.uid()
            );
        }
        if metadata.mode() & 0o077 != 0 {
            anyhow::bail!(
                "local IPC endpoint lock {} is accessible to other users \
                 (mode {:o}); restrict it to 0600 or remove it",
                lock_path.display(),
                metadata.mode() & 0o7777
            );
        }
        if metadata.nlink() == 0 {
            anyhow::bail!(
                "local IPC endpoint lock {} was unlinked while being opened",
                lock_path.display()
            );
        }
        Ok(())
    }

    impl EndpointLock {
        pub(super) fn acquire(path: &Path) -> Result<Self> {
            let mut lock_name = path.as_os_str().to_os_string();
            lock_name.push(".lock");
            let lock_path = PathBuf::from(lock_name);
            require_trusted_lock_dir(&lock_path)?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&lock_path)
                .with_context(|| {
                    format!(
                        "opening local IPC endpoint lifecycle lock {}",
                        lock_path.display()
                    )
                })?;
            let metadata = file.metadata().with_context(|| {
                format!(
                    "inspecting local IPC endpoint lifecycle lock {}",
                    lock_path.display()
                )
            })?;
            require_trusted_lock_file(&metadata, &lock_path)?;

            match file.try_lock() {
                Ok(()) => {
                    // The directory entry must still name the locked inode.
                    // A swap between open and lock would let a second daemon
                    // lock the replacement and re-enter the split-ownership
                    // race this lock exists to prevent.
                    let current = SocketIdentity::read(&lock_path).with_context(|| {
                        format!(
                            "confirming local IPC endpoint lifecycle lock {}",
                            lock_path.display()
                        )
                    })?;
                    if current != SocketIdentity::from_metadata(&metadata) {
                        anyhow::bail!(
                            "local IPC endpoint lock {} was replaced while being \
                             acquired; refusing to share lifecycle ownership",
                            lock_path.display()
                        );
                    }
                    Ok(Self { _file: file })
                }
                Err(TryLockError::WouldBlock) => Err(std::io::Error::new(
                    ErrorKind::AddrInUse,
                    format!(
                        "local IPC endpoint lifecycle is already owned at {}",
                        path.display()
                    ),
                )
                .into()),
                Err(TryLockError::Error(error)) => Err(error).with_context(|| {
                    format!(
                        "locking local IPC endpoint lifecycle at {}",
                        lock_path.display()
                    )
                }),
            }
        }
    }

    /// Removes the bound socket only while the path still names this listener.
    ///
    /// A path can be unlinked and rebound while the original listener remains
    /// alive. Comparing device and inode prevents the old listener from
    /// deleting the replacement endpoint when it shuts down.
    pub struct EndpointGuard {
        path: PathBuf,
        identity: SocketIdentity,
        _lock: EndpointLock,
    }

    impl Drop for EndpointGuard {
        fn drop(&mut self) {
            if SocketIdentity::read(&self.path).ok() == Some(self.identity) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    pub fn default_endpoint(data_dir: &Path) -> PathBuf {
        data_dir.join("daemon.sock")
    }

    /// Creates the endpoint directory and tightens it to owner-only access.
    ///
    /// The chmod is best-effort on purpose: a directory this process just
    /// created is already chmoddable, while a pre-existing shared directory
    /// (e.g. `/tmp` for an external `ZEROCLAW_SOCKET`) rejects it and is
    /// instead vetted by [`require_trusted_lock_dir`] before the lifecycle
    /// lock is acquired. That check — not this chmod — is what decides
    /// whether the directory is safe to hold the lock.
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
        let observed = match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) => SocketIdentity::from_metadata(&metadata),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("inspecting local IPC endpoint"),
        };

        match UnixStream::connect(path).await {
            Ok(_) => Err(std::io::Error::new(
                ErrorKind::AddrInUse,
                format!(
                    "local IPC endpoint is already serving at {}",
                    path.display()
                ),
            )
            .into()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) if error.kind() != ErrorKind::ConnectionRefused => {
                Err(error).context("probing existing local IPC endpoint")
            }
            Err(_) => {
                let current = match tokio::fs::symlink_metadata(path).await {
                    Ok(metadata) => SocketIdentity::from_metadata(&metadata),
                    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
                    Err(error) => {
                        return Err(error).context("rechecking stale local IPC endpoint");
                    }
                };

                if current != observed {
                    return Err(std::io::Error::new(
                        ErrorKind::AddrInUse,
                        format!(
                            "local IPC endpoint changed while being probed at {}",
                            path.display()
                        ),
                    )
                    .into());
                }

                tokio::fs::remove_file(path)
                    .await
                    .context("removing stale socket")
            }
        }
    }

    pub(super) async fn bind_locked(
        path: &Path,
        lock: EndpointLock,
    ) -> Result<(LocalListener, EndpointGuard)> {
        remove_stale(path).await?;
        let listener = UnixListener::bind(path).context("binding unix socket")?;
        let identity = SocketIdentity::read(path).context("identifying bound unix socket")?;
        Ok((
            listener,
            EndpointGuard {
                path: path.to_path_buf(),
                identity,
                _lock: lock,
            },
        ))
    }

    pub async fn bind(path: &Path) -> Result<(LocalListener, EndpointGuard)> {
        let lock = EndpointLock::acquire(path)?;
        bind_locked(path, lock).await
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
    pub struct EndpointGuard;

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

    pub async fn bind(path: &Path) -> Result<(LocalListener, EndpointGuard)> {
        let name = path_to_pipe_name(path);
        let listener = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .with_context(|| format!("creating named pipe {name}"))?;
        Ok((listener, EndpointGuard))
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

    /// Wait for the server-side client count to reach `expected`, or fail with
    /// the value actually observed.
    ///
    /// `run_local_listener` increments the count in its accept loop but
    /// decrements it at the end of the per-connection task, so a disconnect
    /// becomes visible only once the dispatcher has seen EOF and that task has
    /// finished. Waiting a fixed interval assumes that completes within it;
    /// under a loaded test harness it may not.
    #[cfg(unix)]
    async fn wait_for_client_count(count: &Arc<AtomicUsize>, expected: usize) {
        for _ in 0..250 {
            if count.load(Ordering::Relaxed) == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!(
            "client count never reached {expected}; last observed {}",
            count.load(Ordering::Relaxed)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn daemon_startup_socket_initialize_handshake_reports_readiness() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();
        let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(false);
        let readiness = crate::daemon::SocketReadinessReporter::new(move || {
            let _ = ready_tx.send(true);
        });

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            run_local_listener(
                server_ctx,
                server_cancel,
                test_client_count(),
                Some(readiness),
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            ready_rx.wait_for(|ready| *ready).await.unwrap();
        })
        .await
        .expect("local IPC should report its secured bind");

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
            let _ = run_local_listener(server_ctx, server_cancel, test_client_count(), None).await;
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
            let _ = run_local_listener(server_ctx, server_cancel, test_client_count(), None).await;
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
    async fn active_socket_is_not_stolen_by_second_listener() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        let first = zeroclaw_spawn::spawn!(async move {
            run_local_listener(server_ctx, server_cancel, test_client_count(), None).await
        });

        wait_for_socket(&sock_path).await;

        let second = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            run_local_listener(
                ctx.clone(),
                CancellationToken::new(),
                test_client_count(),
                None,
            ),
        )
        .await
        .expect("the incumbent probe should complete")
        .expect_err("a second listener must not replace a live endpoint");
        assert_eq!(
            second
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(ErrorKind::AddrInUse)
        );
        assert!(
            tokio::net::UnixStream::connect(&sock_path).await.is_ok(),
            "the incumbent listener should remain reachable"
        );

        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), first)
            .await
            .expect("the incumbent listener should stop after cancellation")
            .unwrap()
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_stale_start_is_serialized_before_cleanup() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("daemon.sock");
        let stale_listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        drop(stale_listener);
        wait_for_stale_socket(&sock_path).await;

        let stale_metadata = std::fs::symlink_metadata(&sock_path).unwrap();
        let stale_identity = (stale_metadata.dev(), stale_metadata.ino());

        // Pause the first startup immediately after it owns cleanup authority.
        // A concurrent startup must fail before it can inspect or unlink the
        // stale endpoint, then the lock owner can complete the bind.
        let first_lock = platform::EndpointLock::acquire(&sock_path).unwrap();
        let contender = match platform::bind(&sock_path).await {
            Ok(_) => panic!("a concurrent startup must not enter stale cleanup"),
            Err(error) => error,
        };
        assert_eq!(
            contender
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(ErrorKind::AddrInUse)
        );

        let current_metadata = std::fs::symlink_metadata(&sock_path).unwrap();
        assert_eq!(
            (current_metadata.dev(), current_metadata.ino()),
            stale_identity,
            "the losing startup must not mutate the stale endpoint"
        );

        let (listener, guard) = platform::bind_locked(&sock_path, first_lock).await.unwrap();
        assert!(tokio::net::UnixStream::connect(&sock_path).await.is_ok());

        drop(listener);
        drop(guard);
    }

    #[cfg(unix)]
    async fn wait_for_stale_socket(path: &std::path::Path) {
        let started = tokio::time::Instant::now();
        let timeout = Duration::from_secs(2);

        loop {
            match tokio::net::UnixStream::connect(path).await {
                Err(error) if error.kind() == ErrorKind::ConnectionRefused => return,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    panic!("synthetic stale endpoint disappeared before cleanup test: {error}");
                }
                Err(error) if started.elapsed() >= timeout => {
                    panic!("synthetic stale endpoint did not settle before cleanup test: {error}");
                }
                Ok(_) if started.elapsed() >= timeout => {
                    panic!("synthetic stale endpoint stayed reachable before cleanup test");
                }
                Ok(_) | Err(_) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    }

    /// Set on the re-executed child so `endpoint_lock_is_held_through_guard_cleanup`
    /// runs its real assertions instead of relaunching itself again.
    #[cfg(unix)]
    const ENDPOINT_LOCK_TEST_CHILD_ENV: &str = "ZEROCLAW_ENDPOINT_LOCK_TEST_CHILD";

    #[cfg(unix)]
    #[tokio::test]
    async fn endpoint_lock_is_held_through_guard_cleanup() {
        // This test drops a real EndpointLock and then requires that a
        // fresh acquire of the same path immediately succeeds. `flock()`,
        // which EndpointLock uses, is scoped to the *open file
        // description*, and `fork()` duplicates a process's whole
        // descriptor table into any child it creates. If some unrelated
        // test elsewhere in this binary forks a subprocess (the shell
        // tool, a sandbox backend, ...) at the exact instant our lock's
        // descriptor happens to be open, that child inherits a reference
        // that keeps the lock alive past our own `drop()`, and the
        // reacquire below spuriously observes it as still owned. This is
        // not about path uniqueness — this test's `tempfile::tempdir()`
        // path is already unique — it is about the whole test binary
        // process's descriptor table being shared with every other
        // concurrently running test.
        //
        // The only way to make the reacquire deterministic is to guarantee
        // nothing else can fork while it happens, so the real body below
        // runs in a freshly exec'd child process dedicated to this one
        // test, whose descriptor table nothing else can share.
        if std::env::var_os(ENDPOINT_LOCK_TEST_CHILD_ENV).is_none() {
            let exe = std::env::current_exe().expect("resolve current test binary");
            let status = tokio::process::Command::new(exe)
                .arg("rpc::local::tests::endpoint_lock_is_held_through_guard_cleanup")
                .arg("--exact")
                .arg("--test-threads=1")
                .env(ENDPOINT_LOCK_TEST_CHILD_ENV, "1")
                .status()
                .await
                .expect("relaunch endpoint_lock_is_held_through_guard_cleanup in isolation");
            assert!(
                status.success(),
                "isolated endpoint_lock_is_held_through_guard_cleanup failed: {status}"
            );
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("daemon.sock");
        let (listener, guard) = platform::bind(&sock_path).await.unwrap();

        std::fs::remove_file(&sock_path).unwrap();
        let replacement = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let contender = match platform::EndpointLock::acquire(&sock_path) {
            Ok(_) => panic!("the endpoint lock must remain held until guard cleanup finishes"),
            Err(error) => error,
        };
        assert_eq!(
            contender
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(ErrorKind::AddrInUse)
        );

        drop(listener);
        drop(guard);

        assert!(
            tokio::net::UnixStream::connect(&sock_path).await.is_ok(),
            "guard cleanup must preserve a replacement endpoint"
        );
        let _next_owner = platform::EndpointLock::acquire(&sock_path).unwrap();
        drop(replacement);
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_lock_rejects_symlinked_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("daemon.sock");
        let target = tmp.path().join("elsewhere");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, tmp.path().join("daemon.sock.lock")).unwrap();

        platform::EndpointLock::acquire(&sock_path)
            .expect_err("a symlinked lock entry must not be followed");
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_lock_rejects_non_regular_entry() {
        use std::os::unix::ffi::OsStrExt;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("daemon.sock");
        let lock_path = tmp.path().join("daemon.sock.lock");
        let c_path = std::ffi::CString::new(lock_path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        platform::EndpointLock::acquire(&sock_path)
            .expect_err("a non-regular lock entry must be rejected");
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_lock_rejects_preexisting_permissive_entry() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("daemon.sock");
        let lock_path = tmp.path().join("daemon.sock.lock");
        std::fs::write(&lock_path, b"").unwrap();
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o666)).unwrap();

        platform::EndpointLock::acquire(&sock_path)
            .expect_err("a lock entry accessible to other users must be rejected");

        // Restricting the entry restores acquisition without recreating it.
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _lock = platform::EndpointLock::acquire(&sock_path)
            .expect("a private pre-existing entry is usable");
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_lock_rejects_swappable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        // Group-writable without the sticky bit: any group member could
        // unlink and recreate the lock entry.
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o770)).unwrap();
        let sock_path = shared.join("daemon.sock");

        platform::EndpointLock::acquire(&sock_path)
            .expect_err("a directory with replaceable entries must be rejected");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn endpoint_lock_supports_sticky_shared_directory() {
        use std::os::unix::fs::PermissionsExt;

        // The /tmp shape a documented external ZEROCLAW_SOCKET override can
        // point into: world-writable, but the sticky bit restricts unlink
        // and rename to the entry owner, so the lock inode is stable.
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let sock_path = shared.join("daemon.sock");

        let (listener, guard) = platform::bind(&sock_path).await.unwrap();
        assert!(
            tokio::net::UnixStream::connect(&sock_path).await.is_ok(),
            "an external socket path in a sticky shared directory must serve"
        );
        drop(listener);
        drop(guard);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_socket_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");

        let stale_listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        drop(stale_listener);
        assert!(sock_path.exists());

        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            run_local_listener(server_ctx, server_cancel, test_client_count(), None).await
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
        handle.await.unwrap().unwrap();
        assert!(
            !sock_path.exists(),
            "the listener should remove its own socket"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_does_not_unlink_replacement_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let sock_path = ctx.config.read().data_dir.join("daemon.sock");
        let cancel = CancellationToken::new();

        let server_cancel = cancel.clone();
        let server_ctx = ctx.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            run_local_listener(server_ctx, server_cancel, test_client_count(), None).await
        });

        wait_for_socket(&sock_path).await;
        std::fs::remove_file(&sock_path).unwrap();
        let replacement = tokio::net::UnixListener::bind(&sock_path).unwrap();

        cancel.cancel();
        handle.await.unwrap().unwrap();

        assert!(
            sock_path.exists(),
            "shutdown must preserve a replacement socket"
        );
        assert!(
            tokio::net::UnixStream::connect(&sock_path).await.is_ok(),
            "the replacement listener should remain reachable"
        );
        drop(replacement);
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
            let _ = run_local_listener(server_ctx, server_cancel, test_client_count(), None).await;
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
            let _ = run_local_listener(server_ctx, server_cancel, server_count, None).await;
        });

        wait_for_socket(&sock_path).await;

        assert_eq!(count.load(Ordering::Relaxed), 0);

        let s1 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let s2 = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        wait_for_client_count(&count, 2).await;

        drop(s1);
        wait_for_client_count(&count, 1).await;

        drop(s2);
        wait_for_client_count(&count, 0).await;

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
            run_local_listener(server_ctx, server_cancel, test_client_count(), None).await
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
