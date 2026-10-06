//! The client: dial, handshake, multiplex, and observe disconnects.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::{AbortHandle, JoinHandle};
use zeroclaw_api::jsonrpc::{
    JSONRPC_VERSION, JsonRpcError, JsonRpcFrame, JsonRpcResponse, RpcOutbound,
};
use zeroclaw_rpc_proto::types::{InitializeParams, InitializeResult};
use zeroclaw_rpc_proto::{Method, RPC_PROTOCOL_VERSION};

use crate::verify::{EndpointOwner, EndpointRejection, verify_local_endpoint};

/// How long the `initialize` round trip may take before the dial fails.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Default ceiling for one request when the caller names none.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Outbound frames queued ahead of the writer. Matches the daemon's own
/// per-connection writer bound so neither side buffers unboundedly.
const WRITER_QUEUE_DEPTH: usize = 64;
/// Notifications retained for a slow subscriber before it observes a lag.
const NOTIFICATION_CAPACITY: usize = 256;
/// Largest frame accepted from the daemon. Mirrors the daemon's inbound cap.
const MAX_FRAME_BYTES: u64 = 8 * 1024 * 1024;
/// Server-initiated requests held for the application. Beyond this the
/// client answers each new one with [`INBOUND_REQUEST_REJECTED`] instead of
/// holding it, so a peer that floods requests nobody consumes cannot grow
/// the client's memory.
pub const INBOUND_REQUEST_QUEUE_DEPTH: usize = 32;
/// Aggregate size of the held server-initiated requests, measured as the
/// frames they arrived in. Bounds the lane even when every frame is under
/// the 8 MiB per-frame cap.
pub const INBOUND_REQUEST_BYTE_BUDGET: usize = 16 * 1024 * 1024;
/// The error code this client returns to the daemon for a server-initiated
/// request it will not hold: the queue or byte budget is full, or the
/// application has dropped its inbound receiver. Outside the daemon's own
/// code space so a caller can tell the two apart.
pub const INBOUND_REQUEST_REJECTED: i32 = -32050;

/// Why a dial or a request failed.
#[derive(Debug)]
pub enum ClientError {
    /// The transport failed while dialing or writing.
    Io(std::io::Error),
    /// The local endpoint could not be proven to belong to the expected
    /// account, so the dial ended before the credential was written.
    UntrustedEndpoint {
        endpoint: PathBuf,
        rejection: EndpointRejection,
    },
    /// The daemon answered `initialize` with something this client cannot use.
    Handshake(String),
    /// The daemon returned a JSON-RPC error.
    Rpc(JsonRpcError),
    /// No response arrived within the caller's ceiling.
    Timeout { method: String, after: Duration },
    /// The connection ended before the response arrived.
    Disconnected(String),
    /// The result did not decode into the caller's type.
    Decode {
        method: String,
        source: serde_json::Error,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "transport error: {e}"),
            Self::UntrustedEndpoint {
                endpoint,
                rejection,
            } => write!(
                f,
                "refusing to send a credential to core endpoint {}: {rejection}",
                endpoint.display()
            ),
            Self::Handshake(msg) => write!(f, "initialize failed: {msg}"),
            Self::Rpc(e) => write!(f, "daemon returned error {}: {}", e.code, e.message),
            Self::Timeout { method, after } => {
                write!(f, "{method}: no response after {}s", after.as_secs_f64())
            }
            Self::Disconnected(reason) => write!(f, "disconnected: {reason}"),
            Self::Decode { method, source } => write!(f, "{method}: undecodable result: {source}"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Decode { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Whether the connection is still usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Disconnected(String),
}

/// A server-to-client notification (a request without an `id`).
#[derive(Debug, Clone)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// A server-initiated request that expects a response, such as
/// `elicitation/create`. Answer it with [`RpcClient::respond`], echoing `id`.
/// Dropping it releases its share of [`INBOUND_REQUEST_BYTE_BUDGET`].
#[derive(Debug)]
pub struct InboundRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
    _held: HeldBytes,
}

/// The frame bytes one held request charges to the inbound budget, refunded
/// when the application drops the request.
#[derive(Debug)]
struct HeldBytes {
    budget: Arc<AtomicUsize>,
    bytes: usize,
}

impl Drop for HeldBytes {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// Where server-initiated requests go: a bounded queue, a byte budget, and a
/// non-blocking path back to the writer for the refusals.
struct InboundLane {
    queue: mpsc::Sender<InboundRequest>,
    budget: Arc<AtomicUsize>,
    rejected: Arc<AtomicUsize>,
    reject_tx: mpsc::Sender<String>,
}

impl InboundLane {
    /// Answer a request this client will not hold. Never waits: when the
    /// writer queue is also full the peer is flooding both directions, and
    /// the refusal is dropped rather than stalling response dispatch.
    fn reject(&self, id: Value, reason: &str) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        let response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result: None,
            error: Some(JsonRpcError {
                code: INBOUND_REQUEST_REJECTED,
                message: format!("client will not hold this request: {reason}"),
                data: None,
            }),
            id,
        };
        if let Ok(body) = serde_json::to_string(&response) {
            let _ = self.reject_tx.try_send(body);
        }
    }

    fn offer(&self, id: Value, method: String, params: Value, frame_bytes: usize) {
        let held = self.budget.fetch_add(frame_bytes, Ordering::AcqRel) + frame_bytes;
        if held > INBOUND_REQUEST_BYTE_BUDGET {
            self.budget.fetch_sub(frame_bytes, Ordering::AcqRel);
            self.reject(id, "inbound byte budget exhausted");
            return;
        }
        let request = InboundRequest {
            id: id.clone(),
            method,
            params,
            _held: HeldBytes {
                budget: Arc::clone(&self.budget),
                bytes: frame_bytes,
            },
        };
        match self.queue.try_send(request) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(request)) => {
                drop(request);
                self.reject(id, "inbound request queue full");
            }
            Err(mpsc::error::TrySendError::Closed(request)) => {
                drop(request);
                self.reject(id, "no inbound request consumer");
            }
        }
    }
}

