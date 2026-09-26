//! Host-mediated WebSocket resources for tool and channel component worlds.
//!
//! This module owns framing and lifecycle only. Destination policy, DNS
//! validation/pinning, TLS profile selection, and connection accounting remain
//! in the instance's egress service: a WebSocket reaches exactly the hosts the
//! operator granted in `egress_hosts`, plaintext WebSocket included (ADR-014
//! has no separate plaintext grant).

use std::future::pending;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::{Request, Response};
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::frame::Utf8Bytes;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use wasmtime::component::Resource;

use crate::component::{PluginState, bindings};
use crate::egress::{
    AuthorizedEgress, EGRESS_CONNECT_DEADLINE, EgressError, EgressRequest, EgressTransport,
};

const MAX_URL_BYTES: usize = 4 * 1024;
const MAX_HEADER_COUNT: usize = 32;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_SUBPROTOCOL_COUNT: usize = 16;
const MAX_SUBPROTOCOL_BYTES: usize = 4 * 1024;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_INBOUND_FRAME_BYTES: usize = 256 * 1024;
const OUTBOUND_QUEUE_MESSAGES: usize = 4;
const INBOUND_QUEUE_EVENTS: usize = 4;
const CLOSE_DEADLINE: Duration = Duration::from_secs(5);

type ClientSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebSocketError {
    InvalidOptions,
    ReservedHeader,
    InvalidSubprotocol,
    InvalidClose,
    AccessDenied,
    DestinationDenied,
    DnsFailed,
    ConnectionLimit,
    Timeout,
    ConnectFailed,
    TlsFailed,
    HandshakeFailed,
    PayloadTooLarge,
    QueueFull,
    Closed,
    ProtocolError,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WebSocketMessage {
    Text(String),
    Binary(Vec<u8>),
}

impl WebSocketMessage {
    fn len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Binary(bytes) => bytes.len(),
        }
    }

    fn into_tungstenite(self) -> Message {
        match self {
            Self::Text(text) => Message::Text(text.into()),
            Self::Binary(bytes) => Message::Binary(bytes.into()),
        }
    }
}

