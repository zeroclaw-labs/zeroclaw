//! Host-mediated TCP, direct TLS, and STARTTLS for plugin components.
//!
//! The adapter has no destination policy of its own. Every connection begins
//! with the shared [`EgressHostService`](crate::egress::EgressHostService),
//! dials only its pinned addresses, and retains the resulting
//! [`AuthorizedEgress`] for the complete Wasmtime resource lifetime. A single
//! actor owns each live stream, which makes plaintext/TLS transitions and
//! shutdown ordering authoritative in one place.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::AbortHandle;
use tokio_rustls::client::TlsStream;
use wasmtime::component::Resource;

use crate::component::{PluginState, bindings};
use crate::egress::{
    AuthorizedEgress, EgressError, EgressRequest, EgressTransport, StartTlsState,
};

/// Maximum bytes accepted from one guest send or retained in one read chunk.
const MAX_CHUNK_BYTES: usize = 16 * 1024;
/// Maximum unread chunks retained per connection before TCP backpressure.
const INBOUND_CAPACITY: usize = 64;
/// Maximum pending actor commands. A full queue fails fast at the guest call.
const COMMAND_CAPACITY: usize = 64;
/// Bound for DNS-pinned connect attempts and each TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Adapter-internal form of the WIT connect mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectMode {
    Plaintext,
    DirectTls,
    StartTls,
}

impl ConnectMode {
    fn egress_transport(self) -> EgressTransport {
        match self {
            Self::Plaintext => EgressTransport::Tcp,
            Self::DirectTls => EgressTransport::Tls,
            Self::StartTls => EgressTransport::StartTls,
        }
    }
}

/// Stable adapter failure categories mapped directly onto each WIT world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SocketFailure {
    AccessDenied,
    InvalidRequest,
    ResolutionFailed,
    ConnectionLimit,
    ConnectFailed,
    TlsConfigurationFailed,
    TlsHandshakeFailed,
    InvalidState,
    Closed,
    Backpressure,
    ChunkTooLarge,
    HostUnavailable,
}

/// Terminal outcome retained after the actor releases its stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SocketCloseReason {
    PeerClosed,
    IoError,
    HostClosed,
    TlsUpgradeFailed,
}

/// Whether bytes crossed the stream before or after mandatory TLS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrafficClass {
    Negotiation,
    Application,
}

#[derive(Debug)]
struct BufferedChunk {
    class: TrafficClass,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct InboundBuffer {
    chunks: Mutex<VecDeque<BufferedChunk>>,
    drained: Notify,
}

impl InboundBuffer {
    fn is_full(&self) -> bool {
        lock(&self.chunks).len() >= INBOUND_CAPACITY
    }

    fn is_empty(&self) -> bool {
        lock(&self.chunks).is_empty()
    }

    fn push(&self, chunk: BufferedChunk) {
        lock(&self.chunks).push_back(chunk);
    }

    fn pop(&self, expected: TrafficClass) -> Result<Option<Vec<u8>>, SocketFailure> {
        let mut chunks = lock(&self.chunks);
        let Some(front) = chunks.front() else {
            return Ok(None);
        };
        if front.class != expected {
            return Err(SocketFailure::InvalidState);
        }
        let bytes = chunks.pop_front().map(|chunk| chunk.bytes);
        drop(chunks);
        self.drained.notify_one();
        Ok(bytes)
    }

    fn len(&self) -> u32 {
        u32::try_from(lock(&self.chunks).len()).unwrap_or(u32::MAX)
    }
}

/// Host-side receive result before conversion into one generated WIT type.
enum SocketReceive {
    Data(Vec<u8>),
    Idle,
    Closed(SocketCloseReason),
}

enum ActorCommand {
    Send {
        class: TrafficClass,
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<(), SocketFailure>>,
    },
    UpgradeTls {
        config: Arc<rustls::ClientConfig>,
        server_name: ServerName<'static>,
        reply: oneshot::Sender<Result<(), SocketFailure>>,
    },
    Close,
}

/// The actor's single authoritative live transport state.
enum ActorStream {
    Plaintext(TcpStream),
    StartTls {
        stream: Option<TcpStream>,
        state: StartTlsState,
    },
    Tls(TlsStream<TcpStream>),
    Closed,
}

impl ActorStream {
    fn traffic_class(&self) -> Result<TrafficClass, SocketFailure> {
        match self {
            Self::Plaintext(_) | Self::Tls(_) => Ok(TrafficClass::Application),
            Self::StartTls { state, .. } if state.plaintext_negotiation_allowed() => {
                Ok(TrafficClass::Negotiation)
            }
            Self::StartTls { .. } | Self::Closed => Err(SocketFailure::Closed),
        }
    }