/// Aborts a transport task unless ownership passes to the client. The tasks
/// are spawned before the handshake completes, so a dial cancelled mid-way
/// (an outer timeout, a dropped future) must not detach them.
struct AbortOnDrop(Option<JoinHandle<()>>);

impl AbortOnDrop {
    fn release(mut self) -> JoinHandle<()> {
        self.0.take().expect("transport task released once")
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// What the client presents in `initialize`.
///
/// The protocol version is always this crate's [`RPC_PROTOCOL_VERSION`];
/// callers choose only the credential and the client-side facts.
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// Explicit credential: a pairing token or an OIDC access token.
    pub auth_token: Option<String>,
    /// Which configured provider verifies `auth_token` (`native` when unset).
    pub auth_provider: Option<String>,
    /// TUI identity from a previous connection, to reclaim it. The
    /// signature is replayable, so presenting it counts as presenting a
    /// credential.
    pub tui_id: Option<String>,
    pub tui_sig: Option<String>,
    /// Shell environment forwarded to daemon-spawned subprocesses. It can
    /// hold API keys, so a non-empty map counts as a credential.
    pub env: HashMap<String, String>,
    /// Advertised client capabilities (for example the ACP elicitation form).
    pub client_capabilities: Option<Value>,
    /// Ceiling for the handshake; [`DEFAULT_HANDSHAKE_TIMEOUT`] when unset.
    pub handshake_timeout: Option<Duration>,
    /// The account that must serve the endpoint before
    /// [`RpcClient::connect_local`] sends it a credential (a token, a TUI
    /// signature, or forwarded environment), checked on every dial.
    pub endpoint_owner: EndpointOwner,
}

impl ConnectOptions {
    /// The `initialize` params these options produce.
    pub fn initialize_params(&self) -> InitializeParams {
        InitializeParams {
            protocol_version: RPC_PROTOCOL_VERSION,
            tui_id: self.tui_id.clone(),
            tui_sig: self.tui_sig.clone(),
            env: self.env.clone(),
            client_capabilities: self.client_capabilities.clone(),
            auth_token: self.auth_token.clone(),
            auth_provider: self.auth_provider.clone(),
        }
    }

    /// Whether `initialize` would carry anything a stranger could reuse: a
    /// bearer, a TUI signature, or forwarded environment.
    fn carries_credential(&self) -> bool {
        self.auth_token.is_some() || self.tui_sig.is_some() || !self.env.is_empty()
    }
}

/// One authenticated connection to the daemon.
///
/// Dropping the client aborts its reader and writer tasks; the daemon sees
/// EOF and releases the connection.
pub struct RpcClient {
    rpc: Arc<RpcOutbound>,
    notifications: broadcast::Sender<Notification>,
    inbound: Mutex<Option<mpsc::Receiver<InboundRequest>>>,
    rejected_inbound: Arc<AtomicUsize>,
    state: Arc<Mutex<ConnectionState>>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    handshake: InitializeResult,
}

impl fmt::Debug for RpcClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcClient")
            .field("state", &self.state())
            .field("server_version", &self.handshake.server_version)
            .field("principal_id", &self.handshake.principal_id)
            .finish_non_exhaustive()
    }
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

fn set_disconnected(state: &Mutex<ConnectionState>, reason: String) {
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    if *guard == ConnectionState::Connected {
        *guard = ConnectionState::Disconnected(reason);
    }
}

/// Read one NDJSON frame, refusing frames above the daemon's own cap.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Option<String> {
    let mut buf: Vec<u8> = Vec::new();
    let mut limited = (&mut *reader).take(MAX_FRAME_BYTES + 1);
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

/// Route one validated frame: responses wake their pending request,
/// requests with an id go to the inbound queue, the rest are notifications.
fn route_frame(
    rpc: &RpcOutbound,
    notifications: &broadcast::Sender<Notification>,
    inbound: &InboundLane,
    frame: JsonRpcFrame,
    frame_bytes: usize,
) {
    match frame {
        JsonRpcFrame::Response { id, result } => {
            if let Some(id) = id.as_str() {
                rpc.dispatch_validated_response(id, result);
            }
        }
        JsonRpcFrame::Request(request) => match request.id {
            Some(id) if !id.is_null() => {
                inbound.offer(id, request.method, request.params, frame_bytes);
            }
            _ => {
                let _ = notifications.send(Notification {
                    method: request.method,
                    params: request.params,
                });
            }
        },
    }
}

impl RpcClient {
    /// Dial the daemon's local endpoint and complete the handshake.
    ///
    /// When `options` carries a credential (an `auth_token`, a `tui_sig`, or
    /// a non-empty `env`), the endpoint must first prove, through the
    /// kernel, that `options.endpoint_owner` serves it; otherwise the stream
    /// is closed with nothing written and the dial fails with
    /// [`ClientError::UntrustedEndpoint`]. Every call checks again, so a
    /// reconnect loop re-verifies each new dial. A dial that presents none
    /// of these is not gated: the daemon authenticates it by the peer
    /// credential it reads on its own side, which is how a `[users]` roster
    /// member reaches a daemon running as another account.
    ///
    /// This is the only way to reach an operating-system endpoint: the
    /// handshake over an arbitrary socket or pipe is not public.
    pub async fn connect_local(path: &Path, options: ConnectOptions) -> Result<Self, ClientError> {
        let stream = open_local_stream(path).await?;
        if options.carries_credential() {
            verify_local_endpoint(&stream, path, options.endpoint_owner)
                .await
                .map_err(|rejection| ClientError::UntrustedEndpoint {
                    endpoint: path.to_path_buf(),
                    rejection,
                })?;
        }
        Self::initialize_over(stream, options).await
    }

