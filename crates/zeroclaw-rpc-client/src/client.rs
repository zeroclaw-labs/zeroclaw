//! The client: dial, handshake, multiplex, and observe disconnects.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};
use tokio::task::{AbortHandle, JoinHandle};
use zeroclaw_api::jsonrpc::{
    JSONRPC_VERSION, JsonRpcError, JsonRpcFrame, JsonRpcResponse, RpcOutbound,
};
use zeroclaw_rpc_proto::types::{InitializeParams, InitializeResult};
use zeroclaw_rpc_proto::{Method, RPC_PROTOCOL_VERSION};

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

/// Why a dial or a request failed.
#[derive(Debug)]
pub enum ClientError {
    /// The transport failed while dialing or writing.
    Io(std::io::Error),
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
#[derive(Debug)]
pub struct InboundRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
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
    /// TUI identity from a previous connection, to reclaim it.
    pub tui_id: Option<String>,
    pub tui_sig: Option<String>,
    /// Shell environment forwarded to daemon-spawned subprocesses.
    pub env: HashMap<String, String>,
    /// Advertised client capabilities (for example the ACP elicitation form).
    pub client_capabilities: Option<Value>,
    /// Ceiling for the handshake; [`DEFAULT_HANDSHAKE_TIMEOUT`] when unset.
    pub handshake_timeout: Option<Duration>,
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
}

/// One authenticated connection to the daemon.
///
/// Dropping the client aborts its reader and writer tasks; the daemon sees
/// EOF and releases the connection.
pub struct RpcClient {
    rpc: Arc<RpcOutbound>,
    notifications: broadcast::Sender<Notification>,
    inbound: Mutex<Option<mpsc::UnboundedReceiver<InboundRequest>>>,
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
    inbound: &mpsc::UnboundedSender<InboundRequest>,
    frame: JsonRpcFrame,
) {
    match frame {
        JsonRpcFrame::Response { id, result } => {
            if let Some(id) = id.as_str() {
                rpc.dispatch_validated_response(id, result);
            }
        }
        JsonRpcFrame::Request(request) => match request.id {
            Some(id) if !id.is_null() => {
                let _ = inbound.send(InboundRequest {
                    id,
                    method: request.method,
                    params: request.params,
                });
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
    pub async fn connect_local(path: &Path, options: ConnectOptions) -> Result<Self, ClientError> {
        let stream = open_local_stream(path).await?;
        Self::connect_over(stream, options).await
    }

    /// Run the handshake over an already-open byte stream: a socket, a pipe,
    /// or the daemon's in-process duplex.
    pub async fn connect_over<S>(stream: S, options: ConnectOptions) -> Result<Self, ClientError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(WRITER_QUEUE_DEPTH);
        let rpc = Arc::new(RpcOutbound::new(writer_tx));
        let state = Arc::new(Mutex::new(ConnectionState::Connected));
        let (notifications, _) = broadcast::channel::<Notification>(NOTIFICATION_CAPACITY);
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<InboundRequest>();

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
                route_frame(&reader_rpc, &reader_notifications, &inbound_tx, frame);
            }
            set_disconnected(&reader_state, "daemon closed the connection".to_string());
            // Stopping the writer drops the queue receiver, which resolves
            // `RpcOutbound::closed()` and fails every request still waiting.
            writer_abort.abort();
        });

        let handshake_timeout = options
            .handshake_timeout
            .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT);
        let params = match serde_json::to_value(options.initialize_params()) {
            Ok(params) => params,
            Err(e) => {
                reader.abort();
                writer.abort();
                return Err(ClientError::Handshake(format!(
                    "encoding initialize params: {e}"
                )));
            }
        };
        let response = tokio::time::timeout(
            handshake_timeout,
            rpc.request(Method::Initialize.wire_name(), params),
        )
        .await;
        let handshake = match response {
            Ok(Ok(value)) => match serde_json::from_value::<InitializeResult>(value) {
                Ok(result) => result,
                Err(e) => {
                    reader.abort();
                    writer.abort();
                    return Err(ClientError::Handshake(format!(
                        "undecodable initialize result: {e}"
                    )));
                }
            },
            Ok(Err(error)) => {
                reader.abort();
                writer.abort();
                return Err(ClientError::Rpc(error));
            }
            Err(_) => {
                reader.abort();
                writer.abort();
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
            state,
            reader,
            writer,
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
    /// claimed. Requests that arrive before the claim are retained.
    pub fn take_inbound_requests(&self) -> Option<mpsc::UnboundedReceiver<InboundRequest>> {
        self.inbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
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