fn validate_message(message: &WebSocketMessage) -> Result<(), WebSocketError> {
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(WebSocketError::PayloadTooLarge);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WebSocketClose {
    code: u16,
    reason: String,
}

impl WebSocketClose {
    fn into_tungstenite(self) -> Result<CloseFrame, WebSocketError> {
        let code = CloseCode::from(self.code);
        if !code.is_allowed() || self.reason.len() > 123 {
            return Err(WebSocketError::InvalidClose);
        }
        Ok(CloseFrame {
            code,
            reason: Utf8Bytes::from(self.reason),
        })
    }

    fn from_tungstenite(frame: CloseFrame) -> Self {
        Self {
            code: frame.code.into(),
            reason: frame.reason.to_string(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum WebSocketEvent {
    Message(WebSocketMessage),
    Closed(Option<WebSocketClose>),
    Failed(WebSocketError),
}

enum ConnectionCommand {
    Send(WebSocketMessage),
    Close(Option<WebSocketClose>),
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum ConnectionState {
    Open,
    Closing,
    Terminal,
}

/// Canonical transient state for one Component Model connection resource.
///
/// The authorization is deliberately retained here, rather than in the socket
/// task, so a terminal event can be drained while the resource continues to
/// hold its shared cross-transport connection slot.
pub struct WebSocketConnection {
    commands: mpsc::Sender<ConnectionCommand>,
    events: mpsc::Receiver<WebSocketEvent>,
    negotiated_subprotocol: Option<String>,
    state: Arc<AtomicU8>,
    task: JoinHandle<()>,
    _authorization: AuthorizedEgress,
}

impl WebSocketConnection {
    /// Drain one event without blocking.
    ///
    /// `None` means nothing is ready yet. Once the terminal event has been
    /// drained and the socket task has ended, every later call reports
    /// `Closed`, so a guest polling a finished connection stops instead of
    /// spinning on `None` until its host-call budget runs out.
    fn try_receive(&mut self) -> Result<Option<WebSocketEvent>, WebSocketError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => Err(WebSocketError::Closed),
        }
    }
}

impl Drop for WebSocketConnection {
    fn drop(&mut self) {
        self.state
            .store(ConnectionState::Terminal as u8, Ordering::Release);
        self.task.abort();
    }
}

struct ConnectOptions {
    url: String,
    headers: Vec<(String, String)>,
    subprotocols: Vec<String>,
    tls_profile: Option<String>,
}

struct PreparedConnection {
    request: Request,
    host: String,
    port: u16,
    encrypted: bool,
    subprotocols: Vec<String>,
    tls_profile: Option<String>,
}

impl PluginState {
    async fn websocket_connect(
        &mut self,
        options: ConnectOptions,
    ) -> Result<WebSocketConnection, WebSocketError> {
        // A connection opens only in a tool-execute or channel-service frame,
        // never while the host is probing metadata.
        if !self.charge_host_call() || !self.instance_services_enabled() {
            return Err(WebSocketError::Unavailable);
        }
        let prepared = prepare_connection(options)?;
        let mut request = EgressRequest::new(
            self.scope().clone(),
            EgressTransport::WebSocket {
                encrypted: prepared.encrypted,
            },
            &prepared.host,
            prepared.port,
        )
        .map_err(map_egress_error)?;
        if let Some(profile) = prepared.tls_profile.as_deref() {
            request = request
                .with_tls_profile(profile)
                .map_err(map_egress_error)?;
        }
        // No egress authority attached means no reach, as for `wasi:http`.
        let egress = self
            .egress_service()
            .ok_or(WebSocketError::DestinationDenied)?;

        let connected = tokio::time::timeout(EGRESS_CONNECT_DEADLINE, async {
            let authorized = egress.authorize(request).await.map_err(map_egress_error)?;
            let tls_config = if prepared.encrypted {
                Some(
                    self.tls_client_config(&authorized)
                        .await
                        .map_err(map_egress_error)?,
                )
            } else {
                None
            };
            let (socket, negotiated_subprotocol) =
                dial_authorized(&prepared, &authorized, tls_config).await?;
            Ok::<_, WebSocketError>((socket, negotiated_subprotocol, authorized))
        })
        .await
        .map_err(|_| WebSocketError::Timeout)??;

        let (socket, negotiated_subprotocol, authorization) = connected;
        Ok(start_connection(
            socket,
            negotiated_subprotocol,
            authorization,
        ))
    }
}

fn start_connection(
    socket: ClientSocket,
    negotiated_subprotocol: Option<String>,
    authorization: AuthorizedEgress,
) -> WebSocketConnection {
    let (command_tx, command_rx) = mpsc::channel(OUTBOUND_QUEUE_MESSAGES);
    let (event_tx, event_rx) = mpsc::channel(INBOUND_QUEUE_EVENTS);
    let state = Arc::new(AtomicU8::new(ConnectionState::Open as u8));
    let task_state = Arc::clone(&state);
    let task = zeroclaw_spawn::spawn!(run_connection(socket, command_rx, event_tx, task_state,));

    WebSocketConnection {
        commands: command_tx,
        events: event_rx,
        negotiated_subprotocol,
        state,
        task,
        _authorization: authorization,
    }
}

fn prepare_connection(options: ConnectOptions) -> Result<PreparedConnection, WebSocketError> {
    if options.url.is_empty() || options.url.len() > MAX_URL_BYTES {
        return Err(WebSocketError::InvalidOptions);
    }
    validate_headers(&options.headers)?;
    validate_subprotocols(&options.subprotocols)?;

    let mut request = options
        .url
        .as_str()
        .into_client_request()
        .map_err(|_| WebSocketError::InvalidOptions)?;
    let uri = request.uri();
    let scheme = uri.scheme_str().ok_or(WebSocketError::InvalidOptions)?;
    let encrypted = match scheme {
        "ws" => false,
        "wss" => true,
        _ => return Err(WebSocketError::InvalidOptions),
    };
    let authority = uri.authority().ok_or(WebSocketError::InvalidOptions)?;
    if authority.as_str().contains('@') {
        return Err(WebSocketError::InvalidOptions);
    }
    let host = uri
        .host()
        .ok_or(WebSocketError::InvalidOptions)?
        .to_string();
    let port = uri.port_u16().unwrap_or(if encrypted { 443 } else { 80 });

    for (name, value) in options.headers {
        let name = HeaderName::from_str(&name).map_err(|_| WebSocketError::InvalidOptions)?;
        let value = HeaderValue::from_str(&value).map_err(|_| WebSocketError::InvalidOptions)?;
        request.headers_mut().append(name, value);
    }
    if !options.subprotocols.is_empty() {
        let joined = options.subprotocols.join(", ");
        let value =
            HeaderValue::from_str(&joined).map_err(|_| WebSocketError::InvalidSubprotocol)?;
        request.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, value);
    }

    Ok(PreparedConnection {
        request,
        host,
        port,
        encrypted,
        subprotocols: options.subprotocols,
        tls_profile: options.tls_profile,
    })
}

fn validate_headers(headers: &[(String, String)]) -> Result<(), WebSocketError> {
    if headers.len() > MAX_HEADER_COUNT {
        return Err(WebSocketError::InvalidOptions);
    }
    let mut total_bytes = 0usize;
    for (name, value) in headers {
        total_bytes = total_bytes
            .checked_add(name.len())
            .and_then(|bytes| bytes.checked_add(value.len()))
            .ok_or(WebSocketError::InvalidOptions)?;
        if total_bytes > MAX_HEADER_BYTES {
            return Err(WebSocketError::InvalidOptions);
        }
        let parsed = HeaderName::from_str(name).map_err(|_| WebSocketError::InvalidOptions)?;
        let normalized = parsed.as_str();
        if matches!(normalized, "host" | "connection" | "upgrade")
            || normalized.starts_with("sec-websocket-")
        {
            return Err(WebSocketError::ReservedHeader);
        }
        HeaderValue::from_str(value).map_err(|_| WebSocketError::InvalidOptions)?;
    }
    Ok(())
}

fn validate_subprotocols(subprotocols: &[String]) -> Result<(), WebSocketError> {
    if subprotocols.len() > MAX_SUBPROTOCOL_COUNT {
        return Err(WebSocketError::InvalidSubprotocol);
    }
    let mut total_bytes = 0usize;
    for (index, subprotocol) in subprotocols.iter().enumerate() {
        total_bytes = total_bytes
            .checked_add(subprotocol.len())
            .and_then(|bytes| bytes.checked_add(usize::from(index > 0) * 2))
            .ok_or(WebSocketError::InvalidSubprotocol)?;
        if total_bytes > MAX_SUBPROTOCOL_BYTES
            || subprotocol.is_empty()
            || !subprotocol.bytes().all(is_http_token_byte)
            || subprotocols[..index].contains(subprotocol)
        {
            return Err(WebSocketError::InvalidSubprotocol);
        }
    }
    Ok(())
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

async fn dial_authorized(
    prepared: &PreparedConnection,
    authorized: &AuthorizedEgress,
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<(ClientSocket, Option<String>), WebSocketError> {
    let server_name = if prepared.encrypted {
        Some(
            ServerName::try_from(authorized.request().host().to_string())
                .map_err(|_| WebSocketError::TlsFailed)?,
        )
    } else {
        None
    };
    let mut last_error = WebSocketError::ConnectFailed;
    for address in authorized.destination().addresses() {
        let tcp = match TcpStream::connect(address).await {
            Ok(tcp) => tcp,
            Err(_) => continue,
        };
        let _ = tcp.set_nodelay(true);
        let transport = if let Some(server_name) = server_name.clone() {
            let Some(config) = tls_config.clone() else {
                return Err(WebSocketError::TlsFailed);
            };
            match TlsConnector::from(config).connect(server_name, tcp).await {
                Ok(stream) => MaybeTlsStream::Rustls(stream),
                Err(_) => {
                    last_error = WebSocketError::TlsFailed;
                    continue;
                }
            }
        } else {
            MaybeTlsStream::Plain(tcp)
        };
        match tokio_tungstenite::client_async_with_config(
            prepared.request.clone(),
            transport,
            Some(websocket_config()),
        )
        .await
        {
            Ok((socket, response)) => {
                let subprotocol = selected_subprotocol(&response, &prepared.subprotocols)?;
                return Ok((socket, subprotocol));
            }
            Err(error) => last_error = map_handshake_error(&error),
        }
    }
    Err(last_error)
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(16 * 1024)
        .max_write_buffer_size(2 * MAX_MESSAGE_BYTES)
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_INBOUND_FRAME_BYTES))
}

fn selected_subprotocol(
    response: &Response,
    offered: &[String],
) -> Result<Option<String>, WebSocketError> {
    let mut values = response.headers().get_all(SEC_WEBSOCKET_PROTOCOL).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(WebSocketError::InvalidSubprotocol);
    }
    let selected = value
        .to_str()
        .map_err(|_| WebSocketError::InvalidSubprotocol)?;
    if !is_http_token(selected) || !offered.iter().any(|candidate| candidate == selected) {
        return Err(WebSocketError::InvalidSubprotocol);
    }
    Ok(Some(selected.to_string()))
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(is_http_token_byte)
}

async fn run_connection(
    mut socket: ClientSocket,
    mut commands: mpsc::Receiver<ConnectionCommand>,
    events: mpsc::Sender<WebSocketEvent>,
    state: Arc<AtomicU8>,
) {
    let mut close_deadline = None;
    loop {
        tokio::select! {
            command = commands.recv(), if close_deadline.is_none() => {
                let Some(command) = command else {
                    break;
                };
                match command {
                    ConnectionCommand::Send(message) => {
                        if let Err(error) = socket.send(message.into_tungstenite()).await {
                            send_terminal(&events, &state, WebSocketEvent::Failed(map_socket_error(&error))).await;
                            break;
                        }
                    }
                    ConnectionCommand::Close(frame) => {
                        let frame = match frame.map(WebSocketClose::into_tungstenite).transpose() {
                            Ok(frame) => frame,
                            Err(error) => {
                                send_terminal(&events, &state, WebSocketEvent::Failed(error)).await;
                                break;
                            }
                        };
                        if let Err(error) = socket.send(Message::Close(frame)).await {
                            send_terminal(&events, &state, WebSocketEvent::Failed(map_socket_error(&error))).await;
                            break;
                        }
                        close_deadline = Some(Box::pin(tokio::time::sleep(CLOSE_DEADLINE)));
                    }
                }
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if events.send(WebSocketEvent::Message(WebSocketMessage::Text(text.to_string()))).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if events.send(WebSocketEvent::Message(WebSocketMessage::Binary(bytes.to_vec()))).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(error) = socket.send(Message::Pong(payload)).await {
                            send_terminal(&events, &state, WebSocketEvent::Failed(map_socket_error(&error))).await;
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Ok(Message::Close(frame))) => {
                        // Tungstenite queues the protocol-mandated close reply
                        // while reading the peer frame; flush it before the
                        // socket resource transitions to terminal state.
                        let _ = socket.flush().await;
                        send_terminal(
                            &events,
                            &state,
                            WebSocketEvent::Closed(frame.map(WebSocketClose::from_tungstenite)),
                        ).await;
                        break;
                    }
                    Some(Err(TungsteniteError::ConnectionClosed)) => {
                        send_terminal(&events, &state, WebSocketEvent::Closed(None)).await;
                        break;
                    }
                    Some(Err(error)) => {
                        send_terminal(&events, &state, WebSocketEvent::Failed(map_socket_error(&error))).await;
                        break;
                    }
                    None => {
                        send_terminal(&events, &state, WebSocketEvent::Closed(None)).await;
                        break;
                    }
                }
            }
            () = async {
                match close_deadline.as_mut() {
                    Some(deadline) => deadline.await,
                    None => pending().await,
                }
            } => {
                send_terminal(&events, &state, WebSocketEvent::Closed(None)).await;
                break;
            }
        }
    }
    state.store(ConnectionState::Terminal as u8, Ordering::Release);
}

async fn send_terminal(
    events: &mpsc::Sender<WebSocketEvent>,
    state: &AtomicU8,
    event: WebSocketEvent,
) {
    state.store(ConnectionState::Terminal as u8, Ordering::Release);
    let _ = events.send(event).await;
}

fn map_egress_error(error: EgressError) -> WebSocketError {
    match error {
        EgressError::PermissionDenied { .. } => WebSocketError::AccessDenied,
        EgressError::DestinationNotGranted { .. } => WebSocketError::DestinationDenied,
        EgressError::DnsFailed { .. } => WebSocketError::DnsFailed,
        EgressError::ConnectionLimitReached { .. } => WebSocketError::ConnectionLimit,
        EgressError::InvalidTlsMaterial { .. }
        | EgressError::TlsSecretUnavailable { .. }
        | EgressError::AuthorizationScopeMismatch => WebSocketError::TlsFailed,
        EgressError::PolicyUnavailable(_) => WebSocketError::Unavailable,
        EgressError::Network(_)
        | EgressError::InvalidHostPattern(_)
        | EgressError::InvalidNat64Prefix(_)
        | EgressError::InvalidTlsProfileName(_)
        | EgressError::InvalidTlsProfile { .. }
        | EgressError::DuplicateTlsProfile(_)
        | EgressError::TlsProfileWithoutHosts(_)
        | EgressError::TlsProfileHostNotGranted { .. }
        | EgressError::InvalidConnectionLimit
        | EgressError::TlsProfileOnPlaintext(_)
        | EgressError::UnknownTlsProfile(_)
        | EgressError::TlsProfileHostDenied { .. }
        | EgressError::InvalidStartTlsTransition { .. } => WebSocketError::DestinationDenied,
    }
}

fn map_handshake_error(error: &TungsteniteError) -> WebSocketError {
    match error {
        // TCP (and, for WSS, TLS) completed before this boundary. An I/O
        // failure while exchanging the HTTP upgrade is therefore a handshake
        // outcome, not a failed dial.
        TungsteniteError::Io(_) => WebSocketError::HandshakeFailed,
        TungsteniteError::Tls(_) => WebSocketError::TlsFailed,
        TungsteniteError::Capacity(_) => WebSocketError::PayloadTooLarge,
        TungsteniteError::Protocol(_)
        | TungsteniteError::Http(_)
        | TungsteniteError::HttpFormat(_)
        | TungsteniteError::Utf8(_)
        | TungsteniteError::AttackAttempt => WebSocketError::HandshakeFailed,
        TungsteniteError::Url(_) => WebSocketError::InvalidOptions,
        TungsteniteError::ConnectionClosed
        | TungsteniteError::AlreadyClosed
        | TungsteniteError::WriteBufferFull(_) => WebSocketError::HandshakeFailed,
    }
}

fn map_socket_error(error: &TungsteniteError) -> WebSocketError {
    match error {
        TungsteniteError::ConnectionClosed | TungsteniteError::AlreadyClosed => {
            WebSocketError::Closed
        }
        TungsteniteError::Capacity(_) => WebSocketError::PayloadTooLarge,
        TungsteniteError::WriteBufferFull(_) => WebSocketError::QueueFull,
        TungsteniteError::Tls(_) => WebSocketError::TlsFailed,
        TungsteniteError::Protocol(_)
        | TungsteniteError::Utf8(_)
        | TungsteniteError::AttackAttempt => WebSocketError::ProtocolError,
        TungsteniteError::Io(_)
        | TungsteniteError::Url(_)
        | TungsteniteError::Http(_)
        | TungsteniteError::HttpFormat(_) => WebSocketError::Unavailable,
    }
}

macro_rules! impl_websocket_host {
    ($world:ident) => {
        impl bindings::$world::zeroclaw::plugin::websocket::Host for PluginState {
            async fn connect(
                &mut self,
                options: bindings::$world::zeroclaw::plugin::websocket::ConnectOptions,
            ) -> wasmtime::Result<
                Result<
                    Resource<WebSocketConnection>,
                    bindings::$world::zeroclaw::plugin::websocket::WebsocketError,
                >,
            > {
                let options = ConnectOptions {
                    url: options.url,
                    headers: options
                        .headers
                        .into_iter()
                        .map(|header| (header.name, header.value))
                        .collect(),
                    subprotocols: options.subprotocols,
                    tls_profile: options.tls_profile,
                };
                match self.websocket_connect(options).await {
                    Ok(connection) => Ok(Ok(self.resource_table_mut().push(connection)?)),
                    Err(error) => Ok(Err(into_wit_error!($world, error))),
                }
            }
        }

        impl bindings::$world::zeroclaw::plugin::websocket::HostConnection for PluginState {
            async fn send(
                &mut self,
                resource: Resource<WebSocketConnection>,
                message: bindings::$world::zeroclaw::plugin::websocket::Message,
            ) -> wasmtime::Result<
                Result<(), bindings::$world::zeroclaw::plugin::websocket::WebsocketError>,
            > {
                if !self.charge_host_call() {
                    return Ok(Err(into_wit_error!($world, WebSocketError::Unavailable)));
                }
                let message = match message {
                    bindings::$world::zeroclaw::plugin::websocket::Message::Text(text) => {
                        WebSocketMessage::Text(text)
                    }
                    bindings::$world::zeroclaw::plugin::websocket::Message::Binary(bytes) => {
                        WebSocketMessage::Binary(bytes)
                    }
                };
                if let Err(error) = validate_message(&message) {
                    return Ok(Err(into_wit_error!($world, error)));
                }
                let connection = self.resource_table().get(&resource)?;
                if connection.state.load(Ordering::Acquire) != ConnectionState::Open as u8 {
                    return Ok(Err(into_wit_error!($world, WebSocketError::Closed)));
                }
                match connection
                    .commands
                    .try_send(ConnectionCommand::Send(message))
                {
                    Ok(()) => Ok(Ok(())),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        Ok(Err(into_wit_error!($world, WebSocketError::QueueFull)))
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        Ok(Err(into_wit_error!($world, WebSocketError::Closed)))
                    }
                }
            }

            async fn receive(
                &mut self,
                resource: Resource<WebSocketConnection>,
            ) -> wasmtime::Result<
                Result<
                    Option<bindings::$world::zeroclaw::plugin::websocket::Event>,
                    bindings::$world::zeroclaw::plugin::websocket::WebsocketError,
                >,
            > {
                if !self.charge_host_call() {
                    return Ok(Err(into_wit_error!($world, WebSocketError::Unavailable)));
                }
                let connection = self.resource_table_mut().get_mut(&resource)?;
                match connection.try_receive() {
                    Ok(event) => Ok(Ok(event.map(|event| into_wit_event!($world, event)))),
                    Err(error) => Ok(Err(into_wit_error!($world, error))),
                }
            }

            async fn close(
                &mut self,
                resource: Resource<WebSocketConnection>,
                frame: Option<bindings::$world::zeroclaw::plugin::websocket::CloseFrame>,
            ) -> wasmtime::Result<
                Result<(), bindings::$world::zeroclaw::plugin::websocket::WebsocketError>,
            > {
                if !self.charge_host_call() {
                    return Ok(Err(into_wit_error!($world, WebSocketError::Unavailable)));
                }
                let frame = frame.map(|frame| WebSocketClose {
                    code: wit_close_code!($world, frame.code),
                    reason: frame.reason,
                });
                if let Some(frame) = frame.as_ref()
                    && (frame.reason.len() > 123 || !CloseCode::from(frame.code).is_allowed())
                {
                    return Ok(Err(into_wit_error!($world, WebSocketError::InvalidClose)));
                }
                let connection = self.resource_table().get(&resource)?;
                if connection
                    .state
                    .compare_exchange(
                        ConnectionState::Open as u8,
                        ConnectionState::Closing as u8,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    return Ok(Err(into_wit_error!($world, WebSocketError::Closed)));
                }
                match connection
                    .commands
                    .try_send(ConnectionCommand::Close(frame))
                {
                    Ok(()) => Ok(Ok(())),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        if connection
                            .state
                            .compare_exchange(
                                ConnectionState::Closing as u8,
                                ConnectionState::Open as u8,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            Ok(Err(into_wit_error!($world, WebSocketError::QueueFull)))
                        } else {
                            Ok(Err(into_wit_error!($world, WebSocketError::Closed)))
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        connection
                            .state
                            .store(ConnectionState::Terminal as u8, Ordering::Release);
                        Ok(Err(into_wit_error!($world, WebSocketError::Closed)))
                    }
                }
            }

            async fn negotiated_subprotocol(
                &mut self,
                resource: Resource<WebSocketConnection>,
            ) -> wasmtime::Result<
                Result<
                    Option<String>,
                    bindings::$world::zeroclaw::plugin::websocket::WebsocketError,
                >,
            > {
                if !self.charge_host_call() {
                    return Ok(Err(into_wit_error!($world, WebSocketError::Unavailable)));
                }
                Ok(Ok(self
                    .resource_table()
                    .get(&resource)?
                    .negotiated_subprotocol
                    .clone()))
            }

            async fn drop(
                &mut self,
                resource: Resource<WebSocketConnection>,
            ) -> wasmtime::Result<()> {
                self.resource_table_mut().delete(resource)?;
                Ok(())
            }
        }
    };
}