    /// Check that the daemon's local endpoint at `path` accepts a connection
    /// and that `owner` serves it, then close it without writing a byte.
    ///
    /// For health checks. It runs the same kernel checks a credential-bearing
    /// [`RpcClient::connect_local`] runs, whatever the caller would send, and
    /// sends no `initialize`: a probe opens no session and presents nothing.
    pub async fn probe_local(path: &Path, owner: EndpointOwner) -> Result<(), ClientError> {
        let stream = open_local_stream(path).await?;
        verify_local_endpoint(&stream, path, owner)
            .await
            .map_err(|rejection| ClientError::UntrustedEndpoint {
                endpoint: path.to_path_buf(),
                rejection,
            })
    }

    /// Run the handshake over the daemon's in-process duplex, the transport
    /// its in-process connector hands out.
    ///
    /// Both ends of the duplex live in this process, so there is no other
    /// account to verify. The parameter is the concrete duplex type on
    /// purpose: an operating-system socket or pipe does not fit, and has to
    /// go through [`RpcClient::connect_local`], which verifies the endpoint
    /// before a credential leaves the client.
    ///
    /// ```compile_fail
    /// # async fn dial(socket: tokio::net::UnixStream) {
    /// // A socket is not the in-process duplex, so this does not compile.
    /// let _ = zeroclaw_rpc_client::RpcClient::connect_over(socket, Default::default()).await;
    /// # }
    /// ```
    pub async fn connect_over(
        stream: DuplexStream,
        options: ConnectOptions,
    ) -> Result<Self, ClientError> {
        Self::initialize_over(stream, options).await
    }

    /// The handshake itself, over a stream the caller has already vouched
    /// for. Private: the two public constructors above are what vouch.
    async fn initialize_over<S>(stream: S, options: ConnectOptions) -> Result<Self, ClientError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(WRITER_QUEUE_DEPTH);
        let reject_tx = writer_tx.clone();
        let rpc = Arc::new(RpcOutbound::new(writer_tx));
        let state = Arc::new(Mutex::new(ConnectionState::Connected));
        let (notifications, _) = broadcast::channel::<Notification>(NOTIFICATION_CAPACITY);
        let (inbound_tx, inbound_rx) = mpsc::channel::<InboundRequest>(INBOUND_REQUEST_QUEUE_DEPTH);
        let rejected_inbound = Arc::new(AtomicUsize::new(0));
        let inbound_lane = InboundLane {
            queue: inbound_tx,
            budget: Arc::new(AtomicUsize::new(0)),
            rejected: Arc::clone(&rejected_inbound),
            reject_tx,
        };

