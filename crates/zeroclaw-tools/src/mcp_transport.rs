//! MCP transport abstraction — supports stdio, SSE, and HTTP transports.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use parking_lot::Mutex as ParkingMutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify, OwnedRwLockReadGuard, RwLock, oneshot};
use tokio::time::{Duration, timeout};
use tokio_stream::StreamExt;

use crate::mcp_protocol::{JsonRpcRequest, JsonRpcResponse};
use zeroclaw_config::schema::{McpServerConfig, McpTransport};

/// Maximum bytes for a single JSON-RPC response.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024; // 4 MB

/// How often the stdio child-exit watcher polls the direct child process for
/// exit. Short enough that a dead child is surfaced to health checks promptly,
/// long enough to stay negligible against idle transports.
const STDIO_CHILD_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Courtesy window granted to a stdio MCP server to exit on its own after its
/// stdin is closed (EOF), before the reaper escalates to `start_kill`. A server
/// that shuts down on EOF exits near-instantly; this only bounds how long a
/// server that ignores EOF delays teardown before being signalled.
const STDIO_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Timeout for init/list operations.
const RECV_TIMEOUT_SECS: u64 = 30;

/// Legacy default HTTP request timeout for non-tool MCP HTTP/SSE requests.
const DEFAULT_HTTP_REQUEST_TIMEOUT_SECS: u64 = 120;

/// JSON-RPC method name for MCP tool calls.
const TOOLS_CALL_METHOD: &str = "tools/call";

/// Streamable HTTP Accept header required by MCP HTTP transport.
const MCP_STREAMABLE_ACCEPT: &str = "application/json, text/event-stream";

/// Default media type for MCP JSON-RPC request bodies.
const MCP_JSON_CONTENT_TYPE: &str = "application/json";
/// Streamable HTTP session header used to preserve MCP server state.
const MCP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
/// Maximum size of one operator-selected PEM CA file.
const MAX_TLS_CA_BYTES: usize = 1024 * 1024;

fn http_request_timeout_secs(
    request: &JsonRpcRequest,
    tool_timeout_secs: Option<u64>,
) -> Option<u64> {
    if request.method == TOOLS_CALL_METHOD {
        tool_timeout_secs
    } else {
        Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS)
    }
}

fn http_sse_read_timeout_secs(
    request: &JsonRpcRequest,
    tool_timeout_secs: Option<u64>,
) -> Option<u64> {
    if request.method == TOOLS_CALL_METHOD {
        tool_timeout_secs
    } else {
        Some(RECV_TIMEOUT_SECS)
    }
}

fn apply_request_timeout(
    req: reqwest::RequestBuilder,
    timeout_secs: Option<u64>,
) -> reqwest::RequestBuilder {
    if let Some(timeout_secs) = timeout_secs {
        req.timeout(Duration::from_secs(timeout_secs))
    } else {
        req
    }
}

fn require_https_url(server_name: &str, url: &str, target: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .with_context(|| format!("MCP server `{server_name}`: invalid {target} URL"))?;
    if parsed.scheme() != "https" {
        bail!(
            "MCP server `{server_name}`: tls_ca_cert_path requires an HTTPS {target}; \
             refusing plaintext transport"
        );
    }
    Ok(())
}

/// Open a candidate CA file without letting a special file block the caller.
///
/// The open itself carries `O_NONBLOCK` on unix so that a FIFO (or a symlink to
/// one) substituted at `path` returns a handle immediately instead of parking
/// the thread until a writer appears. Classification happens on the returned
/// handle, never on a second pathname lookup, so the file object we validate is
/// the same one we read.
///
/// Symlinks are followed deliberately: certificate rotation and mounted-secret
/// deployments publish CA bundles through symlink indirection. Following them is
/// safe here precisely because the resulting handle is classified after the
/// open — a symlink that retargets to a special file still yields a handle we
/// reject rather than a blocking open.
fn open_tls_ca_file(path: &str) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

fn load_tls_ca_pem(config: &McpServerConfig, path: &str) -> Result<Vec<u8>> {
    let file = open_tls_ca_file(path).with_context(|| {
        format!(
            "MCP server `{}`: cannot read TLS CA certificate at `{path}`",
            config.name
        )
    })?;
    let opened_metadata = file.metadata().with_context(|| {
        format!(
            "MCP server `{}`: cannot inspect opened TLS CA certificate at `{path}`",
            config.name
        )
    })?;
    if !opened_metadata.file_type().is_file() {
        bail!(
            "MCP server `{}`: TLS CA certificate path must name a regular file: `{path}`",
            config.name
        );
    }
    if opened_metadata.len() > MAX_TLS_CA_BYTES as u64 {
        bail!(
            "MCP server `{}`: TLS CA certificate at `{path}` exceeds the {MAX_TLS_CA_BYTES}-byte limit",
            config.name
        );
    }

    let mut pem = Vec::with_capacity(opened_metadata.len() as usize + 1);
    file.take(MAX_TLS_CA_BYTES as u64 + 1)
        .read_to_end(&mut pem)
        .with_context(|| {
            format!(
                "MCP server `{}`: cannot read TLS CA certificate at `{path}`",
                config.name
            )
        })?;
    if pem.len() > MAX_TLS_CA_BYTES {
        bail!(
            "MCP server `{}`: TLS CA certificate at `{path}` exceeds the {MAX_TLS_CA_BYTES}-byte limit",
            config.name
        );
    }
    Ok(pem)
}

/// Build the shared HTTP client for remote MCP transports.
///
/// The optional server-specific CA is additive: system/default roots remain
/// enabled, and normal chain and hostname verification stay in force.
fn build_remote_http_client(config: &McpServerConfig) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();

    if let Some(path) = config.tls_ca_cert_path.as_deref() {
        let server_name = config.name.clone();
        builder = builder.redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error(std::io::Error::other(format!(
                    "MCP server `{server_name}`: too many redirects"
                )))
            } else if attempt.url().scheme() == "https" {
                attempt.follow()
            } else {
                attempt.error(std::io::Error::other(format!(
                    "MCP server `{server_name}`: tls_ca_cert_path forbids redirecting to plaintext"
                )))
            }
        }));

        if !std::path::Path::new(path).is_absolute() {
            bail!(
                "MCP server `{}`: TLS CA certificate path must be absolute: `{}`",
                config.name,
                path
            );
        }

        let pem = load_tls_ca_pem(config, path)?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem).with_context(|| {
            format!(
                "MCP server `{}`: invalid PEM CA certificate at `{}`",
                config.name, path
            )
        })?;
        if certificates.is_empty() {
            bail!(
                "MCP server `{}`: CA certificate file `{}` contained no certificates",
                config.name,
                path
            );
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }

    builder.build().with_context(|| {
        format!(
            "failed to build HTTP client for MCP server `{}`",
            config.name
        )
    })
}

// ── Transport Errors ───────────────────────────────────────────────────────

/// Transport-level failures that require reconnecting and re-running the MCP
/// handshake. The client may retry only when the request is known not to have
/// been written; failures after a possible write are surfaced without replay.
/// Distinct from a genuine tool/application error, which is always reported
/// as-is and never retried.
#[derive(Debug, thiserror::Error)]
pub enum McpTransportError {
    /// The server no longer recognizes our session (typically after it
    /// restarted). Surfaced from HTTP 404/410 responses.
    #[error("MCP session is stale (HTTP {status})")]
    StaleSession { status: u16 },

    /// The underlying stream/connection dropped before a response arrived
    /// (e.g. SSE EOF or connection reset).
    #[error("MCP transport connection closed")]
    TransportClosed,

    /// A recovery was published after this request entered the transport but
    /// before it crossed the concrete writer boundary. The caller must wait
    /// for that recovery instead of treating this as a connection failure.
    #[error("MCP transport write blocked by pending recovery")]
    RecoveryPending,
}

const REQUEST_PRE_WRITE: u8 = 0;
const REQUEST_OUTCOME_UNKNOWN: u8 = 1;
const REQUEST_COMPLETED: u8 = 2;

/// Tracks whether a request can still be proved not to have reached the
/// server. The client uses this state to recover cancelled post-write calls
/// without replaying a possibly side-effecting operation.
pub(crate) struct McpRequestLifecycle {
    phase: AtomicU8,
    epoch: AtomicU64,
    epoch_gate: Option<Arc<RwLock<u64>>>,
    recovery_gate: Option<Arc<dyn McpRecoveryGate>>,
    fixed_epoch: u64,
}

impl McpRequestLifecycle {
    pub(crate) fn coordinated(
        epoch_gate: Arc<RwLock<u64>>,
        recovery_gate: Option<Arc<dyn McpRecoveryGate>>,
    ) -> Self {
        Self {
            phase: AtomicU8::new(REQUEST_PRE_WRITE),
            epoch: AtomicU64::new(0),
            epoch_gate: Some(epoch_gate),
            recovery_gate,
            fixed_epoch: 0,
        }
    }

    pub(crate) fn uncoordinated(epoch: u64) -> Self {
        Self {
            phase: AtomicU8::new(REQUEST_PRE_WRITE),
            epoch: AtomicU64::new(0),
            epoch_gate: None,
            recovery_gate: None,
            fixed_epoch: epoch,
        }
    }

    async fn begin_write(&self) -> McpWritePermit {
        let permit = match &self.epoch_gate {
            Some(gate) => {
                let guard = Arc::clone(gate).read_owned().await;
                let epoch = *guard;
                McpWritePermit {
                    epoch,
                    guard: Some(guard),
                }
            }
            None => McpWritePermit {
                epoch: self.fixed_epoch,
                guard: None,
            },
        };
        self.epoch.store(permit.epoch(), Ordering::Release);
        permit
    }

    pub(crate) fn mark_outcome_unknown(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::Release);
        self.phase.store(REQUEST_OUTCOME_UNKNOWN, Ordering::Release);
    }

    fn mark_completed(&self) {
        self.phase.store(REQUEST_COMPLETED, Ordering::Release);
    }

    pub(crate) fn outcome_unknown_epoch(&self) -> Option<u64> {
        (self.phase.load(Ordering::Acquire) == REQUEST_OUTCOME_UNKNOWN)
            .then(|| self.epoch.load(Ordering::Acquire))
    }

    pub(crate) fn pre_write_epoch(&self) -> Option<u64> {
        (self.phase.load(Ordering::Acquire) == REQUEST_PRE_WRITE)
            .then(|| self.epoch.load(Ordering::Acquire))
    }

    fn check_writer_boundary(&self) -> Result<()> {
        let blocked = self
            .recovery_gate
            .as_ref()
            .is_some_and(|gate| gate.write_blocked());
        if blocked {
            return Err(McpTransportError::RecoveryPending.into());
        }
        Ok(())
    }

    fn arm_recovery_if_unknown(&self) {
        if let Some(epoch) = self.outcome_unknown_epoch()
            && let Some(gate) = &self.recovery_gate
        {
            gate.arm(epoch);
        }
    }
}

/// Coordination surface shared by the client recovery state and concrete
/// transport writer boundaries.
pub(crate) trait McpRecoveryGate: Send + Sync {
    fn arm(&self, epoch: u64);
    fn write_blocked(&self) -> bool;
}

/// Owns the concrete stdio state boundary while a write is being prepared.
///
/// Its explicit `Drop` publishes recovery before releasing `state`; this is
/// stronger than relying on local-variable drop order across an async
/// cancellation point.
struct StdioWriterBoundary<'a> {
    state: Option<tokio::sync::MutexGuard<'a, StdioState>>,
    lifecycle: &'a McpRequestLifecycle,
}

impl<'a> StdioWriterBoundary<'a> {
    fn new(
        state: tokio::sync::MutexGuard<'a, StdioState>,
        lifecycle: &'a McpRequestLifecycle,
    ) -> Result<Self> {
        lifecycle.check_writer_boundary()?;
        Ok(Self {
            state: Some(state),
            lifecycle,
        })
    }

    fn state_mut(&mut self) -> Result<&mut StdioState> {
        self.state
            .as_deref_mut()
            .ok_or_else(|| anyhow::Error::msg("stdio writer state was already released"))
    }

    fn release_state(&mut self) {
        drop(self.state.take());
    }
}

impl Drop for StdioWriterBoundary<'_> {
    fn drop(&mut self) {
        self.lifecycle.arm_recovery_if_unknown();
        self.release_state();
    }
}

struct McpWritePermit {
    epoch: u64,
    guard: Option<OwnedRwLockReadGuard<u64>>,
}