    fn permits(&self, class: TrafficClass) -> bool {
        match (self, class) {
            (Self::Plaintext(_) | Self::Tls(_), TrafficClass::Application) => true,
            (Self::StartTls { state, .. }, TrafficClass::Negotiation) => {
                state.plaintext_negotiation_allowed()
            }
            _ => false,
        }
    }

    async fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plaintext(stream) => stream.read(bytes).await,
            Self::StartTls {
                stream: Some(stream),
                ..
            } => stream.read(bytes).await,
            Self::Tls(stream) => stream.read(bytes).await,
            Self::StartTls { stream: None, .. } | Self::Closed => Ok(0),
        }
    }

    async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Plaintext(stream) => stream.write_all(bytes).await,
            Self::StartTls {
                stream: Some(stream),
                ..
            } => stream.write_all(bytes).await,
            Self::Tls(stream) => stream.write_all(bytes).await,
            Self::StartTls { stream: None, .. } | Self::Closed => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "plugin socket is closed",
            )),
        }
    }

    async fn shutdown(&mut self) {
        let _ = match self {
            Self::Plaintext(stream) => stream.shutdown().await,
            Self::StartTls {
                stream: Some(stream),
                ..
            } => stream.shutdown().await,
            Self::Tls(stream) => stream.shutdown().await,
            Self::StartTls { stream: None, .. } | Self::Closed => Ok(()),
        };
    }

    async fn upgrade_tls(
        &mut self,
        config: Arc<rustls::ClientConfig>,
        server_name: ServerName<'static>,
    ) -> Result<(), SocketFailure> {
        let current = std::mem::replace(self, Self::Closed);
        let Self::StartTls {
            stream: Some(stream),
            mut state,
        } = current
        else {
            *self = current;
            return Err(SocketFailure::InvalidState);
        };
        state
            .begin_upgrade()
            .map_err(|_| SocketFailure::InvalidState)?;

        let connector = tokio_rustls::TlsConnector::from(config);
        match tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(server_name, stream)).await {
            Ok(Ok(stream)) => {
                state
                    .complete_upgrade()
                    .map_err(|_| SocketFailure::InvalidState)?;
                *self = Self::Tls(stream);
                Ok(())
            }
            Ok(Err(_)) | Err(_) => {
                let _ = state.fail_upgrade();
                Err(SocketFailure::TlsHandshakeFailed)
            }
        }
    }
}

/// One Wasmtime-owned connection resource.
///
/// `authorization` is the sole retained policy token and shared-budget lease;
/// it is never reconstructed from guest input. The actor owns the stream, while
/// this resource owns its queues and cancellation handle.
pub struct SocketConnection {
    authorization: Arc<AuthorizedEgress>,
    commands: mpsc::Sender<ActorCommand>,
    inbound: Arc<InboundBuffer>,
    terminal: Arc<Mutex<Option<SocketCloseReason>>>,
    actor: AbortHandle,
}

impl SocketConnection {
    async fn send(
        &self,
        class: TrafficClass,
        bytes: Vec<u8>,
    ) -> Result<(), SocketFailure> {
        if bytes.len() > MAX_CHUNK_BYTES {
            return Err(SocketFailure::ChunkTooLarge);
        }
        if lock(&self.terminal).is_some() {
            return Err(SocketFailure::Closed);
        }
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(ActorCommand::Send {
                class,
                bytes,
                reply,
            })
            .map_err(map_command_send_error)?;
        result.await.unwrap_or(Err(SocketFailure::Closed))
    }