macro_rules! into_wit_error {
    ($world:ident, $error:expr) => {{
        use bindings::$world::zeroclaw::plugin::websocket::WebsocketError as WitError;
        match $error {
            WebSocketError::InvalidOptions => WitError::InvalidOptions,
            WebSocketError::ReservedHeader => WitError::ReservedHeader,
            WebSocketError::InvalidSubprotocol => WitError::InvalidSubprotocol,
            WebSocketError::InvalidClose => WitError::InvalidClose,
            WebSocketError::AccessDenied => WitError::AccessDenied,
            WebSocketError::DestinationDenied => WitError::DestinationDenied,
            WebSocketError::DnsFailed => WitError::DnsFailed,
            WebSocketError::ConnectionLimit => WitError::ConnectionLimit,
            WebSocketError::Timeout => WitError::Timeout,
            WebSocketError::ConnectFailed => WitError::ConnectFailed,
            WebSocketError::TlsFailed => WitError::TlsFailed,
            WebSocketError::HandshakeFailed => WitError::HandshakeFailed,
            WebSocketError::PayloadTooLarge => WitError::PayloadTooLarge,
            WebSocketError::QueueFull => WitError::QueueFull,
            WebSocketError::Closed => WitError::Closed,
            WebSocketError::ProtocolError => WitError::ProtocolError,
            WebSocketError::Unavailable => WitError::Unavailable,
        }
    }};
}