impl McpWritePermit {
    fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl Drop for McpWritePermit {
    fn drop(&mut self) {
        drop(self.guard.take());
    }
}

// ── Transport Traits ─────────────────────────────────────────────────────

/// Public compatibility surface for direct transport users.
///
/// The MCP client uses the cancellation-aware shared transport trait below;
/// this mutable facade preserves the existing downstream API.
#[async_trait::async_trait]
pub trait McpTransportConn: Send + Sync {
    async fn send_and_recv(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse>;

    async fn reset(&mut self) -> Result<()> {
        Ok(())
    }

    fn health_check(&mut self) -> bool {
        true
    }

    async fn close(&mut self) -> Result<()>;
}

#[async_trait::async_trait]
pub(crate) trait SharedMcpTransportConn: Send + Sync {
    /// Send a JSON-RPC request and receive the response.
    async fn send_and_recv(
        &self,
        request: &JsonRpcRequest,
        lifecycle: &McpRequestLifecycle,
    ) -> Result<JsonRpcResponse>;

    /// Reset per-connection session state so the next operation re-establishes
    /// a fresh session. Default is a no-op for stateless transports (stdio).
    async fn reset(&self) -> Result<()> {
        Ok(())
    }

    /// Check whether the underlying transport is still alive without sending a
    /// real request.  The HTTP and SSE transports always return `Ok(true)` —
    /// connection drops surface through `send_and_recv` errors.  The stdio
    /// transport verifies the child process is still running via `try_wait()`.
    fn health_check(&self) -> bool {
        true
    }

    /// Close the connection.
    async fn close(&self) -> Result<()>;
}

// ── Stdio Transport ──────────────────────────────────────────────────────

type PendingMap = Arc<ParkingMutex<HashMap<(u64, u64), oneshot::Sender<JsonRpcResponse>>>>;

struct StdioPendingGuard {
    pending: PendingMap,
    key: (u64, u64),
}

impl Drop for StdioPendingGuard {
    fn drop(&mut self) {
        self.pending.lock().remove(&self.key);
    }
}

struct StdioConn {
    generation: u64,
    /// Shared so both the child-exit watcher (nonblocking `try_wait`) and the
    /// reaper (`start_kill` + `wait`) can access the direct child without a
    /// second `Child` handle.
    child: Arc<tokio::sync::Mutex<Child>>,
    stdin: tokio::process::ChildStdin,
    reader: tokio::task::JoinHandle<()>,
    /// Set to `true` by the child-exit watcher when the *direct* child process
    /// exits, independent of whether its stdout pipe has reached EOF (a
    /// descendant may keep the inherited pipe open). Health checks consult this
    /// so a dead child is never reported healthy.
    child_exited: Arc<AtomicBool>,
    /// Background task that watches the direct child for exit.
    exit_watcher: tokio::task::JoinHandle<()>,
}

impl Drop for StdioConn {
    fn drop(&mut self) {
        // Abort the background tasks so their `Arc<Mutex<Child>>` clone is
        // released. This lets the last `Child` owner drop, which — combined
        // with `kill_on_drop(true)` — reaps the direct child when a connection
        // is dropped without going through `reap_conn` (e.g. registry teardown).
        self.reader.abort();
        self.exit_watcher.abort();
    }
}

#[derive(Default)]
struct StdioState {
    conn: Option<StdioConn>,
    closed: bool,
}

/// Stdio-based transport (spawn local process).
pub struct StdioTransport {
    config: McpServerConfig,
    state: Mutex<StdioState>,
    pending: PendingMap,
    alive: Arc<AtomicBool>,
    active_generation: Arc<AtomicU64>,
    /// Direct-child exit signal for the active connection, independent of
    /// stdout EOF. Reset to `false` on every spawn and set to `true` by the
    /// child-exit watcher when the direct child process exits. Read
    /// synchronously by `health_check`.
    child_exited: Arc<AtomicBool>,
    #[cfg(all(test, unix))]
    write_test_hook: ParkingMutex<Option<Arc<StdioWriteTestHook>>>,
}

#[cfg(all(test, unix))]
pub(crate) struct StdioWriteTestHook {
    attempts: std::sync::atomic::AtomicUsize,
    attempts_changed: Notify,
    pause_next_payload: AtomicBool,
    payload_paused: Notify,
    release_payload: Notify,
}

#[cfg(all(test, unix))]
impl StdioWriteTestHook {
    pub(crate) fn new() -> Self {
        Self {
            attempts: std::sync::atomic::AtomicUsize::new(0),
            attempts_changed: Notify::new(),
            pause_next_payload: AtomicBool::new(false),
            payload_paused: Notify::new(),
            release_payload: Notify::new(),
        }
    }

    pub(crate) fn pause_next_payload(&self) {
        self.pause_next_payload.store(true, Ordering::Release);
    }

    pub(crate) async fn wait_for_attempts(&self, expected: usize) {
        loop {
            let changed = self.attempts_changed.notified();
            if self.attempts.load(Ordering::Acquire) >= expected {
                return;
            }
            changed.await;
        }
    }

    pub(crate) async fn wait_for_payload_pause(&self) {
        self.payload_paused.notified().await;
    }

    fn note_attempt(&self) {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        self.attempts_changed.notify_waiters();
    }

    async fn pause_after_payload_if_armed(&self) {
        if self.pause_next_payload.swap(false, Ordering::AcqRel) {
            self.payload_paused.notify_one();
            self.release_payload.notified().await;
        }
    }
}

impl StdioTransport {
    pub fn new(config: &McpServerConfig) -> Result<Self> {
        let pending = Arc::new(ParkingMutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(false));
        let active_generation = Arc::new(AtomicU64::new(1));
        let child_exited = Arc::new(AtomicBool::new(false));
        let conn = Self::spawn(
            config,
            1,
            Arc::clone(&pending),
            Arc::clone(&alive),
            Arc::clone(&active_generation),
            Arc::clone(&child_exited),
        )?;
        Ok(Self {
            config: config.clone(),
            state: Mutex::new(StdioState {
                conn: Some(conn),
                closed: false,
            }),
            pending,
            alive,
            active_generation,
            child_exited,
            #[cfg(all(test, unix))]
            write_test_hook: ParkingMutex::new(None),
        })
    }

    #[cfg(all(test, unix))]
    pub(crate) fn set_write_test_hook(&self, hook: Arc<StdioWriteTestHook>) {
        *self.write_test_hook.lock() = Some(hook);
    }

    fn spawn(
        config: &McpServerConfig,
        generation: u64,
        pending: PendingMap,
        alive: Arc<AtomicBool>,
        active_generation: Arc<AtomicU64>,
        child_exited: Arc<AtomicBool>,
    ) -> Result<StdioConn> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .envs(&config.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn MCP server `{}`", config.name))?;

        let stdin = child.stdin.take().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "mcp_server": &config.name,
                        "missing": "stdin",
                    })),
                "mcp_transport: no stdin on spawned MCP server"
            );
            anyhow::Error::msg(format!("no stdin on MCP server `{}`", config.name))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "mcp_server": &config.name,
                        "missing": "stdout",
                    })),
                "mcp_transport: no stdout on spawned MCP server"
            );
            anyhow::Error::msg(format!("no stdout on MCP server `{}`", config.name))
        })?;
        // Fresh generation starts alive and not-exited.
        alive.store(true, Ordering::Release);
        child_exited.store(false, Ordering::Release);
        let server_name = config.name.clone();
        let reader = zeroclaw_spawn::spawn!(stdio_read_loop(
            server_name,
            generation,
            stdout,
            pending,
            alive,
            active_generation,
        ));

        let child = Arc::new(tokio::sync::Mutex::new(child));
        let watcher_child = Arc::clone(&child);
        let watcher_flag = Arc::clone(&child_exited);
        let exit_watcher =
            zeroclaw_spawn::spawn!(stdio_child_exit_watcher(watcher_child, watcher_flag,));

        Ok(StdioConn {
            generation,
            child,
            stdin,
            reader,
            child_exited,
            exit_watcher,
        })
    }

    async fn send_raw(&self, stdin: &mut tokio::process::ChildStdin, line: &str) -> Result<()> {
        stdin
            .write_all(line.as_bytes())
            .await
            .context("failed to write to MCP server stdin")?;
        #[cfg(all(test, unix))]
        let write_test_hook = self.write_test_hook.lock().clone();
        #[cfg(all(test, unix))]
        if let Some(hook) = write_test_hook {
            hook.pause_after_payload_if_armed().await;
        }
        stdin
            .write_all(b"\n")
            .await
            .context("failed to write newline to MCP server stdin")?;
        stdin.flush().await.context("failed to flush stdin")?;
        Ok(())
    }

    async fn reap_conn(conn: StdioConn, server_name: &str) -> Result<()> {
        // Clone the shared handles needed after the connection is gone.
        let child = Arc::clone(&conn.child);
        let child_exited = Arc::clone(&conn.child_exited);
        // Dropping the connection runs `StdioConn::Drop`, which aborts the
        // background tasks (releasing their `Arc<Mutex<Child>>` clones so they
        // cannot race the reaping `wait`) AND drops the sole `ChildStdin`. That
        // closes the server's stdin, delivering EOF so a server that shuts down
        // on EOF can exit on its own before any signal. (`AsyncWriteExt::shutdown`
        // is a no-op on `ChildStdin` in tokio: it returns `Ready(Ok(()))`
        // without closing the fd; only dropping the handle closes the pipe.)
        drop(conn);

        let mut child = child.lock().await;
        // Give the server a bounded courtesy window to exit on the EOF it just
        // saw. A server that honors EOF exits near-instantly, so this only adds
        // latency when a server ignores EOF and must be signalled regardless.
        // Escalate to a signal only if the child is still running afterward.
        if timeout(STDIO_CLOSE_GRACE, child.wait()).await.is_err() {
            child
                .start_kill()
                .with_context(|| format!("failed to kill MCP server `{server_name}` child"))?;
            child
                .wait()
                .await
                .with_context(|| format!("failed to reap MCP server `{server_name}` child"))?;
        }
        // The direct child is now gone regardless of stdout pipe state.
        child_exited.store(true, Ordering::Release);
        Ok(())
    }
}

/// Watch the *direct* child process for exit, independent of its stdout pipe.
///
/// A misbehaving MCP server can spawn a descendant that inherits stdout and
/// keeps the pipe open after the direct child exits; the stdout reader would
/// then never see EOF. This nonblocking watcher polls `try_wait` so a dead
/// direct child is observed and surfaced through `health_check` even while the
/// inherited pipe stays open.
async fn stdio_child_exit_watcher(
    child: Arc<tokio::sync::Mutex<Child>>,
    child_exited: Arc<AtomicBool>,
) {
    loop {
        {
            let mut guard = child.lock().await;
            match guard.try_wait() {
                Ok(Some(_status)) => {
                    child_exited.store(true, Ordering::Release);
                    return;
                }
                Ok(None) => {}
                // Treat an inspection error as a dead child: fail closed rather
                // than reporting a possibly-exited process as healthy.
                Err(_) => {
                    child_exited.store(true, Ordering::Release);
                    return;
                }
            }
        }
        tokio::time::sleep(STDIO_CHILD_POLL_INTERVAL).await;
    }
}

enum BoundedLine {
    Line(Vec<u8>),
    Oversized,
    Eof,
}