    fn receive(&self, class: TrafficClass) -> Result<SocketReceive, SocketFailure> {
        if let Some(bytes) = self.inbound.pop(class)? {
            return Ok(SocketReceive::Data(bytes));
        }
        Ok(match *lock(&self.terminal) {
            Some(reason) => SocketReceive::Closed(reason),
            None => SocketReceive::Idle,
        })
    }

    fn pending(&self) -> u32 {
        self.inbound.len()
    }

    fn authorization(&self) -> Arc<AuthorizedEgress> {
        Arc::clone(&self.authorization)
    }

    fn command_sender(&self) -> mpsc::Sender<ActorCommand> {
        self.commands.clone()
    }

    fn ready_for_upgrade(&self) -> bool {
        self.inbound.is_empty() && lock(&self.terminal).is_none()
    }
}

impl Drop for SocketConnection {
    fn drop(&mut self) {
        let _ = self.commands.try_send(ActorCommand::Close);
        self.actor.abort();
    }
}

fn map_command_send_error(error: mpsc::error::TrySendError<ActorCommand>) -> SocketFailure {
    match error {
        mpsc::error::TrySendError::Full(_) => SocketFailure::Backpressure,
        mpsc::error::TrySendError::Closed(_) => SocketFailure::Closed,
    }
}

async fn connect_pinned(authorized: &AuthorizedEgress) -> Result<TcpStream, SocketFailure> {
    let addresses = authorized.destination().addresses();
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addresses))
        .await
        .map_err(|_| SocketFailure::ConnectFailed)?
        .map_err(|_| SocketFailure::ConnectFailed)?;
    stream
        .set_nodelay(true)
        .map_err(|_| SocketFailure::ConnectFailed)?;
    Ok(stream)
}

async fn connect_direct_tls(
    authorized: &AuthorizedEgress,
    config: Arc<rustls::ClientConfig>,
) -> Result<TlsStream<TcpStream>, SocketFailure> {
    let stream = connect_pinned(authorized).await?;
    let server_name = tls_server_name(authorized)?;
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_rustls::TlsConnector::from(config).connect(server_name, stream),
    )
    .await
    .map_err(|_| SocketFailure::TlsHandshakeFailed)?
    .map_err(|_| SocketFailure::TlsHandshakeFailed)
}

fn tls_server_name(authorized: &AuthorizedEgress) -> Result<ServerName<'static>, SocketFailure> {
    ServerName::try_from(authorized.request().host().to_string())
        .map_err(|_| SocketFailure::InvalidRequest)
}

fn spawn_connection(
    authorization: Arc<AuthorizedEgress>,
    stream: ActorStream,
) -> SocketConnection {
    let inbound = Arc::new(InboundBuffer::default());
    let terminal = Arc::new(Mutex::new(None));
    let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let actor_inbound = Arc::clone(&inbound);
    let actor_terminal = Arc::clone(&terminal);
    let actor = zeroclaw_spawn::spawn!(async move {
        connection_actor(stream, command_rx, actor_inbound, actor_terminal).await;
    });
    SocketConnection {
        authorization,
        commands,
        inbound,
        terminal,
        actor: actor.abort_handle(),
    }
}

async fn connection_actor(
    mut stream: ActorStream,
    mut commands: mpsc::Receiver<ActorCommand>,
    inbound: Arc<InboundBuffer>,
    terminal: Arc<Mutex<Option<SocketCloseReason>>>,
) {
    let reason = loop {
        if inbound.is_full() {
            tokio::select! {
                command = commands.recv() => {
                    if let Some(reason) = handle_command(command, &mut stream).await {
                        break reason;
                    }
                }
                () = inbound.drained.notified() => {}
            }
            continue;
        }

        let mut bytes = vec![0_u8; MAX_CHUNK_BYTES];
        tokio::select! {
            command = commands.recv() => {
                if let Some(reason) = handle_command(command, &mut stream).await {
                    break reason;
                }
            }
            read = stream.read(&mut bytes) => {
                match read {
                    Ok(0) => break SocketCloseReason::PeerClosed,
                    Ok(count) => {
                        let Ok(class) = stream.traffic_class() else {
                            break SocketCloseReason::IoError;
                        };
                        bytes.truncate(count);
                        inbound.push(BufferedChunk { class, bytes });
                    }
                    Err(_) => break SocketCloseReason::IoError,
                }
            }
        }
    };
    stream.shutdown().await;
    let mut terminal = lock(&terminal);
    if terminal.is_none() {
        *terminal = Some(reason);
    }
}