macro_rules! into_wit_close_code {
    ($world:ident, $code:expr) => {{
        use bindings::$world::zeroclaw::plugin::websocket::CloseCode as WitCode;
        match $code {
            1000 => WitCode::Normal,
            1001 => WitCode::GoingAway,
            1002 => WitCode::ProtocolError,
            1003 => WitCode::UnsupportedData,
            1007 => WitCode::InvalidPayload,
            1008 => WitCode::PolicyViolation,
            1009 => WitCode::MessageTooBig,
            1010 => WitCode::MandatoryExtension,
            1011 => WitCode::InternalError,
            1012 => WitCode::ServiceRestart,
            1013 => WitCode::TryAgainLater,
            code => WitCode::Other(code),
        }
    }};
}

macro_rules! wit_close_code {
    ($world:ident, $code:expr) => {{
        use bindings::$world::zeroclaw::plugin::websocket::CloseCode as WitCode;
        match $code {
            WitCode::Normal => 1000,
            WitCode::GoingAway => 1001,
            WitCode::ProtocolError => 1002,
            WitCode::UnsupportedData => 1003,
            WitCode::InvalidPayload => 1007,
            WitCode::PolicyViolation => 1008,
            WitCode::MessageTooBig => 1009,
            WitCode::MandatoryExtension => 1010,
            WitCode::InternalError => 1011,
            WitCode::ServiceRestart => 1012,
            WitCode::TryAgainLater => 1013,
            WitCode::Other(code) => code,
        }
    }};
}