async fn read_bounded_line(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> std::io::Result<BoundedLine> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return if line.is_empty() {
                Ok(BoundedLine::Eof)
            } else if oversized {
                Ok(BoundedLine::Oversized)
            } else {
                Ok(BoundedLine::Line(line))
            };
        }

        let newline = buf.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buf.len(), |index| index + 1);
        let content_len = newline.unwrap_or(buf.len());
        if !oversized {
            if line.len().saturating_add(content_len) > MAX_LINE_BYTES {
                oversized = true;
                line.clear();
            } else {
                line.extend_from_slice(&buf[..content_len]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            if oversized {
                return Ok(BoundedLine::Oversized);
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(BoundedLine::Line(line));
        }
    }
}

fn drain_pending_generation(pending: &PendingMap, generation: u64) {
    let senders = {
        let mut guard = pending.lock();
        let keys: Vec<(u64, u64)> = guard
            .keys()
            .filter(|(entry_generation, _)| *entry_generation == generation)
            .copied()
            .collect();
        keys.into_iter()
            .filter_map(|key| guard.remove(&key))
            .collect::<Vec<_>>()
    };
    drop(senders);
}

fn register_pending(
    pending: &PendingMap,
    generation: u64,
    id: u64,
    sender: oneshot::Sender<JsonRpcResponse>,
) -> Result<()> {
    match pending.lock().entry((generation, id)) {
        Entry::Vacant(entry) => {
            entry.insert(sender);
            Ok(())
        }
        Entry::Occupied(_) => bail!("duplicate in-flight MCP request id {id}"),
    }
}

fn deliver_stdio_response(
    pending: &PendingMap,
    generation: u64,
    response: JsonRpcResponse,
) -> bool {
    let Some(id) = response.id.as_ref().and_then(serde_json::Value::as_u64) else {
        return false;
    };
    let sender = pending.lock().remove(&(generation, id));
    sender.is_some_and(|sender| sender.send(response).is_ok())
}

fn finish_stdio_generation(
    pending: &PendingMap,
    generation: u64,
    alive: &AtomicBool,
    active_generation: &AtomicU64,
) {
    if active_generation.load(Ordering::Acquire) == generation {
        alive.store(false, Ordering::Release);
        drain_pending_generation(pending, generation);
    }
}

async fn stdio_read_loop(
    server_name: String,
    generation: u64,
    stdout: tokio::process::ChildStdout,
    pending: PendingMap,
    alive: Arc<AtomicBool>,
    active_generation: Arc<AtomicU64>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_bounded_line(&mut reader).await {
            Ok(BoundedLine::Line(line)) => {
                let Ok(response) = serde_json::from_slice::<JsonRpcResponse>(&line) else {
                    continue;
                };
                let response_id = response.id.as_ref().and_then(serde_json::Value::as_u64);
                if !deliver_stdio_response(&pending, generation, response) {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "mcp_server": &server_name,
                                "response_id": response_id,
                                "generation": generation,
                            })),
                        "mcp_transport: dropped unknown or stale stdio response"
                    );
                }
            }
            Ok(BoundedLine::Oversized) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "mcp_server": &server_name,
                            "max_bytes": MAX_LINE_BYTES,
                        })),
                    "mcp_transport: dropped oversized stdio response"
                );
            }
            Ok(BoundedLine::Eof) | Err(_) => break,
        }
    }

    finish_stdio_generation(&pending, generation, &alive, &active_generation);
}

#[async_trait::async_trait]
impl SharedMcpTransportConn for StdioTransport {
    async fn send_and_recv(
        &self,
        request: &JsonRpcRequest,
        lifecycle: &McpRequestLifecycle,
    ) -> Result<JsonRpcResponse> {
        let line = serde_json::to_string(request)?;
        let epoch_guard = lifecycle.begin_write().await;
        #[cfg(all(test, unix))]
        if let Some(hook) = self.write_test_hook.lock().clone() {
            hook.note_attempt();
        }
        let state = self.state.lock().await;
        // Re-check recovery only after acquiring the real stdio writer
        // boundary. If an earlier writer was cancelled while holding `state`,
        // its boundary guard publishes recovery before releasing `state`, so
        // this queued writer cannot race onto the ambiguous child.
        let mut write_boundary = StdioWriterBoundary::new(state, lifecycle)?;
        let state = write_boundary.state_mut()?;
        if state.closed {
            return Err(McpTransportError::TransportClosed.into());
        }
        let conn = state
            .conn
            .as_mut()
            .ok_or(McpTransportError::TransportClosed)?;
        if !self.alive.load(Ordering::Acquire)
            || self.active_generation.load(Ordering::Acquire) != conn.generation
        {
            return Err(McpTransportError::TransportClosed.into());
        }

        let request_id = request.id.as_ref().and_then(serde_json::Value::as_u64);
        let receiver = if let Some(id) = request_id {
            let (sender, receiver) = oneshot::channel();
            register_pending(&self.pending, conn.generation, id, sender)?;
            Some((
                StdioPendingGuard {
                    pending: Arc::clone(&self.pending),
                    key: (conn.generation, id),
                },
                receiver,
            ))
        } else if request.id.is_some() {
            bail!("unsupported non-integer MCP request id");
        } else {
            None
        };

        lifecycle.mark_outcome_unknown(epoch_guard.epoch());
        if let Err(error) = self.send_raw(&mut conn.stdin, &line).await {
            self.alive.store(false, Ordering::Release);
            return Err(error);
        }
        write_boundary.release_state();
        drop(epoch_guard);

        let Some((_pending_guard, receiver)) = receiver else {
            lifecycle.mark_completed();
            drop(write_boundary);
            return Ok(JsonRpcResponse {
                jsonrpc: crate::mcp_protocol::JSONRPC_VERSION.to_string(),
                id: None,
                result: None,
                error: None,
            });
        };
        let response = receiver
            .await
            .map_err(|_| McpTransportError::TransportClosed)?;
        lifecycle.mark_completed();
        drop(write_boundary);
        Ok(response)
    }

    async fn reset(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.closed {
            bail!("MCP stdio transport is closed");
        }

        let old_generation = self.active_generation.fetch_add(1, Ordering::AcqRel);
        self.alive.store(false, Ordering::Release);
        drain_pending_generation(&self.pending, old_generation);
        if let Some(conn) = state.conn.take() {
            Self::reap_conn(conn, &self.config.name).await?;
        }

        let generation = old_generation.wrapping_add(1);
        let conn = Self::spawn(
            &self.config,
            generation,
            Arc::clone(&self.pending),
            Arc::clone(&self.alive),
            Arc::clone(&self.active_generation),
            Arc::clone(&self.child_exited),
        )?;
        state.conn = Some(conn);
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.closed {
            return Ok(());
        }
        state.closed = true;
        let old_generation = self.active_generation.fetch_add(1, Ordering::AcqRel);
        self.alive.store(false, Ordering::Release);
        drain_pending_generation(&self.pending, old_generation);
        if let Some(conn) = state.conn.take() {
            Self::reap_conn(conn, &self.config.name).await?;
        }
        Ok(())
    }

    fn health_check(&self) -> bool {
        // Healthy only when the reader still owns a live stream *and* the direct
        // child has not exited. The `child_exited` flag is driven by a
        // `try_wait`-based watcher independent of stdout EOF, so a parent that
        // exits while a descendant keeps the inherited stdout pipe open is
        // reported unhealthy instead of falsely alive.
        self.alive.load(Ordering::Acquire) && !self.child_exited.load(Ordering::Acquire)
    }
}

// ── HTTP Transport ───────────────────────────────────────────────────────

/// HTTP-based transport (POST requests).
pub struct HttpTransport {
    url: String,
    /// Per-server tool-call timeout, from `McpServerConfig.tool_timeout_secs`.
    /// Non-tool requests keep the legacy HTTP request timeout and short SSE
    /// read timeout. Tool calls use the configured budget when present; when
    /// absent, the client layer's outer tool-call timeout owns the budget.
    tool_timeout_secs: Option<u64>,
    client: reqwest::Client,
    headers: std::collections::HashMap<String, String>,
    session_id: ParkingMutex<Option<String>>,
}

impl HttpTransport {
    pub fn new(config: &McpServerConfig) -> Result<Self> {
        let url = config
            .url
            .as_ref()
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "mcp_server": &config.name,
                            "transport": "http",
                        })),
                    "mcp_transport: HTTP transport requires URL"
                );
                anyhow::Error::msg("URL required for HTTP transport")
            })?
            .clone();

        if config.tls_ca_cert_path.is_some() {
            require_https_url(&config.name, &url, "configured remote URL")?;
        }
        let client = build_remote_http_client(config)?;

        Ok(Self {
            url,
            tool_timeout_secs: config.tool_timeout_secs,
            client,
            headers: config.headers.clone(),
            session_id: ParkingMutex::new(None),
        })
    }

    fn apply_session_header(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(session_id) = self.session_id.lock().as_deref() {
            req.header(MCP_SESSION_ID_HEADER, session_id)
        } else {
            req
        }
    }

    fn update_session_id_from_headers(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(session_id) = headers
            .get(MCP_SESSION_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            *self.session_id.lock() = Some(session_id.to_string());
        }
    }
}

fn finish_response(
    request: &JsonRpcRequest,
    lifecycle: &McpRequestLifecycle,
    response: JsonRpcResponse,
) -> Result<JsonRpcResponse> {
    if response.id != request.id {
        bail!(
            "MCP response id mismatch: expected {:?}, received {:?}",
            request.id,
            response.id
        );
    }
    lifecycle.mark_completed();
    Ok(response)
}

#[async_trait::async_trait]
impl SharedMcpTransportConn for HttpTransport {
    async fn send_and_recv(
        &self,
        request: &JsonRpcRequest,
        lifecycle: &McpRequestLifecycle,
    ) -> Result<JsonRpcResponse> {
        let body = serde_json::to_string(request)?;

        let has_accept = self
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Accept"));
        let has_content_type = self
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Content-Type"));

        let mut req = apply_request_timeout(
            self.client.post(&self.url).body(body),
            http_request_timeout_secs(request, self.tool_timeout_secs),
        );
        if !has_content_type {
            req = req.header("Content-Type", MCP_JSON_CONTENT_TYPE);
        }
        for (key, value) in &self.headers {
            req = req.header(key, value);
        }
        if !has_accept {
            req = req.header("Accept", MCP_STREAMABLE_ACCEPT);
        }

        let epoch_guard = lifecycle.begin_write().await;
        req = self.apply_session_header(req);
        lifecycle.mark_outcome_unknown(epoch_guard.epoch());
        let resp = req
            .send()
            .await
            .context("HTTP request to MCP server failed")?;
        drop(epoch_guard);

        if !resp.status().is_success() {
            let status = resp.status();
            if self.session_id.lock().is_some()
                && (status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE)
            {
                return Err(McpTransportError::StaleSession {
                    status: status.as_u16(),
                }
                .into());
            }
            lifecycle.mark_completed();
            bail!("MCP server returned HTTP {}", status);
        }

        self.update_session_id_from_headers(resp.headers());

        if request.id.is_none() {
            return finish_response(
                request,
                lifecycle,
                JsonRpcResponse {
                    jsonrpc: crate::mcp_protocol::JSONRPC_VERSION.to_string(),
                    id: None,
                    result: None,
                    error: None,
                },
            );
        }

        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));
        if is_sse {
            let read_response = read_first_jsonrpc_from_sse_response(resp);
            let maybe_resp = if let Some(sse_timeout) =
                http_sse_read_timeout_secs(request, self.tool_timeout_secs)
            {
                timeout(Duration::from_secs(sse_timeout), read_response)
                    .await
                    .context("timeout waiting for MCP response from streamable HTTP SSE stream")??
            } else {
                read_response.await?
            };
            let response = maybe_resp.ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "mcp_transport: MCP server returned no response in SSE stream"
                );
                anyhow::Error::msg("MCP server returned no response in SSE stream")
            })?;
            return finish_response(request, lifecycle, response);
        }

        let resp_text = resp.text().await.context("failed to read HTTP response")?;
        let response = parse_jsonrpc_response_text(&resp_text)?;
        finish_response(request, lifecycle, response)
    }

    async fn reset(&self) -> Result<()> {
        // Drop the stale session so the next request re-initializes and the
        // server issues a fresh `Mcp-Session-Id`.
        *self.session_id.lock() = None;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

// ── SSE Transport ─────────────────────────────────────────────────────────

/// SSE-based transport (HTTP POST for requests, SSE for responses).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum SseStreamState {
    Unknown,
    Connected,
    Unsupported,
}

pub struct SseTransport {
    sse_url: String,
    server_name: String,
    tool_timeout_secs: Option<u64>,
    client: reqwest::Client,
    headers: std::collections::HashMap<String, String>,
    require_https: bool,
    conn: Mutex<SseConnState>,
    shared: std::sync::Arc<Mutex<SseSharedState>>,
    pending: SsePendingMap,
    notify: std::sync::Arc<Notify>,
}

struct SseConnState {
    stream_state: SseStreamState,
    shutdown_tx: Option<oneshot::Sender<()>>,
    reader_task: Option<tokio::task::JoinHandle<()>>,
}