async fn handle_command(
    command: Option<ActorCommand>,
    stream: &mut ActorStream,
) -> Option<SocketCloseReason> {
    match command {
        Some(ActorCommand::Send {
            class,
            bytes,
            reply,
        }) => {
            if !stream.permits(class) {
                let _ = reply.send(Err(SocketFailure::InvalidState));
                return None;
            }
            match stream.write_all(&bytes).await {
                Ok(()) => {
                    let _ = reply.send(Ok(()));
                    None
                }
                Err(_) => {
                    let _ = reply.send(Err(SocketFailure::Closed));
                    Some(SocketCloseReason::IoError)
                }
            }
        }
        Some(ActorCommand::UpgradeTls {
            config,
            server_name,
            reply,
        }) => match stream.upgrade_tls(config, server_name).await {
            Ok(()) => {
                let _ = reply.send(Ok(()));
                None
            }
            Err(SocketFailure::TlsHandshakeFailed) => {
                let _ = reply.send(Err(SocketFailure::TlsHandshakeFailed));
                Some(SocketCloseReason::TlsUpgradeFailed)
            }
            Err(error) => {
                let _ = reply.send(Err(error));
                None
            }
        },
        Some(ActorCommand::Close) | None => Some(SocketCloseReason::HostClosed),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn map_egress_error(error: &EgressError) -> SocketFailure {
    match error {
        EgressError::PermissionDenied { .. }
        | EgressError::PlaintextDenied { .. }
        | EgressError::TlsProfileHostDenied { .. }
        | EgressError::Network(_) => SocketFailure::AccessDenied,
        EgressError::DnsFailed { .. } => SocketFailure::ResolutionFailed,
        EgressError::ConnectionLimitReached { .. } => SocketFailure::ConnectionLimit,
        EgressError::InvalidTlsMaterial { .. } | EgressError::TlsSecretUnavailable { .. } => {
            SocketFailure::TlsConfigurationFailed
        }
        EgressError::PolicyUnavailable(_) => SocketFailure::HostUnavailable,
        EgressError::InvalidStartTlsTransition { .. } => SocketFailure::InvalidState,
        EgressError::InvalidHostPattern(_)
        | EgressError::InvalidSecretReference(_)
        | EgressError::InvalidTlsProfileName(_)
        | EgressError::InvalidTlsProfile { .. }
        | EgressError::DuplicateTlsProfile(_)
        | EgressError::TlsProfileWithoutHosts(_)
        | EgressError::InvalidConnectionLimit
        | EgressError::TlsProfileOnPlaintext(_)
        | EgressError::UnknownTlsProfile(_)
        | EgressError::AuthorizationScopeMismatch => SocketFailure::InvalidRequest,
    }
}

async fn connect(
    state: &mut PluginState,
    host: String,
    port: u16,
    mode: ConnectMode,
    tls_profile: Option<String>,
) -> Result<Resource<SocketConnection>, SocketFailure> {
    if !state.charge_host_call() {
        return Err(SocketFailure::HostUnavailable);
    }
    let request = EgressRequest::new(
        state.scope().clone(),
        mode.egress_transport(),
        &host,
        port,
        tls_profile.as_deref(),
    )
    .map_err(|error| map_egress_error(&error))?;
    let service = state.egress().clone();
    let authorization = Arc::new(
        service
            .authorize(request)
            .await
            .map_err(|error| map_egress_error(&error))?,
    );
    let stream = match mode {
        ConnectMode::Plaintext => ActorStream::Plaintext(connect_pinned(&authorization).await?),
        ConnectMode::DirectTls => {
            let config = state
                .tls_client_config(&authorization)
                .map_err(|error| map_egress_error(&error))?;
            ActorStream::Tls(connect_direct_tls(&authorization, config).await?)
        }
        ConnectMode::StartTls => ActorStream::StartTls {
            stream: Some(connect_pinned(&authorization).await?),
            state: StartTlsState::new(),
        },
    };
    let connection = spawn_connection(authorization, stream);
    state
        .resource_table_mut()
        .push(connection)
        .map_err(|_| SocketFailure::HostUnavailable)
}

async fn send(
    state: &mut PluginState,
    resource: Resource<SocketConnection>,
    class: TrafficClass,
    bytes: Vec<u8>,
) -> Result<(), SocketFailure> {
    if !state.charge_host_call() {
        return Err(SocketFailure::HostUnavailable);
    }
    let connection = state
        .resource_table()
        .get(&resource)
        .map_err(|_| SocketFailure::Closed)?;
    connection.send(class, bytes).await
}

fn receive(
    state: &mut PluginState,
    resource: Resource<SocketConnection>,
    class: TrafficClass,
) -> Result<SocketReceive, SocketFailure> {
    if !state.charge_host_call() {
        return Err(SocketFailure::HostUnavailable);
    }
    state
        .resource_table()
        .get(&resource)
        .map_err(|_| SocketFailure::Closed)?
        .receive(class)
}

fn pending(
    state: &mut PluginState,
    resource: Resource<SocketConnection>,
) -> Result<u32, SocketFailure> {
    if !state.charge_host_call() {
        return Err(SocketFailure::HostUnavailable);
    }
    Ok(state
        .resource_table()
        .get(&resource)
        .map_err(|_| SocketFailure::Closed)?
        .pending())
}

async fn upgrade_tls(
    state: &mut PluginState,
    resource: Resource<SocketConnection>,
) -> Result<(), SocketFailure> {
    if !state.charge_host_call() {
        return Err(SocketFailure::HostUnavailable);
    }
    let (authorization, commands) = {
        let connection = state
            .resource_table()
            .get(&resource)
            .map_err(|_| SocketFailure::Closed)?;
        if !connection.ready_for_upgrade() {
            return Err(SocketFailure::InvalidState);
        }
        (connection.authorization(), connection.command_sender())
    };
    let config = state
        .tls_client_config(&authorization)
        .map_err(|error| map_egress_error(&error))?;
    let server_name = tls_server_name(&authorization)?;
    let (reply, result) = oneshot::channel();
    commands
        .try_send(ActorCommand::UpgradeTls {
            config,
            server_name,
            reply,
        })
        .map_err(map_command_send_error)?;
    result.await.unwrap_or(Err(SocketFailure::Closed))
}

fn close(state: &mut PluginState, resource: Resource<SocketConnection>) {
    if let Ok(connection) = state.resource_table_mut().delete(resource) {
        drop(connection);
    }
}

macro_rules! map_failure {
    ($world:ident, $error:expr) => {{
        use bindings::$world::zeroclaw::plugin::sockets::SocketError;
        match $error {
            SocketFailure::AccessDenied => SocketError::AccessDenied,
            SocketFailure::InvalidRequest => SocketError::InvalidRequest,
            SocketFailure::ResolutionFailed => SocketError::ResolutionFailed,
            SocketFailure::ConnectionLimit => SocketError::ConnectionLimit,
            SocketFailure::ConnectFailed => SocketError::ConnectFailed,
            SocketFailure::TlsConfigurationFailed => SocketError::TlsConfigurationFailed,
            SocketFailure::TlsHandshakeFailed => SocketError::TlsHandshakeFailed,
            SocketFailure::InvalidState => SocketError::InvalidState,
            SocketFailure::Closed => SocketError::Closed,
            SocketFailure::Backpressure => SocketError::Backpressure,
            SocketFailure::ChunkTooLarge => SocketError::ChunkTooLarge,
            SocketFailure::HostUnavailable => SocketError::HostUnavailable,
        }
    }};
}

macro_rules! map_receive {
    ($world:ident, $event:expr) => {{
        use bindings::$world::zeroclaw::plugin::sockets::{CloseReason, ReceiveEvent};
        match $event {
            SocketReceive::Data(bytes) => ReceiveEvent::Data(bytes),
            SocketReceive::Idle => ReceiveEvent::Idle,
            SocketReceive::Closed(reason) => ReceiveEvent::Closed(match reason {
                SocketCloseReason::PeerClosed => CloseReason::PeerClosed,
                SocketCloseReason::IoError => CloseReason::IoError,
                SocketCloseReason::HostClosed => CloseReason::HostClosed,
                SocketCloseReason::TlsUpgradeFailed => CloseReason::TlsUpgradeFailed,
            }),
        }
    }};
}

macro_rules! impl_socket_host {
    ($world:ident) => {
        impl bindings::$world::zeroclaw::plugin::sockets::HostConnection for PluginState {
            async fn send(
                &mut self,
                self_: Resource<SocketConnection>,
                bytes: Vec<u8>,
            ) -> Result<(), bindings::$world::zeroclaw::plugin::sockets::SocketError> {
                send(self, self_, TrafficClass::Application, bytes)
                    .await
                    .map_err(|error| map_failure!($world, error))
            }

            async fn receive(
                &mut self,
                self_: Resource<SocketConnection>,
            ) -> Result<bindings::$world::zeroclaw::plugin::sockets::ReceiveEvent, bindings::$world::zeroclaw::plugin::sockets::SocketError> {
                receive(self, self_, TrafficClass::Application)
                    .map(|event| map_receive!($world, event))
                    .map_err(|error| map_failure!($world, error))
            }

            async fn send_negotiation(
                &mut self,
                self_: Resource<SocketConnection>,
                bytes: Vec<u8>,
            ) -> Result<(), bindings::$world::zeroclaw::plugin::sockets::SocketError> {
                send(self, self_, TrafficClass::Negotiation, bytes)
                    .await
                    .map_err(|error| map_failure!($world, error))
            }

            async fn receive_negotiation(
                &mut self,
                self_: Resource<SocketConnection>,
            ) -> Result<bindings::$world::zeroclaw::plugin::sockets::ReceiveEvent, bindings::$world::zeroclaw::plugin::sockets::SocketError> {
                receive(self, self_, TrafficClass::Negotiation)
                    .map(|event| map_receive!($world, event))
                    .map_err(|error| map_failure!($world, error))
            }

            async fn pending(
                &mut self,
                self_: Resource<SocketConnection>,
            ) -> Result<u32, bindings::$world::zeroclaw::plugin::sockets::SocketError> {
                pending(self, self_).map_err(|error| map_failure!($world, error))
            }

            async fn upgrade_tls(
                &mut self,
                self_: Resource<SocketConnection>,
            ) -> Result<(), bindings::$world::zeroclaw::plugin::sockets::SocketError> {
                upgrade_tls(self, self_)
                    .await
                    .map_err(|error| map_failure!($world, error))
            }

            async fn drop(
                &mut self,
                resource: Resource<SocketConnection>,
            ) -> wasmtime::Result<()> {
                close(self, resource);
                Ok(())
            }
        }

        impl bindings::$world::zeroclaw::plugin::sockets::Host for PluginState {
            async fn connect(
                &mut self,
                request: bindings::$world::zeroclaw::plugin::sockets::ConnectRequest,
            ) -> Result<Resource<SocketConnection>, bindings::$world::zeroclaw::plugin::sockets::SocketError> {
                use bindings::$world::zeroclaw::plugin::sockets::ConnectMode;
                let mode = match request.mode {
                    ConnectMode::Plaintext => ConnectMode::Plaintext,
                    ConnectMode::DirectTls => ConnectMode::DirectTls,
                    ConnectMode::StartTls => ConnectMode::StartTls,
                };
                let mode = match mode {
                    ConnectMode::Plaintext => self::ConnectMode::Plaintext,
                    ConnectMode::DirectTls => self::ConnectMode::DirectTls,
                    ConnectMode::StartTls => self::ConnectMode::StartTls,
                };
                connect(self, request.host, request.port, mode, request.tls_profile)
                    .await
                    .map_err(|error| map_failure!($world, error))
            }

            async fn close(&mut self, connection: Resource<SocketConnection>) {
                close(self, connection);
            }
        }
    };
}

impl_socket_host!(tool);
impl_socket_host!(channel);
