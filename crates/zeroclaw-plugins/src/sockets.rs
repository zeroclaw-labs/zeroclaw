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

use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::AbortHandle;
use tokio_rustls::client::TlsStream;
use wasmtime::component::Resource;

use crate::component::{PluginState, bindings};
use crate::egress::{
    AuthorizedEgress, EGRESS_CONNECT_DEADLINE, EgressError, EgressRequest, EgressTransport,
    StartTlsState,
};
use zeroclaw_infra::net_guard::NetworkGuardError;

/// Maximum bytes accepted from one guest send or retained in one read chunk.
const MAX_CHUNK_BYTES: usize = 16 * 1024;
/// Maximum unread chunks retained per connection before TCP backpressure.
const INBOUND_CAPACITY: usize = 64;
/// Maximum pending actor commands. A full queue fails fast at the guest call.
const COMMAND_CAPACITY: usize = 64;
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
        match tokio::time::timeout(
            EGRESS_CONNECT_DEADLINE,
            connector.connect(server_name, stream),
        )
        .await
        {
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
    async fn send(&self, class: TrafficClass, bytes: Vec<u8>) -> Result<(), SocketFailure> {
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
    let stream = tokio::time::timeout(EGRESS_CONNECT_DEADLINE, TcpStream::connect(addresses))
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
        EGRESS_CONNECT_DEADLINE,
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

fn spawn_connection(authorization: Arc<AuthorizedEgress>, stream: ActorStream) -> SocketConnection {
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
                    if let Some(reason) = handle_command(command, &mut stream, &inbound).await {
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
                if let Some(reason) = handle_command(command, &mut stream, &inbound).await {
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
    inbound: &InboundBuffer,
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
        Some(ActorCommand::UpgradeTls { reply, .. }) if !inbound.is_empty() => {
            let _ = reply.send(Err(SocketFailure::InvalidState));
            None
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
        | EgressError::AuthorizationScopeMismatch => SocketFailure::AccessDenied,
        EgressError::Network(
            NetworkGuardError::InvalidHost(_) | NetworkGuardError::InvalidPort,
        ) => SocketFailure::InvalidRequest,
        EgressError::Network(NetworkGuardError::NoAddresses { .. }) => {
            SocketFailure::ResolutionFailed
        }
        EgressError::Network(_) => SocketFailure::AccessDenied,
        EgressError::DnsFailed { .. } => SocketFailure::ResolutionFailed,
        EgressError::ConnectionLimitReached { .. } => SocketFailure::ConnectionLimit,
        EgressError::InvalidTlsMaterial { .. } | EgressError::TlsSecretUnavailable { .. } => {
            SocketFailure::TlsConfigurationFailed
        }
        EgressError::PolicyUnavailable(_)
        | EgressError::InvalidHostPattern(_)
        | EgressError::InvalidSecretReference(_)
        | EgressError::InvalidTlsProfile { .. }
        | EgressError::DuplicateTlsProfile(_)
        | EgressError::TlsProfileWithoutHosts(_)
        | EgressError::InvalidConnectionLimit => SocketFailure::HostUnavailable,
        EgressError::InvalidStartTlsTransition { .. } => SocketFailure::InvalidState,
        EgressError::InvalidTlsProfileName(_)
        | EgressError::TlsProfileOnPlaintext(_)
        | EgressError::UnknownTlsProfile(_) => SocketFailure::InvalidRequest,
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
    let service = state.egress_service();
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

async fn receive(
    state: &mut PluginState,
    resource: Resource<SocketConnection>,
    class: TrafficClass,
) -> Result<SocketReceive, SocketFailure> {
    if !state.charge_host_call() {
        return Err(SocketFailure::HostUnavailable);
    }
    let event = state
        .resource_table()
        .get(&resource)
        .map_err(|_| SocketFailure::Closed)?
        .receive(class)?;
    if matches!(event, SocketReceive::Idle) {
        tokio::task::yield_now().await;
    }
    Ok(event)
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
            ) -> Result<
                bindings::$world::zeroclaw::plugin::sockets::ReceiveEvent,
                bindings::$world::zeroclaw::plugin::sockets::SocketError,
            > {
                receive(self, self_, TrafficClass::Application)
                    .await
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
            ) -> Result<
                bindings::$world::zeroclaw::plugin::sockets::ReceiveEvent,
                bindings::$world::zeroclaw::plugin::sockets::SocketError,
            > {
                receive(self, self_, TrafficClass::Negotiation)
                    .await
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

            async fn drop(&mut self, resource: Resource<SocketConnection>) -> wasmtime::Result<()> {
                close(self, resource);
                Ok(())
            }
        }

        impl bindings::$world::zeroclaw::plugin::sockets::Host for PluginState {
            async fn connect(
                &mut self,
                request: bindings::$world::zeroclaw::plugin::sockets::ConnectRequest,
            ) -> Result<
                Resource<SocketConnection>,
                bindings::$world::zeroclaw::plugin::sockets::SocketError,
            > {
                use bindings::$world::zeroclaw::plugin::sockets::ConnectMode as WitConnectMode;
                let mode = match request.mode {
                    WitConnectMode::Plaintext => ConnectMode::Plaintext,
                    WitConnectMode::DirectTls => ConnectMode::DirectTls,
                    WitConnectMode::StartTls => ConnectMode::StartTls,
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::egress::{
        EgressHostService, EgressPolicy, EgressPolicyResolver, SecretPropertyRef,
        TlsClientIdentity, TlsProfile, TlsProfileName, build_tls_client_config,
    };
    use crate::{PluginCapability, PluginPermission};

    struct TestPki {
        ca_pem: String,
        ca_der: rustls::pki_types::CertificateDer<'static>,
        server_der: rustls::pki_types::CertificateDer<'static>,
        server_key: PrivateKeyDer<'static>,
        client_pem: String,
        client_key_pem: String,
    }

    impl TestPki {
        fn new() -> Self {
            let ca_key = rcgen::KeyPair::generate().expect("generate test CA key");
            let mut ca_parameters =
                rcgen::CertificateParams::new(vec!["Plugin Socket Test CA".to_string()])
                    .expect("test CA parameters");
            ca_parameters.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let ca = ca_parameters
                .self_signed(&ca_key)
                .expect("self-sign test CA");

            let server_key = rcgen::KeyPair::generate().expect("generate server key");
            let server_parameters = rcgen::CertificateParams::new(vec!["localhost".to_string()])
                .expect("server parameters");
            let server = server_parameters
                .signed_by(&server_key, &ca, &ca_key)
                .expect("sign server certificate");

            let client_key = rcgen::KeyPair::generate().expect("generate client key");
            let client_parameters =
                rcgen::CertificateParams::new(vec!["plugin-client".to_string()])
                    .expect("client parameters");
            let client = client_parameters
                .signed_by(&client_key, &ca, &ca_key)
                .expect("sign client certificate");

            Self {
                ca_pem: ca.pem(),
                ca_der: ca.der().clone(),
                server_der: server.der().clone(),
                server_key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                    server_key.serialize_der(),
                )),
                client_pem: client.pem(),
                client_key_pem: client_key.serialize_pem(),
            }
        }

        fn server_config(&self, require_client: bool) -> Arc<rustls::ServerConfig> {
            let builder = rustls::ServerConfig::builder();
            let builder = if require_client {
                let mut roots = rustls::RootCertStore::empty();
                roots
                    .add(self.ca_der.clone())
                    .expect("trust test client CA");
                let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                    .build()
                    .expect("build client verifier");
                builder.with_client_cert_verifier(verifier)
            } else {
                builder.with_no_client_auth()
            };
            Arc::new(
                builder
                    .with_single_cert(vec![self.server_der.clone()], self.server_key.clone_key())
                    .expect("build test TLS server"),
            )
        }

        fn profile(&self) -> TlsProfile {
            TlsProfile::new(
                TlsProfileName::new("test-mtls").expect("profile name"),
                ["localhost".to_string()],
                false,
                Some(SecretPropertyRef::new("ca_pem").expect("CA reference")),
                Some(TlsClientIdentity::new(
                    SecretPropertyRef::new("client_cert_pem").expect("certificate reference"),
                    SecretPropertyRef::new("client_key_pem").expect("key reference"),
                )),
            )
            .expect("valid mTLS profile")
        }

        fn client_config(&self, profile: Option<&TlsProfile>) -> Arc<rustls::ClientConfig> {
            build_tls_client_config(profile, |reference| match reference.as_str() {
                "ca_pem" => Ok(self.ca_pem.clone()),
                "client_cert_pem" => Ok(self.client_pem.clone()),
                "client_key_pem" => Ok(self.client_key_pem.clone()),
                property => Err(EgressError::TlsSecretUnavailable {
                    profile: "test-mtls".to_string(),
                    property: property.to_string(),
                }),
            })
            .expect("build test client config")
        }
    }

    fn scope(binding: &str) -> crate::instance::PluginInstanceScope {
        crate::instance::test_scope(
            PluginCapability::Channel,
            binding,
            [
                PluginPermission::SocketClient,
                PluginPermission::WebSocketClient,
            ],
        )
    }

    fn service(
        private_hosts: impl IntoIterator<Item = &'static str>,
        plaintext_hosts: impl IntoIterator<Item = &'static str>,
        profiles: impl IntoIterator<Item = TlsProfile>,
        max_connections: usize,
    ) -> EgressHostService {
        let private_hosts = private_hosts
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let plaintext_hosts = plaintext_hosts
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let profiles = profiles.into_iter().collect::<Vec<_>>();
        EgressHostService::new(EgressPolicyResolver::new(move |_| {
            EgressPolicy::new(
                private_hosts.clone(),
                plaintext_hosts.clone(),
                profiles.clone(),
                max_connections,
            )
        }))
    }

    fn authorize(
        service: &EgressHostService,
        binding: &str,
        transport: EgressTransport,
        host: &str,
        address: SocketAddr,
        profile: Option<&str>,
    ) -> AuthorizedEgress {
        let request = EgressRequest::new(scope(binding), transport, host, address.port(), profile)
            .expect("valid test request");
        service
            .authorize_addresses(request, [address])
            .expect("test request is authorized")
    }

    async fn start_plain_echo() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind plaintext echo");
        let address = listener.local_addr().expect("echo address");
        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.expect("accept plaintext client");
            let mut bytes = [0_u8; MAX_CHUNK_BYTES];
            loop {
                match stream.read(&mut bytes).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => stream
                        .write_all(&bytes[..count])
                        .await
                        .expect("echo plaintext bytes"),
                }
            }
        });
        address
    }

    async fn start_tls_echo(config: Arc<rustls::ServerConfig>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind TLS echo");
        let address = listener.local_addr().expect("TLS echo address");
        zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.expect("accept TLS client");
            let Ok(mut stream) = tokio_rustls::TlsAcceptor::from(config).accept(stream).await
            else {
                return;
            };
            let mut bytes = [0_u8; MAX_CHUNK_BYTES];
            loop {
                match stream.read(&mut bytes).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => stream
                        .write_all(&bytes[..count])
                        .await
                        .expect("echo TLS bytes"),
                }
            }
        });
        address
    }

    async fn start_starttls_echo(config: Arc<rustls::ServerConfig>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind STARTTLS echo");
        let address = listener.local_addr().expect("STARTTLS echo address");
        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.expect("accept STARTTLS client");
            stream
                .write_all(b"220 service ready\r\n")
                .await
                .expect("write greeting");
            let mut command = Vec::new();
            loop {
                let byte = stream.read_u8().await.expect("read STARTTLS command");
                command.push(byte);
                if command.ends_with(b"\r\n") {
                    break;
                }
            }
            assert_eq!(command, b"STARTTLS\r\n");
            stream
                .write_all(b"220 begin TLS\r\n")
                .await
                .expect("write upgrade response");
            let mut stream = tokio_rustls::TlsAcceptor::from(config)
                .accept(stream)
                .await
                .expect("accept upgraded TLS");
            let mut bytes = [0_u8; MAX_CHUNK_BYTES];
            loop {
                match stream.read(&mut bytes).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => stream
                        .write_all(&bytes[..count])
                        .await
                        .expect("echo upgraded bytes"),
                }
            }
        });
        address
    }

    async fn wait_receive(connection: &SocketConnection, class: TrafficClass) -> SocketReceive {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match connection.receive(class).expect("receive socket event") {
                    SocketReceive::Idle => tokio::time::sleep(Duration::from_millis(5)).await,
                    event => break event,
                }
            }
        })
        .await
        .expect("socket event before timeout")
    }

    fn expect_data(event: SocketReceive) -> Vec<u8> {
        match event {
            SocketReceive::Data(bytes) => bytes,
            SocketReceive::Idle => panic!("unexpected idle event"),
            SocketReceive::Closed(reason) => panic!("connection closed early: {reason:?}"),
        }
    }

    #[tokio::test]
    async fn plaintext_dials_only_the_pinned_address_and_round_trips() {
        let address = start_plain_echo().await;
        let service = service(["service.example"], ["service.example"], [], 2);
        let authorization = Arc::new(authorize(
            &service,
            "main",
            EgressTransport::Tcp,
            "service.example",
            address,
            None,
        ));

        // `service.example` need not resolve: the connector can consume only
        // the exact address set already pinned by the shared egress boundary.
        let stream = connect_pinned(&authorization)
            .await
            .expect("dial pinned address without a second DNS lookup");
        let connection = spawn_connection(authorization, ActorStream::Plaintext(stream));
        connection
            .send(TrafficClass::Application, b"plain".to_vec())
            .await
            .expect("send plaintext bytes");
        assert_eq!(
            expect_data(wait_receive(&connection, TrafficClass::Application).await),
            b"plain"
        );
        assert_eq!(
            connection
                .send(TrafficClass::Application, vec![0_u8; MAX_CHUNK_BYTES + 1])
                .await,
            Err(SocketFailure::ChunkTooLarge)
        );
    }

    #[tokio::test]
    async fn connection_resource_retains_and_releases_the_shared_lease() {
        let address = start_plain_echo().await;
        let service = service(["service.example"], ["service.example"], [], 1);
        let request = || {
            EgressRequest::new(
                scope("main"),
                EgressTransport::Tcp,
                "service.example",
                address.port(),
                None,
            )
            .expect("valid test request")
        };
        let authorization = Arc::new(
            service
                .authorize_addresses(request(), [address])
                .expect("reserve the only shared connection slot"),
        );
        let stream = connect_pinned(&authorization)
            .await
            .expect("dial pinned address");
        let connection = spawn_connection(authorization, ActorStream::Plaintext(stream));

        assert!(matches!(
            service.authorize_addresses(request(), [address]),
            Err(EgressError::ConnectionLimitReached { limit: 1, .. })
        ));

        drop(connection);
        let replacement = service
            .authorize_addresses(request(), [address])
            .expect("dropping the resource returns its shared slot");
        drop(replacement);
    }

    #[tokio::test]
    async fn peer_eof_drains_buffered_bytes_before_the_terminal_event() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind finite peer");
        let address = listener.local_addr().expect("finite peer address");
        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.expect("accept finite client");
            stream
                .write_all(b"final-bytes")
                .await
                .expect("write final bytes");
            stream.shutdown().await.expect("close finite peer");
        });
        let service = service(["service.example"], ["service.example"], [], 1);
        let authorization = Arc::new(authorize(
            &service,
            "main",
            EgressTransport::Tcp,
            "service.example",
            address,
            None,
        ));
        let stream = connect_pinned(&authorization)
            .await
            .expect("dial finite peer");
        let connection = spawn_connection(authorization, ActorStream::Plaintext(stream));

        assert_eq!(
            expect_data(wait_receive(&connection, TrafficClass::Application).await),
            b"final-bytes"
        );
        assert!(matches!(
            wait_receive(&connection, TrafficClass::Application).await,
            SocketReceive::Closed(SocketCloseReason::PeerClosed)
        ));
    }

    #[tokio::test]
    async fn direct_tls_uses_custom_ca_and_mtls_identity() {
        let pki = TestPki::new();
        let profile = pki.profile();
        let address = start_tls_echo(pki.server_config(true)).await;
        let service = service(["localhost"], [], [profile.clone()], 2);
        let authorization = Arc::new(authorize(
            &service,
            "main",
            EgressTransport::Tls,
            "localhost",
            address,
            Some("test-mtls"),
        ));
        let stream = connect_direct_tls(&authorization, pki.client_config(Some(&profile)))
            .await
            .expect("custom-CA mTLS handshake succeeds");
        let connection = spawn_connection(authorization, ActorStream::Tls(stream));
        connection
            .send(TrafficClass::Application, b"secure".to_vec())
            .await
            .expect("send TLS bytes");
        assert_eq!(
            expect_data(wait_receive(&connection, TrafficClass::Application).await),
            b"secure"
        );
    }

    #[tokio::test]
    async fn direct_tls_rejects_an_untrusted_server() {
        let pki = TestPki::new();
        let address = start_tls_echo(pki.server_config(false)).await;
        let service = service(["localhost"], [], [], 1);
        let authorization = authorize(
            &service,
            "main",
            EgressTransport::Tls,
            "localhost",
            address,
            None,
        );
        let config = build_tls_client_config(None, |_| {
            panic!("implicit roots do not resolve plugin secrets")
        })
        .expect("system roots config");

        assert_eq!(
            connect_direct_tls(&authorization, config).await.map(drop),
            Err(SocketFailure::TlsHandshakeFailed)
        );
    }

    #[tokio::test]
    async fn starttls_separates_negotiation_and_application_traffic() {
        let pki = TestPki::new();
        let profile = pki.profile();
        let address = start_starttls_echo(pki.server_config(true)).await;
        let service = service(["localhost"], [], [profile.clone()], 1);
        let authorization = Arc::new(authorize(
            &service,
            "mail",
            EgressTransport::StartTls,
            "localhost",
            address,
            Some("test-mtls"),
        ));
        let stream = connect_pinned(&authorization).await.expect("dial STARTTLS");
        let connection = spawn_connection(
            Arc::clone(&authorization),
            ActorStream::StartTls {
                stream: Some(stream),
                state: StartTlsState::new(),
            },
        );

        assert_eq!(
            connection
                .send(TrafficClass::Application, b"credential".to_vec())
                .await,
            Err(SocketFailure::InvalidState),
            "application traffic is forbidden before TLS"
        );
        assert_eq!(
            expect_data(wait_receive(&connection, TrafficClass::Negotiation).await),
            b"220 service ready\r\n"
        );
        connection
            .send(TrafficClass::Negotiation, b"STARTTLS\r\n".to_vec())
            .await
            .expect("send protocol negotiation");
        assert_eq!(
            expect_data(wait_receive(&connection, TrafficClass::Negotiation).await),
            b"220 begin TLS\r\n"
        );

        let (reply, result) = oneshot::channel();
        connection
            .commands
            .send(ActorCommand::UpgradeTls {
                config: pki.client_config(Some(&profile)),
                server_name: tls_server_name(&authorization).expect("server name"),
                reply,
            })
            .await
            .expect("queue TLS upgrade");
        assert_eq!(result.await.expect("upgrade result"), Ok(()));
        assert_eq!(
            connection
                .send(TrafficClass::Negotiation, b"plaintext".to_vec())
                .await,
            Err(SocketFailure::InvalidState),
            "plaintext negotiation cannot resume after TLS"
        );
        connection
            .send(TrafficClass::Application, b"authenticated".to_vec())
            .await
            .expect("send post-upgrade bytes");
        assert_eq!(
            expect_data(wait_receive(&connection, TrafficClass::Application).await),
            b"authenticated"
        );

        let (reply, result) = oneshot::channel();
        connection
            .commands
            .send(ActorCommand::UpgradeTls {
                config: pki.client_config(Some(&profile)),
                server_name: tls_server_name(&authorization).expect("server name"),
                reply,
            })
            .await
            .expect("queue duplicate upgrade");
        assert_eq!(
            result.await.expect("duplicate upgrade result"),
            Err(SocketFailure::InvalidState)
        );
    }

    #[tokio::test]
    async fn failed_starttls_upgrade_is_terminal_without_plaintext_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind failed-upgrade peer");
        let address = listener.local_addr().expect("failed-upgrade address");
        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.expect("accept failed upgrade");
            stream
                .write_all(b"220 ready\r\n")
                .await
                .expect("write ready response");
            let mut command = [0_u8; 10];
            stream
                .read_exact(&mut command)
                .await
                .expect("read STARTTLS command");
            assert_eq!(&command, b"STARTTLS\r\n");
            stream
                .write_all(b"220 go\r\n")
                .await
                .expect("write upgrade response");
            // Close after observing the TLS ClientHello. The host must never
            // retry application bytes on this plaintext stream.
            let mut hello = [0_u8; 5];
            let _ = stream.read_exact(&mut hello).await;
        });
        let service = service(["localhost"], [], [], 1);
        let authorization = Arc::new(authorize(
            &service,
            "mail",
            EgressTransport::StartTls,
            "localhost",
            address,
            None,
        ));
        let stream = connect_pinned(&authorization).await.expect("dial STARTTLS");
        let connection = spawn_connection(
            Arc::clone(&authorization),
            ActorStream::StartTls {
                stream: Some(stream),
                state: StartTlsState::new(),
            },
        );
        let _ = wait_receive(&connection, TrafficClass::Negotiation).await;
        connection
            .send(TrafficClass::Negotiation, b"STARTTLS\r\n".to_vec())
            .await
            .expect("send STARTTLS command");
        let _ = wait_receive(&connection, TrafficClass::Negotiation).await;

        let (reply, result) = oneshot::channel();
        connection
            .commands
            .send(ActorCommand::UpgradeTls {
                config: build_tls_client_config(None, |_| {
                    panic!("implicit roots do not resolve secrets")
                })
                .expect("system roots"),
                server_name: tls_server_name(&authorization).expect("server name"),
                reply,
            })
            .await
            .expect("queue failing upgrade");
        assert_eq!(
            result.await.expect("failing upgrade result"),
            Err(SocketFailure::TlsHandshakeFailed)
        );
        assert_eq!(
            connection
                .send(TrafficClass::Negotiation, b"fallback".to_vec())
                .await,
            Err(SocketFailure::Closed)
        );
        assert!(matches!(
            wait_receive(&connection, TrafficClass::Application).await,
            SocketReceive::Closed(SocketCloseReason::TlsUpgradeFailed)
        ));
    }

    #[test]
    fn shared_budget_spans_transports_but_isolates_aliases() {
        let service = service(["service.example"], ["service.example"], [], 1);
        let address = SocketAddr::from(([127, 0, 0, 1], 443));
        let tcp = authorize(
            &service,
            "primary",
            EgressTransport::Tcp,
            "service.example",
            address,
            None,
        );
        let same_alias_websocket = EgressRequest::new(
            scope("primary"),
            EgressTransport::WebSocket { encrypted: true },
            "service.example",
            443,
            None,
        )
        .expect("websocket request");
        assert!(matches!(
            service.authorize_addresses(same_alias_websocket, [address]),
            Err(EgressError::ConnectionLimitReached { .. })
        ));

        let other_alias_websocket = EgressRequest::new(
            scope("backup"),
            EgressTransport::WebSocket { encrypted: true },
            "service.example",
            443,
            None,
        )
        .expect("backup websocket request");
        let backup = service
            .authorize_addresses(other_alias_websocket, [address])
            .expect("another alias has an independent budget");
        drop((tcp, backup));
    }
}