impl SseTransport {
    pub fn new(config: &McpServerConfig) -> Result<Self> {
        let sse_url = config
            .url
            .as_ref()
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "mcp_server": &config.name,
                            "transport": "sse",
                        })),
                    "mcp_transport: SSE transport requires URL"
                );
                anyhow::Error::msg("URL required for SSE transport")
            })?
            .clone();

        let require_https = config.tls_ca_cert_path.is_some();
        if require_https {
            require_https_url(&config.name, &sse_url, "configured remote URL")?;
        }
        let client = build_remote_http_client(config)?;

        Ok(Self {
            sse_url,
            server_name: config.name.clone(),
            tool_timeout_secs: config.tool_timeout_secs,
            client,
            headers: config.headers.clone(),
            require_https,
            conn: Mutex::new(SseConnState {
                stream_state: SseStreamState::Unknown,
                shutdown_tx: None,
                reader_task: None,
            }),
            shared: std::sync::Arc::new(Mutex::new(SseSharedState::default())),
            pending: Arc::new(ParkingMutex::new(HashMap::new())),
            notify: std::sync::Arc::new(Notify::new()),
        })
    }

    async fn ensure_connected(&self) -> Result<SseStreamState> {
        let mut conn = self.conn.lock().await;
        if conn.stream_state == SseStreamState::Unsupported {
            return Ok(conn.stream_state);
        }
        if let Some(task) = &conn.reader_task
            && !task.is_finished()
        {
            conn.stream_state = SseStreamState::Connected;
            return Ok(conn.stream_state);
        }

        let has_accept = self
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Accept"));

        let mut req = self
            .client
            .get(&self.sse_url)
            .header("Cache-Control", "no-cache");
        for (key, value) in &self.headers {
            req = req.header(key, value);
        }
        if !has_accept {
            req = req.header("Accept", MCP_STREAMABLE_ACCEPT);
        }

        let resp = req.send().await.context("SSE GET to MCP server failed")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND
            || resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            conn.stream_state = SseStreamState::Unsupported;
            return Ok(conn.stream_state);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"status": status.as_u16()})),
                "mcp_transport: MCP server returned non-success HTTP"
            );
            return Err(anyhow::Error::msg(format!(
                "MCP server returned HTTP {}",
                status
            )));
        }
        let is_event_stream = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));
        if !is_event_stream {
            conn.stream_state = SseStreamState::Unsupported;
            return Ok(conn.stream_state);
        }

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        conn.shutdown_tx = Some(shutdown_tx);

        let shared = self.shared.clone();
        let pending = Arc::clone(&self.pending);
        let notify = self.notify.clone();
        let sse_url = self.sse_url.clone();
        let server_name = self.server_name.clone();

        conn.reader_task = Some(zeroclaw_spawn::spawn!(async move {
            let stream = resp
                .bytes_stream()
                .map(|item| item.map_err(std::io::Error::other));
            let reader = tokio_util::io::StreamReader::new(stream);
            let mut lines = BufReader::new(reader).lines();

            let mut cur_event: Option<String> = None;
            let mut cur_id: Option<String> = None;
            let mut cur_data: Vec<String> = Vec::new();

            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    line = lines.next_line() => {
                        let Ok(line_opt) = line else { break; };
                        let Some(mut line) = line_opt else { break; };
                        if line.ends_with('\r') {
                            line.pop();
                        }
                        if line.is_empty() {
                            if cur_event.is_none() && cur_id.is_none() && cur_data.is_empty() {
                                continue;
                            }
                            let event = cur_event.take();
                            let data = cur_data.join("\n");
                            cur_data.clear();
                            let id = cur_id.take();
                            handle_sse_event(&server_name, &sse_url, &shared, &pending, &notify, event.as_deref(), id.as_deref(), data).await;
                            continue;
                        }

                        if line.starts_with(':') {
                            continue;
                        }

                        if let Some(rest) = line.strip_prefix("event:") {
                            cur_event = Some(rest.trim().to_string());
                        }
                        if let Some(rest) = line.strip_prefix("data:") {
                            let rest = rest.strip_prefix(' ').unwrap_or(rest);
                            cur_data.push(rest.to_string());
                        }
                        if let Some(rest) = line.strip_prefix("id:") {
                            cur_id = Some(rest.trim().to_string());
                        }
                    }
                }
            }

            // Stream closed: drop every pending sender so each waiter observes a
            // `RecvError`, which `send_and_recv` maps to
            // `McpTransportError::TransportClosed` to trigger a reconnect.
            pending.lock().clear();
        }));
        conn.stream_state = SseStreamState::Connected;

        Ok(conn.stream_state)
    }

    async fn get_message_url(&self) -> Result<(String, bool)> {
        let guard = self.shared.lock().await;
        if let Some(url) = &guard.message_url {
            return Ok((url.clone(), guard.message_url_from_endpoint));
        }
        drop(guard);

        let derived = derive_message_url(&self.sse_url, "messages")
            .or_else(|| derive_message_url(&self.sse_url, "message"))
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"sse_url": &self.sse_url})),
                    "mcp_transport: invalid SSE URL"
                );
                anyhow::Error::msg("invalid SSE URL")
            })?;
        let mut guard = self.shared.lock().await;
        if guard.message_url.is_none() {
            guard.message_url = Some(derived.clone());
            guard.message_url_from_endpoint = false;
        }
        Ok((derived, false))
    }

    async fn teardown(&self) {
        let mut conn = self.conn.lock().await;
        if let Some(tx) = conn.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = conn.reader_task.take() {
            task.abort();
            let _ = task.await;
        }
        conn.stream_state = SseStreamState::Unknown;
        drop(conn);

        let mut shared = self.shared.lock().await;
        shared.message_url = None;
        shared.message_url_from_endpoint = false;
        drop(shared);
        self.pending.lock().clear();
    }
}

#[derive(Default)]
struct SseSharedState {
    message_url: Option<String>,
    message_url_from_endpoint: bool,
}

type SsePendingMap = Arc<ParkingMutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>;

struct SsePendingGuard {
    pending: SsePendingMap,
    id: u64,
}

impl Drop for SsePendingGuard {
    fn drop(&mut self) {
        self.pending.lock().remove(&self.id);
    }
}

fn derive_message_url(sse_url: &str, message_path: &str) -> Option<String> {
    let url = reqwest::Url::parse(sse_url).ok()?;
    let mut segments: Vec<&str> = url.path_segments()?.collect();
    if segments.is_empty() {
        return None;
    }
    if segments.last().copied() == Some("sse") {
        segments.pop();
        segments.push(message_path);
        let mut new_url = url.clone();
        new_url.set_path(&format!("/{}", segments.join("/")));
        return Some(new_url.to_string());
    }
    let mut new_url = url.clone();
    let mut path = url.path().trim_end_matches('/').to_string();
    path.push('/');
    path.push_str(message_path);
    new_url.set_path(&path);
    Some(new_url.to_string())
}

async fn handle_sse_event(
    server_name: &str,
    sse_url: &str,
    shared: &std::sync::Arc<Mutex<SseSharedState>>,
    pending: &SsePendingMap,
    notify: &std::sync::Arc<Notify>,
    event: Option<&str>,
    _id: Option<&str>,
    data: String,
) {
    let event = event.unwrap_or("message");
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return;
    }

    if event.eq_ignore_ascii_case("endpoint") || event.eq_ignore_ascii_case("mcp-endpoint") {
        if let Some(url) = parse_endpoint_from_data(sse_url, trimmed) {
            let mut guard = shared.lock().await;
            guard.message_url = Some(url);
            guard.message_url_from_endpoint = true;
            drop(guard);
            notify.notify_waiters();
        }
        return;
    }

    if !event.eq_ignore_ascii_case("message") {
        return;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return;
    };

    let Ok(resp) = serde_json::from_value::<JsonRpcResponse>(value.clone()) else {
        let _ = serde_json::from_value::<JsonRpcRequest>(value);
        return;
    };

    let Some(id_val) = resp.id.clone() else {
        return;
    };
    let id = match id_val.as_u64() {
        Some(v) => v,
        None => return,
    };

    let tx = pending.lock().remove(&id);
    if let Some(tx) = tx {
        let _ = tx.send(resp);
    } else {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "MCP SSE `{}` received response for unknown id {}",
                server_name, id
            )
        );
    }
}

fn parse_endpoint_from_data(sse_url: &str, data: &str) -> Option<String> {
    if data.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(data).ok()?;
        let endpoint = v.get("endpoint")?.as_str()?;
        return parse_endpoint_from_data(sse_url, endpoint);
    }
    if data.starts_with("http://") || data.starts_with("https://") {
        return Some(data.to_string());
    }
    let base = reqwest::Url::parse(sse_url).ok()?;
    base.join(data).ok().map(|u| u.to_string())
}

fn extract_json_from_sse_text(resp_text: &str) -> Cow<'_, str> {
    let text = resp_text.trim_start_matches('\u{feff}');
    let mut current_data_lines: Vec<&str> = Vec::new();
    let mut last_event_data_lines: Vec<&str> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r').trim_start();
        if line.is_empty() {
            if !current_data_lines.is_empty() {
                last_event_data_lines = std::mem::take(&mut current_data_lines);
            }
            continue;
        }

        if line.starts_with(':') {
            continue;
        }

        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            current_data_lines.push(rest);
        }
    }

    if !current_data_lines.is_empty() {
        last_event_data_lines = current_data_lines;
    }

    if last_event_data_lines.is_empty() {
        return Cow::Borrowed(text.trim());
    }

    if last_event_data_lines.len() == 1 {
        return Cow::Borrowed(last_event_data_lines[0].trim());
    }

    let joined = last_event_data_lines.join("\n");
    Cow::Owned(joined.trim().to_string())
}

fn parse_jsonrpc_response_text(resp_text: &str) -> Result<JsonRpcResponse> {
    let trimmed = resp_text.trim();
    if trimmed.is_empty() {
        bail!("MCP server returned no response");
    }

    let json_text = if looks_like_sse_text(trimmed) {
        extract_json_from_sse_text(trimmed)
    } else {
        Cow::Borrowed(trimmed)
    };

    let mcp_resp: JsonRpcResponse = serde_json::from_str(json_text.as_ref())
        .with_context(|| format!("invalid JSON-RPC response: {}", resp_text))?;
    Ok(mcp_resp)
}

fn looks_like_sse_text(text: &str) -> bool {
    text.starts_with("data:")
        || text.starts_with("event:")
        || text.contains("\ndata:")
        || text.contains("\nevent:")
}

async fn read_first_jsonrpc_from_sse_response(
    resp: reqwest::Response,
) -> Result<Option<JsonRpcResponse>> {
    let stream = resp
        .bytes_stream()
        .map(|item| item.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(stream);
    let mut lines = BufReader::new(reader).lines();

    let mut cur_event: Option<String> = None;
    let mut cur_data: Vec<String> = Vec::new();

    while let Ok(line_opt) = lines.next_line().await {
        let Some(mut line) = line_opt else { break };
        if line.ends_with('\r') {
            line.pop();
        }
        if line.is_empty() {
            if cur_event.is_none() && cur_data.is_empty() {
                continue;
            }
            let event = cur_event.take();
            let data = cur_data.join("\n");
            cur_data.clear();

            let event = event.unwrap_or_else(|| "message".to_string());
            if event.eq_ignore_ascii_case("endpoint") || event.eq_ignore_ascii_case("mcp-endpoint")
            {
                continue;
            }
            if !event.eq_ignore_ascii_case("message") {
                continue;
            }

            let trimmed = data.trim();
            if trimmed.is_empty() {
                continue;
            }
            let json_str = extract_json_from_sse_text(trimmed);
            if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(json_str.as_ref()) {
                return Ok(Some(resp));
            }
            continue;
        }

        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            cur_event = Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            cur_data.push(rest.to_string());
        }
    }

    Ok(None)
}