        let writer_state = Arc::clone(&state);
        let writer = tokio::spawn(async move {
            while let Some(mut line) = writer_rx.recv().await {
                if !line.ends_with('\n') {
                    line.push('\n');
                }
                if let Err(e) = write_half.write_all(line.as_bytes()).await {
                    set_disconnected(&writer_state, e.to_string());
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });
        let writer_abort: AbortHandle = writer.abort_handle();
        let writer = AbortOnDrop(Some(writer));

        let reader_rpc = Arc::clone(&rpc);
        let reader_state = Arc::clone(&state);
        let reader_notifications = notifications.clone();
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            while let Some(line) = read_frame(&mut reader).await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                let Ok(frame) = JsonRpcFrame::from_value(value) else {
                    continue;
                };
                route_frame(
                    &reader_rpc,
                    &reader_notifications,
                    &inbound_lane,
                    frame,
                    trimmed.len(),
                );
            }
            set_disconnected(&reader_state, "daemon closed the connection".to_string());
            // Stopping the writer drops the queue receiver, which resolves
            // `RpcOutbound::closed()` and fails every request still waiting.
            writer_abort.abort();
        });
        let reader = AbortOnDrop(Some(reader));

        let handshake_timeout = options
            .handshake_timeout
            .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT);
        let params = match serde_json::to_value(options.initialize_params()) {
            Ok(params) => params,
            Err(e) => {
                return Err(ClientError::Handshake(format!(
                    "encoding initialize params: {e}"
                )));
            }
        };
        // A daemon that drops the connection mid-handshake must fail the dial
        // now, not after the ceiling: race the request against transport
        // closure the same way ordinary requests do. `biased` keeps a reply
        // that arrived just before EOF (an auth refusal, say) ahead of the
        // closure. Every early return drops the two guards, which aborts the
        // transport tasks; only a completed handshake hands them to the client.
        let response = tokio::time::timeout(handshake_timeout, async {
            tokio::select! {
                biased;
                result = rpc.request(Method::Initialize.wire_name(), params) => Some(result),
                () = rpc.closed() => None,
            }
        })
        .await;
        let handshake = match response {
            Ok(Some(Ok(value))) => match serde_json::from_value::<InitializeResult>(value) {
                Ok(result) => result,
                Err(e) => {
                    return Err(ClientError::Handshake(format!(
                        "undecodable initialize result: {e}"
                    )));
                }
            },
            Ok(Some(Err(error))) => return Err(ClientError::Rpc(error)),
            Ok(None) => {
                let reason = match state.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                    ConnectionState::Disconnected(reason) => reason,
                    ConnectionState::Connected => "connection closed".to_string(),
                };
                return Err(ClientError::Disconnected(reason));
            }
            Err(_) => {
                return Err(ClientError::Timeout {
                    method: Method::Initialize.wire_name().to_string(),
                    after: handshake_timeout,
                });
            }
        };

        Ok(Self {
            rpc,
            notifications,
            inbound: Mutex::new(Some(inbound_rx)),
            rejected_inbound,
            state,
            reader: reader.release(),
            writer: writer.release(),
            handshake,
        })
    }

    /// What the daemon returned from `initialize`.
    pub fn handshake(&self) -> &InitializeResult {
        &self.handshake
    }

    /// Whether the daemon advertised `method` in its capabilities.
    pub fn supports(&self, method: Method) -> bool {
        let wire = method.wire_name();
        self.handshake.capabilities.iter().any(|m| m == wire)
    }

    /// Current connection state.
    pub fn state(&self) -> ConnectionState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Resolve once the connection has ended for any reason.
    pub async fn closed(&self) {
        self.rpc.closed().await;
    }

    /// Send `method` with `params` and await its result, within
    /// [`DEFAULT_REQUEST_TIMEOUT`].
    pub async fn request(&self, method: Method, params: Value) -> Result<Value, ClientError> {
        self.request_with_timeout(method.wire_name(), params, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    /// Send a method by wire name. For methods this crate's [`Method`] table
    /// does not know yet.
    pub async fn request_raw(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        self.request_with_timeout(method, params, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    /// Send a request with an explicit ceiling. A dropped connection fails
    /// the request immediately instead of waiting out the timeout.
    pub async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ClientError> {
        let outcome = tokio::time::timeout(timeout, async {
            tokio::select! {
                biased;
                result = self.rpc.request(method, params) => Some(result),
                () = self.rpc.closed() => None,
            }
        })
        .await;
        match outcome {
            Ok(Some(Ok(value))) => Ok(value),
            Ok(Some(Err(error))) => Err(ClientError::Rpc(error)),
            Ok(None) => Err(ClientError::Disconnected(match self.state() {
                ConnectionState::Disconnected(reason) => reason,
                ConnectionState::Connected => "connection closed".to_string(),
            })),
            Err(_) => Err(ClientError::Timeout {
                method: method.to_string(),
                after: timeout,
            }),
        }
    }

    /// [`RpcClient::request`], decoding the result into `T`.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        params: Value,
    ) -> Result<T, ClientError> {
        let value = self.request(method, params).await?;
        serde_json::from_value(value).map_err(|source| ClientError::Decode {
            method: method.wire_name().to_string(),
            source,
        })
    }

    /// Subscribe to server notifications. A subscriber that falls more than
    /// the channel capacity behind observes a lag error rather than losing
    /// frames silently.
    pub fn notifications(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    /// Claim the single receiver for server-initiated requests. `None` once
    /// claimed. Up to [`INBOUND_REQUEST_QUEUE_DEPTH`] requests (within
    /// [`INBOUND_REQUEST_BYTE_BUDGET`]) that arrive before the claim are
    /// retained; the rest are answered with [`INBOUND_REQUEST_REJECTED`].
    pub fn take_inbound_requests(&self) -> Option<mpsc::Receiver<InboundRequest>> {
        self.inbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// How many server-initiated requests this connection refused to hold.
    pub fn rejected_inbound_requests(&self) -> usize {
        self.rejected_inbound.load(Ordering::Relaxed)
    }

    /// Answer a server-initiated request. Returns `false` when the
    /// connection is gone.
    pub async fn respond(&self, id: Value, result: Result<Value, JsonRpcError>) -> bool {
        let (result, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        let response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result,
            error,
            id,
        };
        match serde_json::to_string(&response) {
            Ok(body) => self.rpc.send_raw(body).await,
            Err(_) => false,
        }
    }

    /// End the connection now.
    pub fn shutdown(&self) {
        self.reader.abort();
        self.writer.abort();
        set_disconnected(&self.state, "shut down by the client".to_string());
    }
}

#[cfg(unix)]
async fn open_local_stream(path: &Path) -> Result<tokio::net::UnixStream, ClientError> {
    tokio::net::UnixStream::connect(path)
        .await
        .map_err(ClientError::Io)
}

#[cfg(windows)]
async fn open_local_stream(
    path: &Path,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, ClientError> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    let name = path.to_string_lossy().into_owned();
    // The daemon may not have a pending pipe instance yet; retry briefly.
    for _ in 0..50 {
        match ClientOptions::new().open(&name) {
            Ok(client) => return Ok(client),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(ClientError::Io(e)),
        }
    }
    Err(ClientError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "named pipe stayed busy",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::DuplexStream;

    /// A scripted daemon: answers `initialize`, then serves `status`, pushes
    /// one notification and one server-initiated request, and echoes the
    /// client's answer to that request back on a channel.
    fn fake_daemon(
        stream: DuplexStream,
        init_error: Option<JsonRpcError>,
    ) -> mpsc::UnboundedReceiver<Value> {
        let (seen_tx, seen_rx) = mpsc::unbounded_channel::<Value>();
        tokio::spawn(async move {
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut reader = BufReader::new(read_half);
            while let Some(line) = read_frame(&mut reader).await {
                let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                let id = frame.get("id").cloned().unwrap_or(Value::Null);
                match frame.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        let response = match &init_error {
                            Some(error) => json!({
                                "jsonrpc": "2.0", "id": id,
                                "error": {"code": error.code, "message": error.message},
                            }),
                            None => json!({
                                "jsonrpc": "2.0", "id": id,
                                "result": {
                                    "protocol_version": 1,
                                    "server_version": "test",
                                    "server_pid": 7,
                                    "capabilities": ["status"],
                                    "principal_id": "shared-operator",
                                    "commands": [],
                                },
                            }),
                        };
                        let _ = write_half
                            .write_all(format!("{response}\n").as_bytes())
                            .await;
                        if init_error.is_none() {
                            let ask = json!({
                                "jsonrpc": "2.0", "id": "srv-1",
                                "method": "elicitation/create", "params": {"q": "ok?"},
                            });
                            let _ = write_half.write_all(format!("{ask}\n").as_bytes()).await;
                        }
                    }
                    Some("status") => {
                        // A notification ahead of the response, so a subscriber
                        // that subscribed before calling `status` sees it.
                        let pushed = json!({
                            "jsonrpc": "2.0", "method": "logs/event", "params": {"n": 1},
                        });
                        let response = json!({
                            "jsonrpc": "2.0", "id": id, "result": {"active_sessions": 0},
                        });
                        let _ = write_half
                            .write_all(format!("{pushed}\n{response}\n").as_bytes())
                            .await;
                    }
                    // The daemon dies mid-request: end the task, which drops
                    // both halves and gives the client EOF.
                    Some("hang") => break,
                    Some(_) => {
                        let response = json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": -32601, "message": "Method not found"},
                        });
                        let _ = write_half
                            .write_all(format!("{response}\n").as_bytes())
                            .await;
                    }
                    None => {
                        // A response to the server-initiated request.
                        let _ = seen_tx.send(frame);
                    }
                }
            }
        });
        seen_rx
    }

    #[tokio::test]
    async fn handshake_and_request_round_trip() {
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        let _seen = fake_daemon(server_half, None);
        let client = RpcClient::connect_over(client_half, ConnectOptions::default())
            .await
            .expect("handshake");
        assert_eq!(client.handshake().server_version, "test");
        assert_eq!(client.handshake().protocol_version, RPC_PROTOCOL_VERSION);
        assert!(client.supports(Method::Status));
        assert!(!client.supports(Method::CronList));
        let status = client
            .request(Method::Status, json!({}))
            .await
            .expect("status");
        assert_eq!(status["active_sessions"], json!(0));
        assert_eq!(client.state(), ConnectionState::Connected);
    }

    #[tokio::test]
    async fn notifications_and_inbound_requests_are_routed_and_answered() {
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        let mut seen = fake_daemon(server_half, None);
        let client = RpcClient::connect_over(client_half, ConnectOptions::default())
            .await
            .expect("handshake");
        let mut inbound = client.take_inbound_requests().expect("first claim");
        assert!(
            client.take_inbound_requests().is_none(),
            "second claim is refused"
        );
        let request = tokio::time::timeout(Duration::from_secs(2), inbound.recv())
            .await
            .expect("inbound request arrives")
            .expect("queue open");
        assert_eq!(request.method, "elicitation/create");
        assert_eq!(request.id, json!("srv-1"));
        assert!(
            client
                .respond(request.id, Ok(json!({"action": "accept"})))
                .await
        );
        let echoed = tokio::time::timeout(Duration::from_secs(2), seen.recv())
            .await
            .expect("daemon sees the answer")
            .expect("channel open");
        assert_eq!(echoed["id"], json!("srv-1"));
        assert_eq!(echoed["result"]["action"], json!("accept"));
        assert!(echoed.get("method").is_none());
    }

    #[tokio::test]
    async fn notification_subscribers_receive_pushed_frames() {
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        let _seen = fake_daemon(server_half, None);
        let client = RpcClient::connect_over(client_half, ConnectOptions::default())
            .await
            .expect("handshake");
        let mut rx = client.notifications();
        let _ = client
            .request(Method::Status, json!({}))
            .await
            .expect("status");
        let notification = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("notification arrives")
            .expect("subscription is live");
        assert_eq!(notification.method, "logs/event");
        assert_eq!(notification.params["n"], json!(1));
    }

    #[tokio::test]
    async fn handshake_rpc_error_is_surfaced() {
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        let _seen = fake_daemon(
            server_half,
            Some(JsonRpcError {
                code: zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED,
                message: "credential required".to_string(),
                data: None,
            }),
        );
        let error = match RpcClient::connect_over(client_half, ConnectOptions::default()).await {
            Ok(_) => panic!("handshake must fail"),
            Err(e) => e,
        };
        match error {
            ClientError::Rpc(e) => {
                assert_eq!(e.code, zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn handshake_times_out_when_the_daemon_never_answers() {
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        // Keep the server half alive but silent.
        let _silent = server_half;
        let options = ConnectOptions {
            handshake_timeout: Some(Duration::from_millis(50)),
            ..ConnectOptions::default()
        };
        match RpcClient::connect_over(client_half, options).await {
            Err(ClientError::Timeout { method, .. }) => assert_eq!(method, "initialize"),
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("handshake must time out"),
        }
    }

    #[tokio::test]
    async fn a_dropped_daemon_fails_pending_requests_and_marks_disconnected() {
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        let _seen = fake_daemon(server_half, None);
        let client = RpcClient::connect_over(client_half, ConnectOptions::default())
            .await
            .expect("handshake");
        // `hang` makes the fake daemon exit, which closes the server half.
        let pending = client.request_with_timeout("hang", json!({}), Duration::from_secs(5));
        let closed = tokio::time::timeout(Duration::from_secs(2), client.closed());
        let (result, _) = tokio::join!(pending, closed);
        match result {
            Err(ClientError::Disconnected(_)) => {}
            other => panic!("expected Disconnected, got {other:?}"),
        }
        assert!(matches!(client.state(), ConnectionState::Disconnected(_)));
    }

    #[tokio::test]
    async fn cancelling_the_dial_releases_the_transport() {
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel::<()>();
        // A peer that reads `initialize`, withholds its answer, and then
        // reports whether the client's side ever closed.
        let peer = tokio::spawn(async move {
            let (read_half, write_half) = tokio::io::split(server_half);
            let mut reader = BufReader::new(read_half);
            let saw_initialize = read_frame(&mut reader).await.is_some();
            let _ = seen_tx.send(());
            let eof = read_frame(&mut reader).await.is_none();
            drop(write_half);
            (saw_initialize, eof)
        });
        let options = ConnectOptions {
            handshake_timeout: Some(Duration::from_secs(30)),
            ..ConnectOptions::default()
        };
        let dial = tokio::spawn(RpcClient::connect_over(client_half, options));
        seen_rx.await.expect("peer saw initialize");
        dial.abort();
        let _ = dial.await;
        let (saw_initialize, eof) = tokio::time::timeout(Duration::from_secs(2), peer)
            .await
            .expect("the cancelled dial must close its transport")
            .expect("peer task");
        assert!(saw_initialize);
        assert!(eof, "peer must observe EOF once the dial is cancelled");
    }

    #[tokio::test]
    async fn peer_eof_during_initialize_fails_promptly() {
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        // Read the handshake, then drop the whole stream without replying.
        tokio::spawn(async move {
            let (read_half, _write_half) = tokio::io::split(server_half);
            let mut reader = BufReader::new(read_half);
            let _ = read_frame(&mut reader).await;
        });
        let options = ConnectOptions {
            handshake_timeout: Some(Duration::from_secs(10)),
            ..ConnectOptions::default()
        };
        let started = std::time::Instant::now();
        match RpcClient::connect_over(client_half, options).await {
            Err(ClientError::Disconnected(_)) => {}
            Err(other) => panic!("expected Disconnected, got {other}"),
            Ok(_) => panic!("handshake must fail"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a dropped peer must not wait out the handshake ceiling"
        );
    }

    #[tokio::test]
    async fn unanswered_server_requests_are_bounded_and_responses_still_flow() {
        const FLOOD: usize = 48;
        let (client_half, server_half) = tokio::io::duplex(64 * 1024);
        let rejected = Arc::new(AtomicUsize::new(0));
        let (flooded_tx, flooded_rx) = tokio::sync::oneshot::channel::<()>();
        let (status_id_tx, mut status_id_rx) = mpsc::unbounded_channel::<Value>();
        let (init_id_tx, init_id_rx) = tokio::sync::oneshot::channel::<Value>();
        let (read_half, mut write_half) = tokio::io::split(server_half);
        // Peer reader: counts the client's refusals and forwards its `status`.
        let peer_rejected = Arc::clone(&rejected);
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut init_id_tx = Some(init_id_tx);
            while let Some(line) = read_frame(&mut reader).await {
                let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                match frame.get("method").and_then(Value::as_str) {
                    Some("initialize") => {
                        if let Some(tx) = init_id_tx.take() {
                            let _ = tx.send(frame["id"].clone());
                        }
                    }
                    Some("status") => {
                        let _ = status_id_tx.send(frame["id"].clone());
                    }
                    Some(_) => {}
                    None => {
                        if frame["error"]["code"] == json!(INBOUND_REQUEST_REJECTED) {
                            peer_rejected.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        });
        // Peer writer: answers initialize, floods 1 MiB requests, then serves
        // the client's `status` once asked.
        tokio::spawn(async move {
            let Ok(init_id) = init_id_rx.await else {
                return;
            };
            let init = json!({
                "jsonrpc": "2.0", "id": init_id,
                "result": {
                    "protocol_version": 1, "server_version": "test", "server_pid": 7,
                    "capabilities": ["status"], "principal_id": "shared-operator", "commands": [],
                },
            });
            let _ = write_half.write_all(format!("{init}\n").as_bytes()).await;
            let payload = "x".repeat(1024 * 1024);
            for n in 0..FLOOD {
                let ask = json!({
                    "jsonrpc": "2.0", "id": format!("srv-{n}"),
                    "method": "elicitation/create", "params": {"q": payload},
                });
                if write_half
                    .write_all(format!("{ask}\n").as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = flooded_tx.send(());
            if let Some(id) = status_id_rx.recv().await {
                let response =
                    json!({"jsonrpc": "2.0", "id": id, "result": {"active_sessions": 0}});
                let _ = write_half
                    .write_all(format!("{response}\n").as_bytes())
                    .await;
            }
        });
        let client = RpcClient::connect_over(client_half, ConnectOptions::default())
            .await
            .expect("handshake");
        // Nobody claims the inbound receiver: the flood has no consumer.
        tokio::time::timeout(Duration::from_secs(20), flooded_rx)
            .await
            .expect("flood completes")
            .expect("peer alive");
        let status = client
            .request_with_timeout("status", json!({}), Duration::from_secs(5))
            .await
            .expect("responses still flow while the inbound lane is saturated");
        assert_eq!(status["active_sessions"], json!(0));
        let refused = client.rejected_inbound_requests();
        assert!(
            refused >= FLOOD - INBOUND_REQUEST_QUEUE_DEPTH,
            "at most the queue depth may be held; refused {refused} of {FLOOD}"
        );
        assert!(
            (FLOOD - refused) * 1024 * 1024 <= INBOUND_REQUEST_BYTE_BUDGET,
            "held requests exceed the byte budget; refused {refused} of {FLOOD}"
        );
        // The refusals reached the peer as error responses.
        tokio::time::timeout(Duration::from_secs(5), async {
            while rejected.load(Ordering::Relaxed) < refused {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("peer receives every refusal");
        assert_eq!(rejected.load(Ordering::Relaxed), refused);
    }

    /// Record every byte a client writes and answer its `initialize`, until
    /// the client closes the stream.
    async fn record_and_answer<S>(stream: S) -> Vec<u8>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let mut seen = Vec::new();
        while let Some(line) = read_frame(&mut reader).await {
            seen.extend_from_slice(line.as_bytes());
            let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if frame.get("method").and_then(Value::as_str) == Some("initialize") {
                let response = json!({
                    "jsonrpc": "2.0", "id": frame["id"],
                    "result": {
                        "protocol_version": 1, "server_version": "test", "server_pid": 7,
                        "capabilities": [], "principal_id": "shared-operator", "commands": [],
                    },
                });
                let _ = write_half
                    .write_all(format!("{response}\n").as_bytes())
                    .await;
            }
        }
        seen
    }

    async fn bytes_seen(peer: JoinHandle<Vec<u8>>) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(5), peer)
            .await
            .expect("the client closes the stream")
            .expect("peer task")
    }

    fn with_token() -> ConnectOptions {
        ConnectOptions {
            auth_token: Some("bearer-secret".to_string()),
            ..ConnectOptions::default()
        }
    }

    /// Every way a dial can carry something reusable, each without a token
    /// except the first, and a name for failure messages.
    fn credential_bearing_dials() -> Vec<(&'static str, ConnectOptions)> {
        vec![
            ("token", with_token()),
            (
                "forwarded environment",
                ConnectOptions {
                    env: HashMap::from([(
                        "PROVIDER_API_KEY".to_string(),
                        "env-secret".to_string(),
                    )]),
                    ..ConnectOptions::default()
                },
            ),
            (
                "tui signature",
                ConnectOptions {
                    tui_id: Some("tui-1".to_string()),
                    tui_sig: Some("sig-secret".to_string()),
                    ..ConnectOptions::default()
                },
            ),
        ]
    }

    /// Serve one connection on a socket bound at `path`.
    #[cfg(unix)]
    fn listen_once(path: &Path) -> JoinHandle<Vec<u8>> {
        let listener = tokio::net::UnixListener::bind(path).expect("bind test socket");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            record_and_answer(stream).await
        })
    }

    /// A directory under `root` with exactly `mode`.
    #[cfg(unix)]
    fn dir_with_mode(root: &Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = root.join(name);
        std::fs::create_dir(&dir).expect("create dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
        std::fs::canonicalize(&dir).expect("canonical dir")
    }

    /// This process's uid, as the owner of a directory it just created.
    #[cfg(unix)]
    fn own_uid(dir: &Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir).expect("tempdir metadata").uid()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_credential_is_never_written_to_an_endpoint_of_another_account() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.sock");
        let peer = listen_once(&path);
        let own = own_uid(dir.path());
        // The listener runs as this account, so expecting any other uid puts
        // the client exactly where it would be facing another user's socket.
        let other = own.wrapping_add(1);
        let options = ConnectOptions {
            endpoint_owner: EndpointOwner::Uid(other),
            ..with_token()
        };
        match RpcClient::connect_local(&path, options).await {
            Err(error @ ClientError::UntrustedEndpoint { .. }) => {
                let message = error.to_string();
                let ClientError::UntrustedEndpoint {
                    endpoint,
                    rejection,
                } = error
                else {
                    unreachable!()
                };
                assert_eq!(endpoint, path);
                assert_eq!(
                    rejection,
                    EndpointRejection::PeerUid {
                        expected: other,
                        actual: own
                    }
                );
                assert!(
                    message.contains(&format!("served by uid {own}, expected {other}")),
                    "{message}"
                );
            }
            Err(other) => panic!("expected UntrustedEndpoint, got {other}"),
            Ok(_) => panic!("the dial must be refused"),
        }
        assert!(
            bytes_seen(peer).await.is_empty(),
            "not one byte may reach an unverified endpoint"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_same_account_endpoint_in_a_private_directory_receives_the_credential() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.sock");
        let peer = listen_once(&path);
        let client = RpcClient::connect_local(&path, with_token())
            .await
            .expect("a verified endpoint completes the handshake");
        assert_eq!(client.handshake().server_version, "test");
        drop(client);
        let seen = String::from_utf8(bytes_seen(peer).await).expect("utf-8 frames");
        assert!(seen.contains("\"auth_token\":\"bearer-secret\""), "{seen}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_socket_in_a_directory_other_accounts_can_write_is_refused() {
        // Group-writable, other-writable, and `/tmp`-style sticky.
        for mode in [0o770, 0o707, 0o1777] {
            let root = tempfile::tempdir().expect("tempdir");
            let shared = dir_with_mode(root.path(), "shared", mode);
            let path = shared.join("daemon.sock");
            let peer = listen_once(&path);
            match RpcClient::connect_local(&path, with_token()).await {
                Err(ClientError::UntrustedEndpoint { rejection, .. }) => assert_eq!(
                    rejection,
                    EndpointRejection::DirectoryWritable {
                        dir: shared.clone(),
                        mode
                    },
                    "mode {mode:o}"
                ),
                Err(other) => panic!("mode {mode:o}: expected UntrustedEndpoint, got {other}"),
                Ok(_) => panic!("mode {mode:o}: the dial must be refused"),
            }
            assert!(bytes_seen(peer).await.is_empty(), "mode {mode:o}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn every_dial_verifies_the_endpoint_again() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("daemon.sock");
        let options = with_token();

        let first = listen_once(&path);
        let client = RpcClient::connect_local(&path, options.clone())
            .await
            .expect("the original endpoint verifies");
        drop(client);
        assert!(!bytes_seen(first).await.is_empty());

        // The endpoint is replaced: the same path now leads to a listener
        // in a directory any account can write. Nothing from the first
        // dial may carry over to the second.
        let shared = dir_with_mode(root.path(), "shared", 0o777);
        let replacement = shared.join("daemon.sock");
        let second = listen_once(&replacement);
        std::fs::remove_file(&path).expect("remove the original socket");
        std::os::unix::fs::symlink(&replacement, &path).expect("redirect the path");
        match RpcClient::connect_local(&path, options).await {
            Err(ClientError::UntrustedEndpoint { rejection, .. }) => assert_eq!(
                rejection,
                EndpointRejection::DirectoryWritable {
                    dir: shared,
                    mode: 0o777
                }
            ),
            Err(other) => panic!("expected UntrustedEndpoint, got {other}"),
            Ok(_) => panic!("the replaced endpoint must be refused"),
        }
        assert!(bytes_seen(second).await.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn environment_and_tui_signature_dials_are_verified_like_tokens() {
        for (kind, options) in credential_bearing_dials() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("daemon.sock");
            let peer = listen_once(&path);
            let own = own_uid(dir.path());
            let options = ConnectOptions {
                endpoint_owner: EndpointOwner::Uid(own.wrapping_add(1)),
                ..options
            };
            match RpcClient::connect_local(&path, options).await {
                Err(ClientError::UntrustedEndpoint { rejection, .. }) => assert_eq!(
                    rejection,
                    EndpointRejection::PeerUid {
                        expected: own.wrapping_add(1),
                        actual: own
                    },
                    "{kind}"
                ),
                Err(other) => panic!("{kind}: expected UntrustedEndpoint, got {other}"),
                Ok(_) => panic!("{kind}: the dial must be refused"),
            }
            assert!(
                bytes_seen(peer).await.is_empty(),
                "{kind}: not one byte may reach an unverified endpoint"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_probe_checks_the_endpoint_like_a_credential_dial_and_writes_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let own = own_uid(root.path());

        let path = root.path().join("daemon.sock");
        let peer = listen_once(&path);
        RpcClient::probe_local(&path, EndpointOwner::SameAccount)
            .await
            .expect("a private same-account endpoint passes");
        assert!(
            bytes_seen(peer).await.is_empty(),
            "a passing probe writes nothing"
        );

        let shared = dir_with_mode(root.path(), "shared", 0o777);
        let path = shared.join("daemon.sock");
        let peer = listen_once(&path);
        match RpcClient::probe_local(&path, EndpointOwner::SameAccount).await {
            Err(ClientError::UntrustedEndpoint { rejection, .. }) => assert_eq!(
                rejection,
                EndpointRejection::DirectoryWritable {
                    dir: shared.clone(),
                    mode: 0o777
                }
            ),
            other => panic!("expected UntrustedEndpoint, got {other:?}"),
        }
        assert!(bytes_seen(peer).await.is_empty());

        let path = root.path().join("other.sock");
        let peer = listen_once(&path);
        match RpcClient::probe_local(&path, EndpointOwner::Uid(own.wrapping_add(1))).await {
            Err(ClientError::UntrustedEndpoint { rejection, .. }) => assert!(
                matches!(rejection, EndpointRejection::PeerUid { .. }),
                "{rejection}"
            ),
            other => panic!("expected UntrustedEndpoint, got {other:?}"),
        }
        assert!(bytes_seen(peer).await.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dial_that_presents_nothing_reusable_is_not_gated() {
        let root = tempfile::tempdir().expect("tempdir");
        let own = own_uid(root.path());
        let shared = dir_with_mode(root.path(), "shared", 0o777);
        let path = shared.join("daemon.sock");
        let _peer = listen_once(&path);
        // Every check would fail here. No token, no forwarded environment
        // and no TUI signature (an unsigned TUI id is not one), so the dial
        // runs none of them.
        let options = ConnectOptions {
            tui_id: Some("tui-1".to_string()),
            endpoint_owner: EndpointOwner::Uid(own.wrapping_add(1)),
            ..ConnectOptions::default()
        };
        let client = RpcClient::connect_local(&path, options)
            .await
            .expect("a dial with nothing reusable behaves as before");
        assert_eq!(client.handshake().server_version, "test");
    }

    #[cfg(windows)]
    fn serve_pipe_once(name: &str) -> JoinHandle<Vec<u8>> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(name)
            .expect("create test pipe");
        tokio::spawn(async move {
            server.connect().await.expect("client connects");
            record_and_answer(server).await
        })
    }

    #[cfg(windows)]
    fn test_pipe_name(tag: &str) -> String {
        format!(r"\\.\pipe\zeroclaw-rpc-client-{tag}-{}", std::process::id())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_refuses_a_credential_dial_it_cannot_verify() {
        for (n, (kind, options)) in credential_bearing_dials().into_iter().enumerate() {
            let name = test_pipe_name(&format!("credential-{n}"));
            let peer = serve_pipe_once(&name);
            match RpcClient::connect_local(Path::new(&name), options).await {
                Err(ClientError::UntrustedEndpoint { rejection, .. }) => {
                    assert_eq!(rejection, EndpointRejection::Unsupported, "{kind}");
                }
                Err(other) => panic!("{kind}: expected UntrustedEndpoint, got {other}"),
                Ok(_) => panic!("{kind}: an unverifiable pipe must not receive a credential"),
            }
            assert!(bytes_seen(peer).await.is_empty(), "{kind}");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_dials_without_a_token_as_before() {
        let name = test_pipe_name("tokenless");
        let _peer = serve_pipe_once(&name);
        let client = RpcClient::connect_local(Path::new(&name), ConnectOptions::default())
            .await
            .expect("a tokenless dial behaves as before");
        assert_eq!(client.handshake().server_version, "test");
    }

    #[tokio::test]
    async fn connect_options_carry_the_protocol_version_and_credential() {
        let options = ConnectOptions {
            auth_token: Some("tok".to_string()),
            auth_provider: Some("native".to_string()),
            ..ConnectOptions::default()
        };
        let params = options.initialize_params();
        assert_eq!(params.protocol_version, RPC_PROTOCOL_VERSION);
        assert_eq!(params.auth_token.as_deref(), Some("tok"));
        assert_eq!(params.auth_provider.as_deref(), Some("native"));
        assert!(params.env.is_empty());
    }
}