macro_rules! into_wit_event {
    ($world:ident, $event:expr) => {{
        use bindings::$world::zeroclaw::plugin::websocket::{
            CloseFrame as WitCloseFrame, Event as WitEvent, Message as WitMessage,
        };
        match $event {
            WebSocketEvent::Message(WebSocketMessage::Text(text)) => {
                WitEvent::Message(WitMessage::Text(text))
            }
            WebSocketEvent::Message(WebSocketMessage::Binary(bytes)) => {
                WitEvent::Message(WitMessage::Binary(bytes))
            }
            WebSocketEvent::Closed(frame) => WitEvent::Closed(frame.map(|frame| WitCloseFrame {
                code: into_wit_close_code!($world, frame.code),
                reason: frame.reason,
            })),
            WebSocketEvent::Failed(error) => WitEvent::Failed(into_wit_error!($world, error)),
        }
    }};
}

impl_websocket_host!(tool);
impl_websocket_host!(channel);

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;
    use tokio_tungstenite::tungstenite::handshake::server::{
        Callback, ErrorResponse, Request as ServerRequest, Response as ServerResponse,
    };
    use zeroclaw_api::plugin_key::SecretPropertyRef;

    use crate::egress::{
        EgressHostService, EgressPolicy, EgressPolicyResolver, TlsProfile, TlsProfileName,
        build_tls_client_config,
    };
    use crate::instance::PluginInstanceScope;
    use crate::{PluginCapability, PluginManifest, PluginPermission};

    use super::*;

    struct PinnedHandshake;

    impl Callback for PinnedHandshake {
        fn on_request(
            self,
            request: &ServerRequest,
            mut response: ServerResponse,
        ) -> Result<ServerResponse, ErrorResponse> {
            assert_eq!(request.uri().path(), "/events");
            assert_eq!(
                request.headers()[SEC_WEBSOCKET_PROTOCOL],
                "json.v1, binary.v1"
            );
            response.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                HeaderValue::from_static("binary.v1"),
            );
            Ok(response)
        }
    }

    fn options(url: &str) -> ConnectOptions {
        ConnectOptions {
            url: url.to_string(),
            headers: Vec::new(),
            subprotocols: Vec::new(),
            tls_profile: None,
        }
    }

    fn scope(binding: &str) -> PluginInstanceScope {
        let manifest = PluginManifest {
            name: "websocket-fixture".to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: None,
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Channel],
            permissions: vec![PluginPermission::WebSocketClient],
            config_schema: None,
            signature: None,
            publisher_key: None,
            egress: Default::default(),
        };
        PluginInstanceScope::from_manifest(
            &manifest,
            PluginCapability::Channel,
            binding,
            [PluginPermission::WebSocketClient],
        )
        .unwrap()
    }

    fn service(policy: EgressPolicy) -> EgressHostService {
        EgressHostService::with_private_connection_accounting(EgressPolicyResolver::new(
            move |_| Ok(policy.clone()),
        ))
    }

    fn websocket_request(
        scope: PluginInstanceScope,
        prepared: &PreparedConnection,
    ) -> EgressRequest {
        EgressRequest::new(
            scope,
            EgressTransport::WebSocket {
                encrypted: prepared.encrypted,
            },
            &prepared.host,
            prepared.port,
        )
        .and_then(|request| match prepared.tls_profile.as_deref() {
            Some(profile) => request.with_tls_profile(profile),
            None => Ok(request),
        })
        .unwrap()
    }

    /// A loopback TLS server identity for `socket.test`, signed by a private CA
    /// that is trusted only through the `private-ca` profile.
    struct PrivateCa {
        acceptor: TlsAcceptor,
        ca_pem: String,
    }

    impl PrivateCa {
        fn new() -> Self {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let ca_key = rcgen::KeyPair::generate().unwrap();
            let mut ca_parameters =
                rcgen::CertificateParams::new(vec!["WebSocket Test CA".to_string()]).unwrap();
            ca_parameters.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let ca_certificate = ca_parameters.self_signed(&ca_key).unwrap();

            let server_key = rcgen::KeyPair::generate().unwrap();
            let server_parameters =
                rcgen::CertificateParams::new(vec!["socket.test".to_string()]).unwrap();
            let server_certificate = server_parameters
                .signed_by(&server_key, &ca_certificate, &ca_key)
                .unwrap();
            let server_private_key = rustls::pki_types::PrivateKeyDer::Pkcs8(
                rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()),
            );
            let server_config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![server_certificate.der().clone()], server_private_key)
                .unwrap();
            Self {
                acceptor: TlsAcceptor::from(Arc::new(server_config)),
                ca_pem: ca_certificate.pem(),
            }
        }

        /// Egress granting `socket.test` with `max_connections`. The name
        /// resolves to the loopback test server, so the grant carries the
        /// private-address carveout for it.
        fn service(&self, max_connections: usize) -> EgressHostService {
            let profile = TlsProfile::new(
                TlsProfileName::new("private-ca").unwrap(),
                &["socket.test".to_string()],
                false,
                Some(SecretPropertyRef::parse("ca_pem").unwrap()),
                None,
            )
            .unwrap();
            let hosts = ["socket.test".to_string()];
            service(
                EgressPolicy::new(&hosts, &hosts, &[], max_connections)
                    .and_then(|policy| policy.with_tls_profiles([profile]))
                    .unwrap(),
            )
        }

        fn options(&self, port: u16, path: &str) -> ConnectOptions {
            let mut options = options(&format!("wss://socket.test:{port}{path}"));
            options.tls_profile = Some("private-ca".to_string());
            options
        }

        fn client_config(&self, authorization: &AuthorizedEgress) -> Arc<rustls::ClientConfig> {
            build_tls_client_config(
                authorization.tls_profile(),
                &rustls::RootCertStore::empty(),
                |reference| {
                    assert_eq!(reference.as_str(), "ca_pem");
                    Ok(self.ca_pem.clone())
                },
            )
            .unwrap()
        }
    }

    #[test]
    fn reserved_upgrade_headers_are_rejected_case_insensitively() {
        for name in [
            "Host",
            "CONNECTION",
            "Upgrade",
            "Sec-WebSocket-Key",
            "sec-websocket-protocol",
            "Sec-WebSocket-Extensions",
        ] {
            let mut candidate = options("wss://socket.example/path");
            candidate
                .headers
                .push((name.to_string(), "guest".to_string()));
            assert_eq!(
                prepare_connection(candidate).err(),
                Some(WebSocketError::ReservedHeader),
                "{name}"
            );
        }
    }

    #[test]
    fn subprotocols_are_typed_deduplicated_tokens() {
        let mut valid = options("wss://socket.example/path");
        valid.subprotocols = vec!["chat.v2".to_string(), "binary+json".to_string()];
        let prepared = prepare_connection(valid).unwrap();
        assert_eq!(prepared.subprotocols, ["chat.v2", "binary+json"]);
        assert_eq!(
            prepared.request.headers()[SEC_WEBSOCKET_PROTOCOL],
            "chat.v2, binary+json"
        );

        for subprotocols in [
            vec!["with space".to_string()],
            vec!["chat".to_string(), "chat".to_string()],
            vec![String::new()],
        ] {
            let mut invalid = options("wss://socket.example/path");
            invalid.subprotocols = subprotocols;
            assert_eq!(
                prepare_connection(invalid).err(),
                Some(WebSocketError::InvalidSubprotocol)
            );
        }
    }

    #[test]
    fn server_must_select_exactly_one_offered_subprotocol() {
        let offered = vec!["chat.v1".to_string(), "binary.v1".to_string()];
        let mut response = Response::new(None);
        response.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("binary.v1"),
        );
        assert_eq!(
            selected_subprotocol(&response, &offered),
            Ok(Some("binary.v1".to_string()))
        );

        response.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("unoffered.v1"),
        );
        assert_eq!(
            selected_subprotocol(&response, &offered),
            Err(WebSocketError::InvalidSubprotocol)
        );

        response
            .headers_mut()
            .append(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("chat.v1"));
        assert_eq!(
            selected_subprotocol(&response, &offered),
            Err(WebSocketError::InvalidSubprotocol)
        );
    }

    #[test]
    fn upgrade_io_failures_are_not_misreported_as_dial_failures() {
        let error = TungsteniteError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "upgrade reset",
        ));
        assert_eq!(map_handshake_error(&error), WebSocketError::HandshakeFailed);
    }

    #[test]
    fn close_frames_reject_reserved_codes_and_oversized_reasons() {
        assert_eq!(
            WebSocketClose {
                code: 1006,
                reason: String::new(),
            }
            .into_tungstenite()
            .err(),
            Some(WebSocketError::InvalidClose)
        );
        assert_eq!(
            WebSocketClose {
                code: 4000,
                reason: "x".repeat(124),
            }
            .into_tungstenite()
            .err(),
            Some(WebSocketError::InvalidClose)
        );
    }

    #[test]
    fn outbound_payloads_have_a_host_owned_ceiling() {
        assert_eq!(
            validate_message(&WebSocketMessage::Binary(vec![0; MAX_MESSAGE_BYTES + 1])),
            Err(WebSocketError::PayloadTooLarge)
        );
        assert!(validate_message(&WebSocketMessage::Text("x".repeat(MAX_MESSAGE_BYTES))).is_ok());
    }

    #[tokio::test]
    async fn a_ping_gets_exactly_one_pong() {
        let ca = PrivateCa::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = ca.acceptor.clone();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(stream).await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tls).await.unwrap();
            socket
                .send(Message::Ping(
                    tokio_tungstenite::tungstenite::Bytes::from_static(b"beat"),
                ))
                .await
                .unwrap();
            let pong = socket.next().await.unwrap().unwrap();
            assert_eq!(
                pong,
                Message::Pong(tokio_tungstenite::tungstenite::Bytes::from_static(b"beat"))
            );
            let extra = tokio::time::timeout(Duration::from_millis(300), socket.next()).await;
            assert!(
                extra.is_err(),
                "a second frame followed the pong: {extra:?}"
            );
            socket.send(Message::Close(None)).await.unwrap();
        });

        let prepared = prepare_connection(ca.options(address.port(), "/")).unwrap();
        let egress = ca.service(1);
        let authorization = egress
            .authorize_addresses(websocket_request(scope("ping"), &prepared), [address])
            .unwrap();
        let tls_config = ca.client_config(&authorization);
        let (socket, negotiated) = dial_authorized(&prepared, &authorization, Some(tls_config))
            .await
            .unwrap();
        let mut connection = start_connection(socket, negotiated, authorization);
        assert!(matches!(
            connection.events.recv().await,
            Some(WebSocketEvent::Closed(_))
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn dials_only_pinned_addresses_and_holds_an_instance_lease_until_resource_drop() {
        let ca = PrivateCa::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = ca.acceptor.clone();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(stream).await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(tls, PinnedHandshake)
                .await
                .unwrap();

            for _ in 0..2 {
                let message = socket.next().await.unwrap().unwrap();
                socket.send(message).await.unwrap();
            }
            socket
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Normal,
                    reason: Utf8Bytes::from_static("complete"),
                })))
                .await
                .unwrap();
        });

        // `socket.test` need not resolve: the dial can reach only the address
        // pinned by the authorization below.
        let mut connection_options = ca.options(address.port(), "/events");
        connection_options.subprotocols = vec!["json.v1".to_string(), "binary.v1".to_string()];
        let prepared = prepare_connection(connection_options).unwrap();
        let egress = ca.service(1);
        let primary_scope = scope("primary");
        let primary_request = websocket_request(primary_scope.clone(), &prepared);
        let authorization = egress
            .authorize_addresses(primary_request.clone(), [address])
            .unwrap();
        let tls_config = ca.client_config(&authorization);
        let (socket, negotiated) = dial_authorized(&prepared, &authorization, Some(tls_config))
            .await
            .unwrap();
        assert_eq!(negotiated.as_deref(), Some("binary.v1"));
        let mut connection = start_connection(socket, negotiated, authorization);

        connection
            .commands
            .send(ConnectionCommand::Send(WebSocketMessage::Text(
                "hello".to_string(),
            )))
            .await
            .unwrap();
        connection
            .commands
            .send(ConnectionCommand::Send(WebSocketMessage::Binary(vec![
                1, 2, 3,
            ])))
            .await
            .unwrap();

        assert_eq!(
            connection.events.recv().await,
            Some(WebSocketEvent::Message(WebSocketMessage::Text(
                "hello".to_string()
            )))
        );
        assert_eq!(
            connection.events.recv().await,
            Some(WebSocketEvent::Message(WebSocketMessage::Binary(vec![
                1, 2, 3
            ])))
        );
        assert_eq!(
            connection.events.recv().await,
            Some(WebSocketEvent::Closed(Some(WebSocketClose {
                code: 1000,
                reason: "complete".to_string(),
            })))
        );
        // Past the terminal event, polling reports closed rather than idle.
        let after_terminal = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match connection.try_receive() {
                    Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
                    other => break other,
                }
            }
        })
        .await
        .expect("socket task ends after its terminal event");
        assert_eq!(after_terminal, Err(WebSocketError::Closed));

        assert!(matches!(
            egress.authorize_addresses(primary_request.clone(), [address]),
            Err(EgressError::ConnectionLimitReached { .. })
        ));
        let secondary_request = websocket_request(scope("secondary"), &prepared);
        let secondary = egress
            .authorize_addresses(secondary_request, [address])
            .unwrap();
        drop(secondary);

        drop(connection);
        assert!(
            egress
                .authorize_addresses(primary_request, [address])
                .is_ok()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn secure_websockets_use_named_custom_ca_profiles_with_original_host_sni() {
        let ca = PrivateCa::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = ca.acceptor.clone();
        let server = zeroclaw_spawn::spawn!(async move {
            let (untrusted_stream, _) = listener.accept().await.unwrap();
            assert!(acceptor.accept(untrusted_stream).await.is_err());

            let (trusted_stream, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(trusted_stream).await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tls).await.unwrap();
            socket
                .send(Message::Text("secure".to_string().into()))
                .await
                .unwrap();
        });

        let egress = ca.service(1);
        let prepared = prepare_connection(ca.options(address.port(), "/secure")).unwrap();
        let untrusted_authorization = egress
            .authorize_addresses(websocket_request(scope("secure"), &prepared), [address])
            .unwrap();
        let system_config = build_tls_client_config(None, &rustls::RootCertStore::empty(), |_| {
            panic!("implicit system roots must not resolve plugin secrets")
        })
        .unwrap();
        assert_eq!(
            dial_authorized(&prepared, &untrusted_authorization, Some(system_config))
                .await
                .err(),
            Some(WebSocketError::TlsFailed)
        );
        drop(untrusted_authorization);

        let authorization = egress
            .authorize_addresses(websocket_request(scope("secure"), &prepared), [address])
            .unwrap();
        let tls_config = ca.client_config(&authorization);
        let (socket, negotiated) = dial_authorized(&prepared, &authorization, Some(tls_config))
            .await
            .unwrap();
        let mut connection = start_connection(socket, negotiated, authorization);

        assert_eq!(
            connection.events.recv().await,
            Some(WebSocketEvent::Message(WebSocketMessage::Text(
                "secure".to_string()
            )))
        );
        drop(connection);
        server.await.unwrap();
    }
}