#[async_trait::async_trait]
impl SharedMcpTransportConn for SseTransport {
    async fn send_and_recv(
        &self,
        request: &JsonRpcRequest,
        lifecycle: &McpRequestLifecycle,
    ) -> Result<JsonRpcResponse> {
        let stream_state = self.ensure_connected().await?;

        let id = request.id.as_ref().and_then(|v| v.as_u64());
        if request.id.is_some() && id.is_none() {
            bail!("unsupported non-integer MCP request id");
        }
        let body = serde_json::to_string(request)?;

        let (mut message_url, mut from_endpoint) = self.get_message_url().await?;
        if stream_state == SseStreamState::Connected && !from_endpoint {
            for _ in 0..3 {
                {
                    let guard = self.shared.lock().await;
                    if guard.message_url_from_endpoint
                        && let Some(url) = &guard.message_url
                    {
                        message_url = url.clone();
                        from_endpoint = true;
                        break;
                    }
                }
                let _ = timeout(Duration::from_millis(300), self.notify.notified()).await;
            }
        }
        let message_url = if from_endpoint {
            message_url.clone()
        } else {
            self.sse_url.clone()
        };

        // The message endpoint can be supplied by the server via the SSE
        // `endpoint` event, so re-check it here: a custom CA must never be
        // downgraded to plaintext by a server-controlled redirect target.
        if self.require_https {
            require_https_url(&self.server_name, &message_url, "SSE message endpoint")?;
        }

        // Acquire the epoch permit before registering a response waiter.
        // Cancellation while waiting for the permit is provably pre-write and
        // therefore cannot leak a pending sender.
        let epoch_guard = lifecycle.begin_write().await;
        let mut rx = None;
        if let Some(id) = id
            && stream_state == SseStreamState::Connected
        {
            let (tx, ch) = oneshot::channel();
            match self.pending.lock().entry(id) {
                Entry::Vacant(entry) => {
                    entry.insert(tx);
                }
                Entry::Occupied(_) => {
                    bail!("duplicate in-flight MCP request id {id}");
                }
            }
            rx = Some((
                SsePendingGuard {
                    pending: Arc::clone(&self.pending),
                    id,
                },
                ch,
            ));
        }

        let has_accept = self
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Accept"));
        let has_content_type = self
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Content-Type"));
        let mut req = apply_request_timeout(
            self.client.post(&message_url).body(body),
            http_request_timeout_secs(request, self.tool_timeout_secs),
        );
        if !has_content_type {
            req = req.header("Content-Type", MCP_JSON_CONTENT_TYPE);
        }
        for (key, value) in &self.headers {
            req = req.header(key, value);
        }
        if !has_accept {
            req = req.header("Accept", MCP_STREAMABLE_ACCEPT);
        }

        lifecycle.mark_outcome_unknown(epoch_guard.epoch());
        let resp = req.send().await.context("SSE POST to MCP server failed")?;
        let status = resp.status();
        let mut got_direct = None;

        if status.is_success() {
            if request.id.is_none() {
                got_direct = Some(JsonRpcResponse {
                    jsonrpc: crate::mcp_protocol::JSONRPC_VERSION.to_string(),
                    id: None,
                    result: None,
                    error: None,
                });
            } else {
                let is_sse = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));

                if is_sse {
                    got_direct = read_first_jsonrpc_from_sse_response(resp).await?;
                } else {
                    let text = resp.text().await.unwrap_or_default();
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let json_str =
                            if trimmed.contains("\ndata:") || trimmed.starts_with("data:") {
                                extract_json_from_sse_text(trimmed)
                            } else {
                                Cow::Borrowed(trimmed)
                            };
                        if let Ok(mcp_resp) =
                            serde_json::from_str::<JsonRpcResponse>(json_str.as_ref())
                        {
                            got_direct = Some(mcp_resp);
                        }
                    }
                }
            }
        }
        drop(epoch_guard);

        if let Some(resp) = got_direct {
            return finish_response(request, lifecycle, resp);
        }

        if !status.is_success() {
            if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
                return Err(McpTransportError::StaleSession {
                    status: status.as_u16(),
                }
                .into());
            }
            bail!("MCP server returned HTTP {}", status);
        }

        let Some((_pending_guard, rx)) = rx else {
            bail!("MCP server returned no response");
        };

        // A dropped receiver means the SSE reader task tore down the stream
        // before our response arrived — recoverable via reconnect.
        rx.await
            .map_err(|_| McpTransportError::TransportClosed.into())
            .and_then(|response| finish_response(request, lifecycle, response))
    }

    async fn reset(&self) -> Result<()> {
        // Tear down the reader task and clear the cached endpoint/session state
        // so the next send re-handshakes: a fresh GET stream and a new
        // `endpoint` event from the (possibly restarted) server.
        self.teardown().await;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.teardown().await;
        Ok(())
    }
}

macro_rules! impl_legacy_transport {
    ($transport:ty) => {
        #[async_trait::async_trait]
        impl McpTransportConn for $transport {
            async fn send_and_recv(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse> {
                let lifecycle = McpRequestLifecycle::uncoordinated(0);
                SharedMcpTransportConn::send_and_recv(self, request, &lifecycle).await
            }

            async fn reset(&mut self) -> Result<()> {
                SharedMcpTransportConn::reset(self).await
            }

            fn health_check(&mut self) -> bool {
                SharedMcpTransportConn::health_check(self)
            }

            async fn close(&mut self) -> Result<()> {
                SharedMcpTransportConn::close(self).await
            }
        }
    };
}

impl_legacy_transport!(StdioTransport);
impl_legacy_transport!(HttpTransport);
impl_legacy_transport!(SseTransport);

// ── Factory ──────────────────────────────────────────────────────────────

/// Create a transport based on config.
pub fn create_transport(config: &McpServerConfig) -> Result<Box<dyn McpTransportConn>> {
    match config.transport {
        McpTransport::Stdio => Ok(Box::new(StdioTransport::new(config)?)),
        McpTransport::Http => Ok(Box::new(HttpTransport::new(config)?)),
        McpTransport::Sse => Ok(Box::new(SseTransport::new(config)?)),
    }
}

pub(crate) fn create_shared_transport(
    config: &McpServerConfig,
) -> Result<Box<dyn SharedMcpTransportConn>> {
    match config.transport {
        McpTransport::Stdio => Ok(Box::new(StdioTransport::new(config)?)),
        McpTransport::Http => Ok(Box::new(HttpTransport::new(config)?)),
        McpTransport::Sse => Ok(Box::new(SseTransport::new(config)?)),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct TestTlsServer {
        url: String,
        ca_pem: String,
        task: tokio::task::JoinHandle<()>,
    }

    #[derive(Clone)]
    enum TestTlsBehavior {
        JsonRpc,
        Sse,
        Redirect(String),
    }

    fn test_ca_file() -> tempfile::NamedTempFile {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};

        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["ZeroClaw MCP test CA".into()]).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), cert.pem()).unwrap();
        file
    }

    /// Install the `ring` `CryptoProvider` for this process (idempotent).
    ///
    /// The workspace test build links both `ring` (this crate) and `aws-lc-rs`
    /// (pulled in transitively by the `matrix-sdk` dev-dependency), so rustls
    /// cannot infer a process-level provider from crate features and panics
    /// inside `ServerConfig::builder()`. Production is unaffected: the remote
    /// transports build their clients through reqwest's `__rustls-ring`
    /// feature, which selects the provider explicitly.
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    async fn spawn_test_tls_server() -> TestTlsServer {
        spawn_test_tls_server_with_san("127.0.0.1").await
    }

    async fn spawn_test_tls_server_with_san(server_san: &str) -> TestTlsServer {
        spawn_test_tls_server_with_behavior(server_san, TestTlsBehavior::JsonRpc).await
    }

    async fn serve_test_tls_connection(
        stream: tokio::net::TcpStream,
        acceptor: tokio_rustls::TlsAcceptor,
        addr: std::net::SocketAddr,
        behavior: TestTlsBehavior,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let Ok(mut stream) = acceptor.accept(stream).await else {
            return;
        };
        let mut request = vec![0_u8; 4096];
        let bytes_read = stream.read(&mut request).await.unwrap();
        let is_get = request[..bytes_read].starts_with(b"GET ");

        let response = match behavior {
            TestTlsBehavior::JsonRpc => {
                let body = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
            TestTlsBehavior::Sse if is_get => {
                let body = format!("event: endpoint\ndata: https://{addr}/messages\n\n");
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
            TestTlsBehavior::Sse => {
                let body = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
            TestTlsBehavior::Redirect(location) => {
                format!(
                    "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                )
            }
        };
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    }

    async fn spawn_test_tls_server_with_behavior(
        server_san: &str,
        behavior: TestTlsBehavior,
    ) -> TestTlsServer {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
        use rustls::pki_types::PrivatePkcs8KeyDer;
        use std::sync::Arc;
        use tokio_rustls::TlsAcceptor;

        ensure_crypto_provider();

        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(vec!["ZeroClaw MCP test CA".into()]).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let server_key = KeyPair::generate().unwrap();
        let mut server_params = CertificateParams::new(vec![server_san.into()]).unwrap();
        server_params.is_ca = IsCa::NoCa;
        let server_cert = server_params
            .signed_by(&server_key, &ca_cert, &ca_key)
            .unwrap();
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![server_cert.der().clone()],
                PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
            )
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connection_count = if matches!(&behavior, TestTlsBehavior::Sse) {
            2
        } else {
            1
        };
        let task = ::zeroclaw_spawn::spawn!(async move {
            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let mut handlers = Vec::with_capacity(connection_count);
            for _ in 0..connection_count {
                let (stream, _) = listener.accept().await.unwrap();
                let connection_acceptor = acceptor.clone();
                let connection_behavior = behavior.clone();
                handlers.push(::zeroclaw_spawn::spawn!(serve_test_tls_connection(
                    stream,
                    connection_acceptor,
                    addr,
                    connection_behavior,
                )));
            }
            for handler in handlers {
                handler.await.unwrap();
            }
        });

        TestTlsServer {
            url: format!("https://{addr}/mcp"),
            ca_pem: ca_cert.pem(),
            task,
        }
    }

    #[tokio::test]
    async fn stdio_routes_only_exact_numeric_id_and_preserves_other_waiters() {
        let pending: PendingMap = Arc::new(ParkingMutex::new(HashMap::new()));
        let (sender, receiver) = oneshot::channel();
        register_pending(&pending, 7, 5, sender).expect("register waiter");

        assert!(!deliver_stdio_response(
            &pending,
            7,
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: Some(serde_json::json!(6)),
                result: Some(serde_json::json!("wrong")),
                error: None,
            }
        ));
        assert!(pending.lock().contains_key(&(7, 5)));
        assert!(!deliver_stdio_response(
            &pending,
            7,
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: Some(serde_json::json!("5")),
                result: Some(serde_json::json!("wrong shape")),
                error: None,
            }
        ));
        assert!(pending.lock().contains_key(&(7, 5)));

        assert!(deliver_stdio_response(
            &pending,
            7,
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: Some(serde_json::json!(5)),
                result: Some(serde_json::json!("correct")),
                error: None,
            }
        ));
        let response = receiver.await.expect("exact-id response");
        assert_eq!(response.result, Some(serde_json::json!("correct")));
    }

    #[tokio::test]
    async fn duplicate_stdio_id_does_not_evict_original_waiter() {
        let pending: PendingMap = Arc::new(ParkingMutex::new(HashMap::new()));
        let (original_sender, original_receiver) = oneshot::channel();
        register_pending(&pending, 3, 9, original_sender).expect("register original");
        let (duplicate_sender, duplicate_receiver) = oneshot::channel();
        let error = register_pending(&pending, 3, 9, duplicate_sender)
            .expect_err("duplicate id must be rejected");
        assert!(error.to_string().contains("duplicate in-flight"));
        drop(duplicate_receiver);

        assert!(deliver_stdio_response(
            &pending,
            3,
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: Some(serde_json::json!(9)),
                result: Some(serde_json::json!("original")),
                error: None,
            }
        ));
        assert_eq!(
            original_receiver
                .await
                .expect("original waiter must remain registered")
                .result,
            Some(serde_json::json!("original"))
        );
    }

    #[tokio::test]
    async fn old_stdio_reader_finalizer_cannot_drain_new_generation() {
        let pending: PendingMap = Arc::new(ParkingMutex::new(HashMap::new()));
        let (old_sender, old_receiver) = oneshot::channel();
        let (new_sender, new_receiver) = oneshot::channel();
        register_pending(&pending, 1, 3, old_sender).expect("old waiter");
        register_pending(&pending, 2, 3, new_sender).expect("new waiter");
        let alive = AtomicBool::new(true);
        let active_generation = AtomicU64::new(2);

        finish_stdio_generation(&pending, 1, &alive, &active_generation);

        assert!(alive.load(Ordering::Acquire));
        assert!(pending.lock().contains_key(&(1, 3)));
        assert!(pending.lock().contains_key(&(2, 3)));
        drop(old_receiver);
        drop(new_receiver);
    }

    #[tokio::test]
    async fn late_stdio_response_from_old_generation_cannot_reach_new_waiter() {
        let pending: PendingMap = Arc::new(ParkingMutex::new(HashMap::new()));
        let (new_sender, new_receiver) = oneshot::channel();
        register_pending(&pending, 2, 3, new_sender).expect("new waiter");

        assert!(!deliver_stdio_response(
            &pending,
            1,
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: Some(serde_json::json!(3)),
                result: Some(serde_json::json!("late")),
                error: None,
            }
        ));
        assert!(pending.lock().contains_key(&(2, 3)));
        assert!(deliver_stdio_response(
            &pending,
            2,
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: Some(serde_json::json!(3)),
                result: Some(serde_json::json!("current")),
                error: None,
            }
        ));
        assert_eq!(
            new_receiver
                .await
                .expect("new waiter must receive response")
                .result,
            Some(serde_json::json!("current"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_direct_stdio_request_removes_pending_waiter() {
        let config = McpServerConfig {
            name: "stdio-cancel".into(),
            transport: McpTransport::Stdio,
            command: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "while IFS= read -r line; do exec tail -f /dev/null; done".into(),
            ],
            ..Default::default()
        };
        let transport = Arc::new(StdioTransport::new(&config).expect("build transport"));
        let task_transport = Arc::clone(&transport);
        let request = JsonRpcRequest::new(7, "tools/call", serde_json::json!({}));
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let call = zeroclaw_spawn::spawn!(async move {
            SharedMcpTransportConn::send_and_recv(task_transport.as_ref(), &request, &lifecycle)
                .await
        });
        timeout(Duration::from_secs(2), async {
            while transport.pending.lock().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("request did not register a waiter");

        call.abort();
        assert!(
            call.await
                .expect_err("call must be cancelled")
                .is_cancelled()
        );
        assert!(transport.pending.lock().is_empty());
        SharedMcpTransportConn::close(transport.as_ref())
            .await
            .expect("close transport");
    }

    /// `close()` must deliver stdin EOF and let a well-behaved server exit on
    /// its own before escalating to a signal. The stub reads stdin to EOF, then
    /// writes a marker and exits 0; a force-kill that landed before EOF would
    /// SIGKILL the shell before the marker write, so the marker's presence
    /// proves the graceful path ran.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_close_delivers_eof_before_killing_the_server() {
        let marker =
            std::env::temp_dir().join(format!("zeroclaw_stdio_eof_marker_{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);

        let config = McpServerConfig {
            name: "stdio-graceful-eof".into(),
            transport: McpTransport::Stdio,
            command: "/bin/sh".into(),
            // The marker path is passed positionally (`$1`), never interpolated
            // into the script, so it is safe under a TMPDIR with spaces.
            args: vec![
                "-c".into(),
                "cat >/dev/null; printf done > \"$1\"".into(),
                "sh".into(),
                marker.to_string_lossy().into_owned(),
            ],
            ..Default::default()
        };
        let transport = StdioTransport::new(&config).expect("build transport");

        SharedMcpTransportConn::close(&transport)
            .await
            .expect("close transport");

        assert!(
            marker.exists(),
            "close() did not deliver stdin EOF before killing: the server was \
             signalled before it could observe EOF and write its marker"
        );
        let _ = std::fs::remove_file(&marker);
    }

    /// When the direct child exits but a descendant keeps the inherited stdout
    /// pipe open (so the reader never sees EOF), `health_check` must still
    /// report the transport unhealthy. It relies on the nonblocking
    /// direct-child-exit watcher, not on stdout EOF.
    #[cfg(unix)]
    #[tokio::test]
    async fn health_check_detects_direct_child_exit_with_inherited_stdout_open() {
        let config = McpServerConfig {
            name: "stdio-orphan-stdout".into(),
            transport: McpTransport::Stdio,
            command: "/bin/sh".into(),
            // Background a bounded process that inherits stdout, then the
            // direct shell child exits immediately. stdout stays open long
            // enough to prove the direct-child watcher wins over EOF.
            args: vec!["-c".into(), "sleep 2 & exit 0".into()],
            ..Default::default()
        };
        let transport = StdioTransport::new(&config).expect("build transport");

        // Once the direct child exits, the watcher must flip health to false
        // even though stdout (held by the descendant) never reached EOF.
        let became_unhealthy = timeout(Duration::from_secs(5), async {
            loop {
                if !SharedMcpTransportConn::health_check(&transport) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            became_unhealthy.is_ok(),
            "health_check kept reporting healthy after the direct child exited \
             (inherited stdout stayed open, so EOF alone is insufficient)"
        );

        SharedMcpTransportConn::close(&transport)
            .await
            .expect("close transport");
    }

    #[tokio::test]
    async fn sse_pre_write_cancellation_does_not_leak_pending_waiter() {
        use std::future::Future;
        use std::task::{Context as TaskContext, Poll};

        let config = McpServerConfig {
            name: "sse-cancel".into(),
            transport: McpTransport::Sse,
            url: Some("http://localhost:1/sse".into()),
            ..Default::default()
        };
        let transport = SseTransport::new(&config).expect("build transport");
        let reader = zeroclaw_spawn::spawn!(std::future::pending::<()>());
        {
            let mut conn = transport.conn.lock().await;
            conn.stream_state = SseStreamState::Connected;
            conn.reader_task = Some(reader);
        }
        {
            let mut shared = transport.shared.lock().await;
            shared.message_url = Some("http://localhost:1/messages".into());
            shared.message_url_from_endpoint = true;
        }

        let epoch_gate = Arc::new(RwLock::new(0));
        let epoch_writer = epoch_gate.write().await;
        let lifecycle = McpRequestLifecycle::coordinated(Arc::clone(&epoch_gate), None);
        let request = JsonRpcRequest::new(7, "tools/call", serde_json::json!({}));
        let mut send = Box::pin(SharedMcpTransportConn::send_and_recv(
            &transport, &request, &lifecycle,
        ));
        let waker = futures_util::task::noop_waker();
        let mut context = TaskContext::from_waker(&waker);
        assert!(matches!(send.as_mut().poll(&mut context), Poll::Pending));
        drop(send);

        assert!(lifecycle.outcome_unknown_epoch().is_none());
        assert!(transport.pending.lock().is_empty());
        drop(epoch_writer);
        SharedMcpTransportConn::close(&transport)
            .await
            .expect("close transport");
    }

    #[test]
    fn test_transport_default_is_stdio() {
        let config = McpServerConfig::default();
        assert_eq!(config.transport, McpTransport::Stdio);
    }

    #[test]
    fn test_http_transport_requires_url() {
        let config = McpServerConfig {
            name: "test".into(),
            transport: McpTransport::Http,
            ..Default::default()
        };
        assert!(HttpTransport::new(&config).is_err());
    }

    #[test]
    fn test_sse_transport_requires_url() {
        let config = McpServerConfig {
            name: "test".into(),
            transport: McpTransport::Sse,
            ..Default::default()
        };
        assert!(SseTransport::new(&config).is_err());
    }

    #[test]
    fn remote_transports_without_custom_ca_build_unchanged() {
        let http = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("https://localhost/mcp".into()),
            ..Default::default()
        };
        let sse = McpServerConfig {
            name: "test-sse".into(),
            transport: McpTransport::Sse,
            url: Some("https://localhost/sse".into()),
            ..Default::default()
        };
        assert!(HttpTransport::new(&http).is_ok());
        assert!(SseTransport::new(&sse).is_ok());
    }

    #[test]
    fn remote_transports_with_custom_ca_reject_plaintext_configured_url() {
        let ca_file = test_ca_file();
        for transport in [McpTransport::Http, McpTransport::Sse] {
            let config = McpServerConfig {
                name: "internal".into(),
                transport,
                url: Some("http://internal.example/mcp".into()),
                tls_ca_cert_path: Some(ca_file.path().to_string_lossy().into_owned()),
                ..Default::default()
            };
            let error = create_transport(&config)
                .err()
                .expect("custom CA must reject a plaintext configured URL");
            let message = error.to_string();
            assert!(message.contains("internal"));
            assert!(message.contains("requires an HTTPS configured remote URL"));
            assert!(message.contains("refusing plaintext transport"));
        }
    }

    #[tokio::test]
    async fn custom_ca_rejects_plaintext_endpoint_advertised_by_https_sse_stream() {
        let ca_file = test_ca_file();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Sse,
            url: Some("https://internal.example/sse".into()),
            tls_ca_cert_path: Some(ca_file.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let transport = SseTransport::new(&config).expect("HTTPS SSE transport should build");

        handle_sse_event(
            &transport.server_name,
            &transport.sse_url,
            &transport.shared,
            &transport.pending,
            &transport.notify,
            Some("endpoint"),
            None,
            "http://internal.example/messages".to_string(),
        )
        .await;
        // Mark the stream unsupported so the send path uses the cached
        // endpoint directly instead of trying to open a real GET stream.
        transport.conn.lock().await.stream_state = SseStreamState::Unsupported;

        let request = JsonRpcRequest::new(1, "initialize", serde_json::json!({}));
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let error = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .expect_err("custom CA must reject a plaintext advertised endpoint");
        let message = error.to_string();
        assert!(message.contains("internal"));
        assert!(message.contains("requires an HTTPS SSE message endpoint"));
        assert!(message.contains("refusing plaintext transport"));
    }

    #[test]
    fn remote_transport_rejects_relative_custom_ca_path() {
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some("https://localhost/mcp".into()),
            tls_ca_cert_path: Some("internal-ca.pem".into()),
            ..Default::default()
        };
        let error = HttpTransport::new(&config)
            .err()
            .expect("relative path must fail");
        let message = error.to_string();
        assert!(message.contains("internal"));
        assert!(message.contains("must be absolute"));
    }

    #[test]
    fn both_remote_transports_fail_closed_for_missing_custom_ca() {
        for transport in [McpTransport::Http, McpTransport::Sse] {
            let config = McpServerConfig {
                name: "internal".into(),
                transport,
                url: Some("https://localhost/mcp".into()),
                tls_ca_cert_path: Some("/nonexistent/zeroclaw-internal-ca.pem".into()),
                ..Default::default()
            };
            let error = create_transport(&config)
                .err()
                .expect("missing CA must fail");
            let message = error.to_string();
            assert!(message.contains("internal"));
            assert!(message.contains("/nonexistent/zeroclaw-internal-ca.pem"));
            assert!(!message.contains("BEGIN CERTIFICATE"));
        }
    }

    #[test]
    fn remote_transport_fails_closed_for_invalid_custom_ca() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"not a certificate").unwrap();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some("https://localhost/mcp".into()),
            tls_ca_cert_path: Some(file.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let error = HttpTransport::new(&config)
            .err()
            .expect("invalid CA must fail");
        let message = error.to_string();
        assert!(message.contains("internal"));
        assert!(message.contains(&file.path().to_string_lossy().to_string()));
        assert!(!message.contains("not a certificate"));
    }

    #[test]
    fn remote_transport_rejects_oversized_custom_ca_before_reading() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_TLS_CA_BYTES as u64 + 1).unwrap();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some("https://localhost/mcp".into()),
            tls_ca_cert_path: Some(file.path().to_string_lossy().into_owned()),
            ..Default::default()
        };

        let error = HttpTransport::new(&config)
            .err()
            .expect("oversized CA must fail before PEM parsing");
        let message = error.to_string();
        assert!(message.contains("internal"));
        assert!(message.contains("exceeds"));
        assert!(message.contains(&MAX_TLS_CA_BYTES.to_string()));
    }

    #[test]
    fn remote_transport_rejects_non_regular_custom_ca_path() {
        let directory = tempfile::TempDir::new().unwrap();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some("https://localhost/mcp".into()),
            tls_ca_cert_path: Some(directory.path().to_string_lossy().into_owned()),
            ..Default::default()
        };

        let error = HttpTransport::new(&config)
            .err()
            .expect("a directory must not be read as a CA bundle");
        // The directory is refused by whichever layer sees it first. POSIX
        // opens it and rejects the classified handle; Windows cannot open a
        // directory as a file at all (`OpenOptions::open` needs
        // `FILE_FLAG_BACKUP_SEMANTICS`, which the loader deliberately does not
        // pass), so the refusal surfaces as the read failure instead. Either
        // way the directory never reaches the PEM parser.
        let message = error.to_string();
        assert!(
            message.contains("must name a regular file")
                || message.contains("cannot read TLS CA certificate"),
            "a directory CA path must be refused, got: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_transport_rejects_special_custom_ca_file() {
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some("https://localhost/mcp".into()),
            tls_ca_cert_path: Some("/dev/zero".into()),
            ..Default::default()
        };

        let error = HttpTransport::new(&config)
            .err()
            .expect("a non-terminating device must be rejected");
        assert!(error.to_string().contains("must name a regular file"));
    }

    /// Create a FIFO at `path` using the POSIX utility, so the test does not
    /// need `unsafe` to reach `mkfifo(3)`.
    #[cfg(unix)]
    fn make_test_fifo(path: &std::path::Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("mkfifo must be available on unix");
        assert!(status.success(), "mkfifo failed for {}", path.display());
    }

    /// A CA path that alternates between a regular file and a FIFO must never
    /// park the loader. The pathname is classified only through the opened
    /// handle, so the losing side of the race is rejected rather than blocking
    /// MCP registry startup on a writer that never arrives.
    #[cfg(unix)]
    #[test]
    fn custom_ca_path_swapped_for_a_fifo_returns_instead_of_hanging() {
        let directory = tempfile::TempDir::new().unwrap();
        let ca_path = directory.path().join("rotating-ca.pem");
        let pem = test_ca_file();
        let pem_bytes = std::fs::read(pem.path()).unwrap();

        let config_for = |path: &std::path::Path| McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some("https://localhost/mcp".into()),
            tls_ca_cert_path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        };

        // Alternate the same path between a readable bundle and a FIFO. Each
        // iteration must terminate: the regular file loads, the FIFO is
        // rejected as a non-regular file. A blocking open would hang here
        // forever because nothing ever opens the FIFO for writing.
        for _ in 0..10 {
            std::fs::write(&ca_path, &pem_bytes).unwrap();
            load_tls_ca_pem(&config_for(&ca_path), &ca_path.to_string_lossy())
                .expect("a regular CA bundle must still load");

            std::fs::remove_file(&ca_path).unwrap();
            make_test_fifo(&ca_path);
            let error = load_tls_ca_pem(&config_for(&ca_path), &ca_path.to_string_lossy())
                .expect_err("a FIFO must be rejected, not opened for reading");
            assert!(error.to_string().contains("must name a regular file"));

            std::fs::remove_file(&ca_path).unwrap();
        }
    }

    /// Certificate rotation and mounted-secret deployments publish CA bundles
    /// through symlink indirection, so a symlink to a bounded regular file is
    /// supported. A symlink pointing at a special file is still rejected,
    /// because classification happens on the opened handle.
    #[cfg(unix)]
    #[test]
    fn custom_ca_follows_symlinks_to_regular_files_but_not_to_special_files() {
        let directory = tempfile::TempDir::new().unwrap();
        let pem = test_ca_file();
        let pem_bytes = std::fs::read(pem.path()).unwrap();

        let target = directory.path().join("real-ca.pem");
        std::fs::write(&target, &pem_bytes).unwrap();
        let link = directory.path().join("linked-ca.pem");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let config_for = |path: &std::path::Path| McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some("https://localhost/mcp".into()),
            tls_ca_cert_path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        };

        let loaded = load_tls_ca_pem(&config_for(&link), &link.to_string_lossy())
            .expect("a symlink to a bounded regular CA file must load");
        assert_eq!(loaded, pem_bytes);

        let fifo_target = directory.path().join("special");
        make_test_fifo(&fifo_target);
        let fifo_link = directory.path().join("linked-special");
        std::os::unix::fs::symlink(&fifo_target, &fifo_link).unwrap();

        let error = load_tls_ca_pem(&config_for(&fifo_link), &fifo_link.to_string_lossy())
            .expect_err("a symlink to a FIFO must be rejected");
        assert!(error.to_string().contains("must name a regular file"));
    }

    #[tokio::test]
    async fn private_ca_http_fails_unset_and_succeeds_with_matching_ca() {
        let request = JsonRpcRequest::new(1, "initialize", serde_json::json!({}));

        let server = spawn_test_tls_server().await;
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some(server.url),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).unwrap();
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let error = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .expect_err("a private CA must remain untrusted when the field is unset");
        assert!(
            error
                .to_string()
                .contains("HTTP request to MCP server failed")
        );
        server.task.await.unwrap();

        let server = spawn_test_tls_server().await;
        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), &server.ca_pem).unwrap();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some(server.url),
            tls_ca_cert_path: Some(ca_file.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).unwrap();
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let response = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .unwrap();
        assert_eq!(response.id, Some(serde_json::Value::from(1)));
        assert_eq!(response.result, Some(serde_json::json!({"ok": true})));
        server.task.await.unwrap();
    }

    #[tokio::test]
    async fn private_ca_sse_fails_unset_and_succeeds_with_matching_ca() {
        let request = JsonRpcRequest::new(1, "initialize", serde_json::json!({}));

        let server = spawn_test_tls_server().await;
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Sse,
            url: Some(server.url.replace("/mcp", "/sse")),
            ..Default::default()
        };
        let transport = SseTransport::new(&config).unwrap();
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let error = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .expect_err("a private CA must remain untrusted when the field is unset");
        assert!(error.to_string().contains("SSE GET to MCP server failed"));
        server.task.await.unwrap();

        let server = spawn_test_tls_server_with_behavior("127.0.0.1", TestTlsBehavior::Sse).await;
        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), &server.ca_pem).unwrap();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Sse,
            url: Some(server.url.replace("/mcp", "/sse")),
            tls_ca_cert_path: Some(ca_file.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let transport = SseTransport::new(&config).unwrap();
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let response = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .unwrap();
        assert_eq!(response.id, Some(serde_json::Value::from(1)));
        assert_eq!(response.result, Some(serde_json::json!({"ok": true})));
        transport.close().await.unwrap();
        server.task.await.unwrap();
    }

    #[tokio::test]
    async fn custom_ca_redirect_to_plaintext_sends_nothing_to_destination() {
        let destination = wiremock::MockServer::start().await;
        let redirect_target = format!("{}/capture", destination.uri());
        let server = spawn_test_tls_server_with_behavior(
            "127.0.0.1",
            TestTlsBehavior::Redirect(redirect_target),
        )
        .await;
        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), &server.ca_pem).unwrap();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some(server.url),
            headers: std::collections::HashMap::from([(
                "Authorization".into(),
                "Bearer synthetic-test-token".into(),
            )]),
            tls_ca_cert_path: Some(ca_file.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).unwrap();
        let request = JsonRpcRequest::new(
            1,
            "initialize",
            serde_json::json!({"synthetic": "request-body"}),
        );

        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let error = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .expect_err("HTTPS-to-HTTP redirect must be rejected");
        assert!(
            error
                .to_string()
                .contains("HTTP request to MCP server failed")
        );
        server.task.await.unwrap();
        assert!(
            destination
                .received_requests()
                .await
                .expect("destination request log")
                .is_empty(),
            "redirect policy must block headers and content before the plaintext destination"
        );
    }

    #[tokio::test]
    async fn custom_ca_does_not_trust_unrelated_ca_or_wrong_hostname() {
        let request = JsonRpcRequest::new(1, "initialize", serde_json::json!({}));

        let server = spawn_test_tls_server().await;
        let wrong_ca_file = tempfile::NamedTempFile::new().unwrap();
        let wrong_ca_key = rcgen::KeyPair::generate().unwrap();
        let mut wrong_ca_params =
            rcgen::CertificateParams::new(vec!["unrelated test CA".into()]).unwrap();
        wrong_ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let wrong_ca = wrong_ca_params.self_signed(&wrong_ca_key).unwrap();
        std::fs::write(wrong_ca_file.path(), wrong_ca.pem()).unwrap();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some(server.url),
            tls_ca_cert_path: Some(wrong_ca_file.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).unwrap();
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let error = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .expect_err("an unrelated CA must not authenticate the server");
        assert!(
            error
                .to_string()
                .contains("HTTP request to MCP server failed")
        );
        server.task.await.unwrap();

        let server = spawn_test_tls_server_with_san("wrong.example").await;
        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), &server.ca_pem).unwrap();
        let config = McpServerConfig {
            name: "internal".into(),
            transport: McpTransport::Http,
            url: Some(server.url),
            tls_ca_cert_path: Some(ca_file.path().to_string_lossy().into_owned()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).unwrap();
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let error = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .expect_err("a trusted CA must not bypass hostname verification");
        assert!(
            error
                .to_string()
                .contains("HTTP request to MCP server failed")
        );
        server.task.await.unwrap();
    }

    #[test]
    fn http_request_timeout_defaults_non_tool_requests_to_legacy_value() {
        let request = JsonRpcRequest::new(1, "initialize", serde_json::json!({}));
        assert_eq!(
            http_request_timeout_secs(&request, None),
            Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS)
        );
    }

    #[test]
    fn http_request_timeout_does_not_shorten_non_tool_requests_from_tool_config() {
        let request = JsonRpcRequest::new(1, "tools/list", serde_json::json!({}));
        assert_eq!(
            http_request_timeout_secs(&request, Some(5)),
            Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS)
        );
    }

    #[test]
    fn http_request_timeout_honors_configured_tool_call_timeout_above_legacy_value() {
        let request = JsonRpcRequest::new(1, TOOLS_CALL_METHOD, serde_json::json!({}));
        assert_eq!(
            http_request_timeout_secs(&request, Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS + 60)),
            Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS + 60)
        );
    }

    #[test]
    fn http_request_timeout_leaves_default_tool_call_budget_to_client_wrapper() {
        let request = JsonRpcRequest::new(1, TOOLS_CALL_METHOD, serde_json::json!({}));
        assert_eq!(http_request_timeout_secs(&request, None), None);
    }

    #[test]
    fn http_sse_read_timeout_defaults_non_tool_requests_to_recv_timeout() {
        let request = JsonRpcRequest::new(1, "initialize", serde_json::json!({}));
        assert_eq!(
            http_sse_read_timeout_secs(&request, None),
            Some(RECV_TIMEOUT_SECS)
        );
    }

    #[test]
    fn http_sse_read_timeout_honors_configured_tool_call_timeout() {
        let request = JsonRpcRequest::new(1, TOOLS_CALL_METHOD, serde_json::json!({}));
        assert_eq!(
            http_sse_read_timeout_secs(&request, Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS + 60)),
            Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS + 60)
        );
    }

    #[test]
    fn http_sse_read_timeout_leaves_default_tool_call_budget_to_client_wrapper() {
        let request = JsonRpcRequest::new(1, TOOLS_CALL_METHOD, serde_json::json!({}));
        assert_eq!(http_sse_read_timeout_secs(&request, None), None);
    }

    #[test]
    fn http_transport_stores_configured_tool_timeout() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("http://localhost/mcp".into()),
            tool_timeout_secs: Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS + 60),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).expect("build transport");
        assert_eq!(
            transport.tool_timeout_secs,
            Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS + 60)
        );
    }

    #[test]
    fn sse_transport_stores_configured_tool_timeout() {
        let config = McpServerConfig {
            name: "test-sse".into(),
            transport: McpTransport::Sse,
            url: Some("http://localhost/sse".into()),
            tool_timeout_secs: Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS + 60),
            ..Default::default()
        };
        let transport = SseTransport::new(&config).expect("build transport");
        assert_eq!(
            transport.tool_timeout_secs,
            Some(DEFAULT_HTTP_REQUEST_TIMEOUT_SECS + 60)
        );
    }

    #[test]
    fn test_extract_json_from_sse_data_no_space() {
        let input = "data:{\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_extract_json_from_sse_with_event_and_id() {
        let input = "id: 1\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_extract_json_from_sse_multiline_data() {
        let input = "event: message\ndata: {\ndata:   \"jsonrpc\": \"2.0\",\ndata:   \"result\": {}\ndata: }\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_extract_json_from_sse_skips_bom_and_leading_whitespace() {
        let input = "\u{feff}\n\n  data: {\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_extract_json_from_sse_uses_last_event_with_data() {
        let input =
            ": keep-alive\n\nid: 1\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_parse_jsonrpc_response_text_handles_plain_json() {
        let parsed = parse_jsonrpc_response_text("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}")
            .expect("plain JSON response should parse");
        assert_eq!(parsed.id, Some(serde_json::json!(1)));
        assert!(parsed.error.is_none());
    }

    #[test]
    fn test_parse_jsonrpc_response_text_handles_sse_framed_json() {
        let sse =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
        let parsed =
            parse_jsonrpc_response_text(sse).expect("SSE-framed JSON response should parse");
        assert_eq!(parsed.id, Some(serde_json::json!(2)));
        assert_eq!(
            parsed
                .result
                .as_ref()
                .and_then(|v| v.get("ok"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_parse_jsonrpc_response_text_rejects_empty_payload() {
        assert!(parse_jsonrpc_response_text(" \n\t ").is_err());
    }

    #[test]
    fn http_transport_updates_session_id_from_response_headers() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("http://localhost/mcp".into()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).expect("build transport");

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static("mcp-session-id"),
            reqwest::header::HeaderValue::from_static("session-abc"),
        );
        transport.update_session_id_from_headers(&headers);
        assert_eq!(transport.session_id.lock().as_deref(), Some("session-abc"));
    }

    #[test]
    fn http_transport_injects_session_id_header_when_available() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("http://localhost/mcp".into()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).expect("build transport");
        *transport.session_id.lock() = Some("session-xyz".to_string());

        let req = transport
            .apply_session_header(reqwest::Client::new().post("http://localhost/mcp"))
            .build()
            .expect("build request");
        assert_eq!(
            req.headers()
                .get(MCP_SESSION_ID_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some("session-xyz")
        );
    }

    // ── derive_message_url tests ──────────────────────────────────────────────

    #[test]
    fn derive_message_url_replaces_sse_segment_with_messages() {
        let url = derive_message_url("http://localhost:3000/mcp/sse", "messages");
        assert_eq!(url, Some("http://localhost:3000/mcp/messages".to_string()));
    }

    #[test]
    fn derive_message_url_appends_when_no_sse_segment() {
        let url = derive_message_url("http://localhost:3000/mcp", "messages");
        assert_eq!(url, Some("http://localhost:3000/mcp/messages".to_string()));
    }

    #[test]
    fn derive_message_url_returns_none_for_invalid_url() {
        let url = derive_message_url("not-a-url", "messages");
        assert!(url.is_none());
    }

    #[test]
    fn derive_message_url_message_path_variant() {
        let url = derive_message_url("http://localhost:3000/mcp/sse", "message");
        assert_eq!(url, Some("http://localhost:3000/mcp/message".to_string()));
    }

    // ── parse_endpoint_from_data tests ───────────────────────────────────────

    #[test]
    fn parse_endpoint_absolute_http_url_returned_as_is() {
        let result = parse_endpoint_from_data("http://base/sse", "http://other/messages");
        assert_eq!(result, Some("http://other/messages".to_string()));
    }

    #[test]
    fn parse_endpoint_absolute_https_url_returned_as_is() {
        let result = parse_endpoint_from_data("https://base/sse", "https://other/messages");
        assert_eq!(result, Some("https://other/messages".to_string()));
    }

    #[test]
    fn parse_endpoint_relative_path_resolved_against_base() {
        let result = parse_endpoint_from_data("http://localhost:3000/sse", "/messages");
        assert_eq!(result, Some("http://localhost:3000/messages".to_string()));
    }

    #[test]
    fn parse_endpoint_json_object_with_endpoint_key() {
        let json_data = r#"{"endpoint":"/messages"}"#;
        let result = parse_endpoint_from_data("http://localhost:3000/sse", json_data);
        assert_eq!(result, Some("http://localhost:3000/messages".to_string()));
    }

    // ── looks_like_sse_text tests ─────────────────────────────────────────────

    #[test]
    fn looks_like_sse_text_detects_data_prefix() {
        assert!(looks_like_sse_text("data:{\"jsonrpc\":\"2.0\"}"));
    }

    #[test]
    fn looks_like_sse_text_detects_event_prefix() {
        assert!(looks_like_sse_text("event: message\ndata: {}"));
    }

    #[test]
    fn looks_like_sse_text_detects_embedded_data_line() {
        assert!(looks_like_sse_text("id: 1\ndata:{\"x\":1}"));
    }

    #[test]
    fn looks_like_sse_text_plain_json_is_not_sse() {
        assert!(!looks_like_sse_text(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"
        ));
    }

    // ── extract_json_from_sse_text edge cases ─────────────────────────────────

    #[test]
    fn extract_json_skips_comment_lines() {
        let input = ": keep-alive\ndata: {\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let v: serde_json::Value = serde_json::from_str(extracted.as_ref()).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
    }

    #[test]
    fn extract_json_empty_input_returns_empty_trimmed() {
        let result = extract_json_from_sse_text("   ");
        assert!(result.as_ref().trim().is_empty());
    }

    #[test]
    fn extract_json_plain_json_returned_unchanged() {
        let input = "{\"jsonrpc\":\"2.0\",\"result\":{}}";
        let extracted = extract_json_from_sse_text(input);
        // No SSE framing, extracted as-is (trimmed)
        assert_eq!(extracted.as_ref(), input);
    }

    // ── parse_jsonrpc_response_text edge cases ────────────────────────────────

    #[test]
    fn parse_jsonrpc_response_rejects_whitespace_only() {
        assert!(parse_jsonrpc_response_text("   \n\t  ").is_err());
    }

    #[test]
    fn parse_jsonrpc_response_with_error_result() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"not found"}}"#;
        let resp = parse_jsonrpc_response_text(json).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    // ── create_transport factory ──────────────────────────────────────────────

    #[test]
    fn create_transport_stdio_fails_without_valid_command() {
        // Spawning a non-existent binary should fail
        let config = McpServerConfig {
            name: "test-stdio".into(),
            transport: McpTransport::Stdio,
            command: "/usr/bin/zeroclaw_nonexistent_binary_abc123".into(),
            ..Default::default()
        };
        let result = create_transport(&config);
        assert!(result.is_err());
    }

    #[test]
    fn create_transport_http_without_url_fails() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            ..Default::default()
        };
        assert!(create_transport(&config).is_err());
    }

    #[test]
    fn create_transport_sse_without_url_fails() {
        let config = McpServerConfig {
            name: "test-sse".into(),
            transport: McpTransport::Sse,
            ..Default::default()
        };
        assert!(create_transport(&config).is_err());
    }

    #[test]
    fn create_transport_http_with_url_succeeds() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("http://localhost:9999/mcp".into()),
            ..Default::default()
        };
        // Build should succeed even if server isn't running
        assert!(create_transport(&config).is_ok());
    }

    #[test]
    fn create_transport_sse_with_url_succeeds() {
        let config = McpServerConfig {
            name: "test-sse".into(),
            transport: McpTransport::Sse,
            url: Some("http://localhost:9999/sse".into()),
            ..Default::default()
        };
        assert!(create_transport(&config).is_ok());
    }

    // ── HTTP session id whitespace handling ───────────────────────────────────

    #[test]
    fn http_transport_ignores_empty_session_id_header() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("http://localhost/mcp".into()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).expect("build transport");
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static("mcp-session-id"),
            reqwest::header::HeaderValue::from_static("   "),
        );
        transport.update_session_id_from_headers(&headers);
        // Whitespace-only session id should not be stored
        assert!(transport.session_id.lock().is_none());
    }

    #[test]
    fn http_transport_no_session_header_leaves_none() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("http://localhost/mcp".into()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).expect("build transport");
        assert!(transport.session_id.lock().is_none());
    }

    #[test]
    fn http_transport_apply_session_header_noop_when_no_session() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("http://localhost/mcp".into()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).expect("build transport");
        let req = transport
            .apply_session_header(reqwest::Client::new().post("http://localhost/mcp"))
            .build()
            .expect("build request");
        assert!(req.headers().get(MCP_SESSION_ID_HEADER).is_none());
    }

    #[tokio::test]
    async fn http_transport_reset_clears_session_id() {
        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some("http://localhost/mcp".into()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).expect("build transport");
        *transport.session_id.lock() = Some("stale-session".into());
        SharedMcpTransportConn::reset(&transport)
            .await
            .expect("reset");
        assert!(transport.session_id.lock().is_none());
    }

    #[tokio::test]
    async fn http_transport_maps_404_to_stale_session() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some(server.uri()),
            ..Default::default()
        };
        let transport = HttpTransport::new(&config).expect("build transport");
        // A 404 only signals a stale session when the request carried a session id.
        *transport.session_id.lock() = Some("sess-1".into());
        let req = JsonRpcRequest::new(1, "tools/call", serde_json::json!({}));
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let err = transport
            .send_and_recv(&req, &lifecycle)
            .await
            .expect_err("404 should error");
        match err.downcast_ref::<McpTransportError>() {
            Some(McpTransportError::StaleSession { status }) => assert_eq!(*status, 404),
            other => panic!("expected StaleSession, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_transport_404_without_session_is_plain_error() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let config = McpServerConfig {
            name: "test-http".into(),
            transport: McpTransport::Http,
            url: Some(server.uri()),
            ..Default::default()
        };
        // No session id was ever issued (stateless server, or a misconfigured url):
        // a 404 here is a missing endpoint, not a stale session — it must NOT map to
        // StaleSession (which would make `call_tool` burn a wasted reconnect).
        let transport = HttpTransport::new(&config).expect("build transport");
        assert!(transport.session_id.lock().is_none());
        let req = JsonRpcRequest::new(1, "tools/call", serde_json::json!({}));
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let err = transport
            .send_and_recv(&req, &lifecycle)
            .await
            .expect_err("404 should error");
        assert!(
            !matches!(
                err.downcast_ref::<McpTransportError>(),
                Some(McpTransportError::StaleSession { .. })
            ),
            "sessionless 404 must not be classified as StaleSession, got: {err:?}"
        );
        assert!(
            err.to_string().contains("MCP server returned HTTP 404"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn sse_post_404_is_not_replayed_to_derived_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/sse"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/sse"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let config = McpServerConfig {
            name: "sse-no-replay".into(),
            transport: McpTransport::Sse,
            url: Some(format!("{}/sse", server.uri())),
            ..Default::default()
        };
        let transport = SseTransport::new(&config).expect("build transport");
        let request = JsonRpcRequest::new(7, "tools/call", serde_json::json!({}));
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let error = SharedMcpTransportConn::send_and_recv(&transport, &request, &lifecycle)
            .await
            .expect_err("404 after a write must surface");
        assert!(matches!(
            error.downcast_ref::<McpTransportError>(),
            Some(McpTransportError::StaleSession { status: 404 })
        ));
        assert_eq!(lifecycle.outcome_unknown_epoch(), Some(0));

        let requests = server
            .received_requests()
            .await
            .expect("request recording enabled");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method.as_str() == "POST" && request.url.path() == "/sse")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method.as_str() == "POST"
                    && request.url.path() == "/messages")
                .count(),
            0,
            "a post-write 404 does not prove the first endpoint skipped execution"
        );
    }

    #[tokio::test]
    async fn cancelled_direct_sse_request_removes_pending_waiter() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        let config = McpServerConfig {
            name: "sse-cancel-direct".into(),
            transport: McpTransport::Sse,
            url: Some(format!("{}/sse", server.uri())),
            ..Default::default()
        };
        let transport = Arc::new(SseTransport::new(&config).expect("build transport"));
        let reader = zeroclaw_spawn::spawn!(std::future::pending::<()>());
        {
            let mut conn = transport.conn.lock().await;
            conn.stream_state = SseStreamState::Connected;
            conn.reader_task = Some(reader);
        }
        {
            let mut shared = transport.shared.lock().await;
            shared.message_url = Some(format!("{}/messages", server.uri()));
            shared.message_url_from_endpoint = true;
        }

        let task_transport = Arc::clone(&transport);
        let request = JsonRpcRequest::new(7, "tools/call", serde_json::json!({}));
        let lifecycle = McpRequestLifecycle::uncoordinated(0);
        let call = zeroclaw_spawn::spawn!(async move {
            SharedMcpTransportConn::send_and_recv(task_transport.as_ref(), &request, &lifecycle)
                .await
        });
        timeout(Duration::from_secs(2), async {
            while transport.pending.lock().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("request did not register a waiter");

        call.abort();
        assert!(
            call.await
                .expect_err("call must be cancelled")
                .is_cancelled()
        );
        assert!(transport.pending.lock().is_empty());
        SharedMcpTransportConn::close(transport.as_ref())
            .await
            .expect("close transport");
    }

    #[tokio::test]
    async fn sse_transport_reset_clears_session_and_endpoint_state() {
        let config = McpServerConfig {
            name: "test-sse".into(),
            transport: McpTransport::Sse,
            url: Some("http://localhost:1/sse".into()),
            ..Default::default()
        };
        let transport = SseTransport::new(&config).expect("build transport");
        transport.conn.lock().await.stream_state = SseStreamState::Connected;
        {
            let mut guard = transport.shared.lock().await;
            guard.message_url = Some("http://localhost:1/messages".into());
            guard.message_url_from_endpoint = true;
        }
        let (tx, _rx) = oneshot::channel();
        transport.pending.lock().insert(7, tx);

        SharedMcpTransportConn::reset(&transport)
            .await
            .expect("reset");

        assert_eq!(
            transport.conn.lock().await.stream_state,
            SseStreamState::Unknown
        );
        let guard = transport.shared.lock().await;
        assert!(guard.message_url.is_none());
        assert!(!guard.message_url_from_endpoint);
        drop(guard);
        assert!(transport.pending.lock().is_empty());
    }

    #[tokio::test]
    async fn sse_transport_close_clears_reader_endpoint_and_pending_state() {
        let config = McpServerConfig {
            name: "test-sse-close".into(),
            transport: McpTransport::Sse,
            url: Some("http://localhost:1/sse".into()),
            ..Default::default()
        };
        let transport = SseTransport::new(&config).expect("build transport");
        let reader = zeroclaw_spawn::spawn!(std::future::pending::<()>());
        {
            let mut conn = transport.conn.lock().await;
            conn.stream_state = SseStreamState::Connected;
            conn.reader_task = Some(reader);
        }
        {
            let mut shared = transport.shared.lock().await;
            shared.message_url = Some("http://localhost:1/messages".into());
            shared.message_url_from_endpoint = true;
        }
        let (tx, rx) = oneshot::channel();
        transport.pending.lock().insert(7, tx);

        SharedMcpTransportConn::close(&transport)
            .await
            .expect("close");

        let conn = transport.conn.lock().await;
        assert_eq!(conn.stream_state, SseStreamState::Unknown);
        assert!(conn.reader_task.is_none());
        drop(conn);
        let shared = transport.shared.lock().await;
        assert!(shared.message_url.is_none());
        assert!(!shared.message_url_from_endpoint);
        drop(shared);
        assert!(transport.pending.lock().is_empty());
        assert!(rx.await.is_err(), "pending receiver must be released");
    }
}
