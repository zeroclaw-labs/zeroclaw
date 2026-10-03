//! The gateway's RPC connections to the core.
//!
//! This is the strangler seam for the gateway split. While the gateway still
//! runs inside the daemon, it dials the dispatcher through the daemon's
//! in-process connector; routes that migrate onto RPC reach it through the
//! [`CoreAccess`] extractor. The cut-over later replaces the in-process dial
//! with the real socket without changing any route.
//!
//! Every core connection is bound to one HTTP caller's credential. The pool
//! is keyed by the selected auth provider and a SHA-256 of the bearer, and a
//! request is only ever sent on the connection its own credential opened:
//! there is no gateway-wide connection and no credential-less one. The pool
//! keeps no bearer, only its hash; a connection that ended is dialed again
//! on the caller's next request, which presents the bearer again.
//!
//! No credential-less connection, ever. A request whose credential is absent,
//! blank, malformed, or selects a provider that cannot verify a bearer is
//! answered `401` before any connection is opened or reused. That check is
//! local and covers shape only: whether a well-formed bearer is valid, and
//! whether a well-formed `oidc.<alias>` names a configured provider, is the
//! core's call, made on one dial that presents the credential. A credential
//! the core refuses, at the handshake or on a later call, is answered `401`
//! (`403` when the principal lacks a grant) and its connection leaves the
//! pool; the request is never retried without the credential or with another
//! one. While the core is unreachable a well-formed credential cannot be
//! judged and is answered `503`. This matters beyond the in-process transport,
//! which already refuses an anonymous `initialize`: on a Unix socket the core
//! admits a same-uid peer that presents nothing as the shared operator, and
//! with no user roster it admits any local peer through local compatibility.
//!
//! Only the pool initializes a connection, with the credential its key was
//! derived from; a request can never send `initialize` and rebind it.
//!
//! The pool holds at most `MAX_CREDENTIALS` open connections, counting one
//! that is being dialed and one a request still holds after it left the pool.
//! A connection takes its capacity before it is dialed and returns it only
//! when it closes. When the pool is full, the least recently used connection
//! no request holds is closed to make room; if every connection is in use,
//! the request waits briefly and is then answered `503 core_busy`.
//!
//! A subscription is opened through [`CoreCall::subscribe`] and nowhere
//! else. The core creates a subscription before it replies, so a caller that
//! stops waiting in between would leave it running on a pooled connection
//! that other requests keep alive. `subscribe` owns the subscription from
//! before its request is sent until the caller holds it, and the holder
//! cancels it when dropped. If the core refuses or cannot take that cancel,
//! the connection is retired, which ends every subscription on it.
//!
//! Two cases stay in-process. With pairing disabled and no provider selected
//! the caller presents no credential by design, so the request is served by
//! the route's in-process body exactly as before. A gateway with no core
//! attached (a standalone run) serves every request in-process.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use axum::Json;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::DuplexStream;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, broadcast, oneshot};
use tokio::time::Instant;
use zeroclaw_api::jsonrpc::JsonRpcError;
use zeroclaw_api::jsonrpc::error_codes::{
    AUTH_REQUIRED, CONNECTION_LIMIT_REACHED, FORBIDDEN, FS_INVALID_PATH, FS_NOT_FOUND,
    FS_PERMISSION_DENIED, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND,
    SESSION_BUSY, SESSION_LIMIT_REACHED, SESSION_NOT_FOUND, SESSION_NOT_OWNED, SOP_ALREADY_EXISTS,
    SOP_NOT_FOUND, VERSION_MISMATCH,
};
use zeroclaw_rpc_client::{
    ClientError, ConnectOptions, ConnectionState, EndpointOwner, Method, Notification, RpcClient,
};
use zeroclaw_rpc_proto::types::CLIENT_KIND_GATEWAY;
use zeroclaw_runtime::rpc::inproc::InprocConnector;

use crate::principal_gate::AUTH_PROVIDER_HEADER;

/// Core connections open at once, one per credential.
const MAX_CREDENTIALS: usize = 64;
/// How long a request waits for capacity when every connection is in use.
const CAPACITY_WAIT: Duration = Duration::from_secs(5);
/// A credential's connection leaves the pool after this long unused.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How often the idle sweep runs.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Ceiling for obtaining a transport from the dialer.
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest bearer forwarded to the core. An OIDC access token is well
/// below this; anything longer is refused as malformed.
const MAX_BEARER_BYTES: usize = 8 * 1024;

/// The provider a bare bearer selects, as in the RPC handshake.
const NATIVE_PROVIDER: &str = "native";
/// Prefix of every OIDC provider selection (`oidc.<alias>`).
const OIDC_PROVIDER_PREFIX: &str = "oidc.";

/// The 401 message for a missing or unusable bearer, unchanged from the
/// gateway's existing denial.
const PAIR_FIRST_MESSAGE: &str =
    "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>";
/// The 401 message for a missing or unusable bearer under an OIDC
/// selection, where pairing is not how a caller gets one.
const OIDC_BEARER_MESSAGE: &str =
    "Unauthorized — send Authorization: Bearer <access token> for the selected auth provider";
/// The 401 message for a provider header naming no bearer provider,
/// unchanged from the config route layer's.
const INVALID_PROVIDER_MESSAGE: &str = "Invalid auth_provider selection";

pub(crate) type DialFuture<'a> = Pin<Box<dyn Future<Output = Option<DuplexStream>> + Send + 'a>>;

/// Opens a transport to the core. The in-process connector implements it;
/// tests substitute their own.
pub(crate) trait Dial: Send + Sync + 'static {
    /// A fresh connection, or `None` when the core is not accepting any.
    fn dial(&self) -> DialFuture<'_>;
}

impl Dial for InprocConnector {
    fn dial(&self) -> DialFuture<'_> {
        Box::pin(self.connect())
    }
}

/// Handle to the gateway's core connections, shared with every request as
/// an axum extension. Cheap to clone. The default handle has no core
/// attached: every request is served in-process.
#[derive(Clone, Default)]
pub struct CoreRpc {
    seam: Option<Arc<Seam>>,
}

struct Seam {
    pool: Arc<Pool>,
    /// Whether pairing is required, asked of the same pairing authority the
    /// in-process routes check, on every request. The authority fixes the
    /// setting for its daemon generation; a reload brings a new one.
    pairing_required: Box<dyn Fn() -> bool + Send + Sync>,
}

impl CoreRpc {
    /// Serve RPC-backed routes through the daemon's in-process connector.
    /// `pairing_required` asks the pairing authority the in-process routes
    /// use.
    pub fn inproc(
        connector: InprocConnector,
        pairing_required: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self::with_dialer(connector, pairing_required, PoolLimits::default())
    }

    /// A handle over any dialer, for route tests outside this module.
    #[cfg(test)]
    pub(crate) fn over_dialer(dialer: impl Dial) -> Self {
        Self::with_dialer(dialer, || true, PoolLimits::default())
    }

    /// Serve RPC-backed routes through the daemon's local socket at
    /// `endpoint`, which `owner` must serve: every dial verifies that
    /// through the kernel before the caller's credential is written. A
    /// gateway in its own process has no in-process path, so every request
    /// needs a credential.
    pub fn local(endpoint: PathBuf, owner: EndpointOwner) -> Self {
        let limits = PoolLimits::default();
        Self::with_pool(Pool::local(endpoint, owner, limits), || true)
    }

    fn with_dialer(
        dialer: impl Dial,
        pairing_required: impl Fn() -> bool + Send + Sync + 'static,
        limits: PoolLimits,
    ) -> Self {
        Self::with_pool(Pool::new(Box::new(dialer), limits), pairing_required)
    }

    fn with_pool(pool: Pool, pairing_required: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        let pool = Arc::new(pool);
        spawn_idle_sweep(Arc::downgrade(&pool), pool.limits.sweep_interval);
        Self {
            seam: Some(Arc::new(Seam {
                pool,
                pairing_required: Box::new(pairing_required),
            })),
        }
    }

    /// Decide how the request carrying `headers` reaches the core.
    ///
    /// Every refusal here happens before a connection is opened or reused.
    pub async fn access(&self, headers: &HeaderMap) -> Result<CoreAccess, CoreError> {
        let Some(seam) = &self.seam else {
            return Ok(CoreAccess::InProcess);
        };
        let provider = provider_selection(headers)?;
        if provider.is_none() && !(seam.pairing_required)() {
            return Ok(CoreAccess::InProcess);
        }
        let provider = provider.unwrap_or(NATIVE_PROVIDER);
        let credential = HttpCredential {
            provider,
            token: bearer(headers, provider)?,
        };
        Arc::clone(&seam.pool)
            .acquire(&credential)
            .await
            .map(CoreAccess::Core)
    }
}

/// How one HTTP request reaches the core.
pub enum CoreAccess {
    /// Through the core, on a connection bound to the caller's credential.
    Core(CoreCall),
    /// Through the route's in-process body: pairing is disabled and the
    /// caller selected no provider, or this gateway has no core attached.
    InProcess,
}

impl<S: Send + Sync> FromRequestParts<S> for CoreAccess {
    type Rejection = CoreError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // The router always installs the handle; a route without it is a
        // wiring fault, refused rather than served in-process.
        let Some(core) = parts.extensions.get::<CoreRpc>().cloned() else {
            return Err(CoreError::Unavailable(
                "the core connection is not configured for this route".into(),
            ));
        };
        core.access(&parts.headers).await
    }
}

/// [`CoreAccess`] for a WebSocket upgrade. A browser cannot set an
/// `Authorization` header on a WebSocket, so the dashboard offers its bearer
/// as a `bearer.<token>` subprotocol, which the in-process sockets accept.
/// That bearer counts only when the request carries no `Authorization`
/// header; the decision is then exactly [`CoreRpc::access`]'s.
pub struct WsCoreAccess(pub CoreAccess);

impl<S: Send + Sync> FromRequestParts<S> for WsCoreAccess {
    type Rejection = CoreError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Some(core) = parts.extensions.get::<CoreRpc>().cloned() else {
            return Err(CoreError::Unavailable(
                "the core connection is not configured for this route".into(),
            ));
        };
        let offered = subprotocol_bearer(&parts.headers)
            .filter(|_| !parts.headers.contains_key(header::AUTHORIZATION))
            .and_then(|token| HeaderValue::from_str(&format!("Bearer {token}")).ok());
        let access = match offered {
            Some(authorization) => {
                let mut headers = parts.headers.clone();
                headers.insert(header::AUTHORIZATION, authorization);
                core.access(&headers).await
            }
            None => core.access(&parts.headers).await,
        };
        access.map(Self)
    }
}

/// The bearer a WebSocket client offers as a `bearer.<token>` subprotocol.
pub(crate) fn subprotocol_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .and_then(|protos| {
            protos
                .split(',')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("bearer."))
        })
}

/// Why a request could not be served through the core. Each case answers
/// with its own status, so a refused credential (`401`) is never reported
/// as an unreachable core (`503`) or the reverse.
#[derive(Debug)]
pub enum CoreError {
    /// The caller presented no usable credential, or the core refused it.
    AuthRequired(String),
    /// The core refused this principal the operation.
    Forbidden(String),
    /// The core could not be reached.
    Unavailable(String),
    /// The endpoint is not served by the account the gateway expects, so
    /// nothing was sent to it.
    UntrustedEndpoint(String),
    /// Every core connection the gateway may hold is in use.
    Busy,
    /// The core did not answer in time.
    Timeout,
    /// Any other refusal from the core, mapped by its code.
    Rpc(JsonRpcError),
}

impl CoreError {
    fn from_rpc(error: JsonRpcError) -> Self {
        match error.code {
            AUTH_REQUIRED => Self::AuthRequired(error.message),
            FORBIDDEN => Self::Forbidden(error.message),
            _ => Self::Rpc(error),
        }
    }

    /// The HTTP status and stable machine code this error answers with.
    pub fn status(&self) -> (StatusCode, &'static str) {
        match self {
            Self::AuthRequired(_) => (StatusCode::UNAUTHORIZED, "auth_required"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "core_unavailable"),
            Self::UntrustedEndpoint(_) => {
                (StatusCode::SERVICE_UNAVAILABLE, "core_untrusted_endpoint")
            }
            Self::Busy => (StatusCode::SERVICE_UNAVAILABLE, "core_busy"),
            Self::Timeout => (StatusCode::GATEWAY_TIMEOUT, "core_timeout"),
            Self::Rpc(error) => rpc_status(error.code),
        }
    }
}

/// The HTTP mapping of a core error code.
fn rpc_status(code: i32) -> (StatusCode, &'static str) {
    match code {
        AUTH_REQUIRED => (StatusCode::UNAUTHORIZED, "auth_required"),
        FORBIDDEN | SESSION_NOT_OWNED | FS_PERMISSION_DENIED => {
            (StatusCode::FORBIDDEN, "forbidden")
        }
        INVALID_PARAMS | FS_INVALID_PATH => (StatusCode::BAD_REQUEST, "invalid_params"),
        SESSION_NOT_FOUND | SOP_NOT_FOUND | FS_NOT_FOUND => (StatusCode::NOT_FOUND, "not_found"),
        SESSION_LIMIT_REACHED => (StatusCode::TOO_MANY_REQUESTS, "session_limit_reached"),
        SESSION_BUSY | SOP_ALREADY_EXISTS => (StatusCode::CONFLICT, "conflict"),
        METHOD_NOT_FOUND => (StatusCode::SERVICE_UNAVAILABLE, "core_capability_missing"),
        VERSION_MISMATCH => (StatusCode::SERVICE_UNAVAILABLE, "core_incompatible"),
        CONNECTION_LIMIT_REACHED => (StatusCode::SERVICE_UNAVAILABLE, "core_unavailable"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "core_error"),
    }
}

impl IntoResponse for CoreError {
    fn into_response(self) -> Response {
        let (status, code) = self.status();
        let message = match self {
            Self::AuthRequired(message)
            | Self::Forbidden(message)
            | Self::Unavailable(message)
            | Self::UntrustedEndpoint(message) => message,
            Self::Busy => "every core connection is in use; retry shortly".to_owned(),
            Self::Timeout => "the core did not answer in time".to_owned(),
            Self::Rpc(error) => error.message,
        };
        (
            status,
            Json(serde_json::json!({ "error": message, "code": code })),
        )
            .into_response()
    }
}

/// The provider the caller selected, `None` when the header is absent.
///
/// A header that is present must name a provider that verifies a bearer:
/// `native`, or an `oidc.<alias>` selection. Anything else, including a
/// blank value or a provider that authenticates the transport rather than a
/// bearer, is refused; it never falls back to the native provider or to the
/// in-process path. Whether an OIDC alias is configured is the core's call.
fn provider_selection(headers: &HeaderMap) -> Result<Option<&str>, CoreError> {
    let Some(value) = headers.get(AUTH_PROVIDER_HEADER) else {
        return Ok(None);
    };
    let name = value.to_str().map(str::trim).unwrap_or_default();
    let verifies_a_bearer = name == NATIVE_PROVIDER
        || name
            .strip_prefix(OIDC_PROVIDER_PREFIX)
            .is_some_and(|alias| !alias.is_empty() && alias.bytes().all(|b| b.is_ascii_graphic()));
    if verifies_a_bearer {
        Ok(Some(name))
    } else {
        Err(CoreError::AuthRequired(INVALID_PROVIDER_MESSAGE.into()))
    }
}

/// The caller's bearer: non-empty, printable ASCII with no whitespace, and
/// bounded in length. Anything else is refused before the core sees it, with
/// the hint that fits the selected provider.
fn bearer<'h>(headers: &'h HeaderMap, provider: &str) -> Result<&'h str, CoreError> {
    let hint = if provider == NATIVE_PROVIDER {
        PAIR_FIRST_MESSAGE
    } else {
        OIDC_BEARER_MESSAGE
    };
    let refused = || CoreError::AuthRequired(hint.into());
    let token = crate::api::extract_bearer_token(headers).ok_or_else(refused)?;
    if token.is_empty()
        || token.len() > MAX_BEARER_BYTES
        || !token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(refused());
    }
    Ok(token)
}

/// A credential that passed the gateway's shape checks. Borrowed from the
/// request; the pool never stores it.
struct HttpCredential<'a> {
    provider: &'a str,
    token: &'a str,
}

impl HttpCredential<'_> {
    fn key(&self) -> PoolKey {
        PoolKey {
            provider: self.provider.to_owned(),
            credential_sha256: Sha256::digest(self.token.as_bytes()).into(),
        }
    }

    /// The handshake options for this credential. Always carries the
    /// bearer and the provider that must verify it, and declares the
    /// connection a gateway's, so the core's `tui/list` does not pass it off
    /// as a terminal. The declaration grants nothing.
    fn connect_options(&self) -> ConnectOptions {
        ConnectOptions {
            auth_token: Some(self.token.to_owned()),
            auth_provider: Some(self.provider.to_owned()),
            client_capabilities: Some(serde_json::json!({ "client_kind": CLIENT_KIND_GATEWAY })),
            ..ConnectOptions::default()
        }
    }
}

/// Pool key: the provider selection and the credential's SHA-256, never
/// the principal (two bearers of one principal expire and are revoked
/// independently).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PoolKey {
    provider: String,
    credential_sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
struct PoolLimits {
    max_credentials: usize,
    idle_timeout: Duration,
    sweep_interval: Duration,
    capacity_wait: Duration,
}

impl Default for PoolLimits {
    fn default() -> Self {
        Self {
            max_credentials: MAX_CREDENTIALS,
            idle_timeout: IDLE_TIMEOUT,
            sweep_interval: SWEEP_INTERVAL,
            capacity_wait: CAPACITY_WAIT,
        }
    }
}

/// Where the pool opens its connections.
enum Connector {
    /// The daemon's in-process duplex, or a test's stand-in.
    Duplex(Box<dyn Dial>),
    /// The daemon's local socket. The client verifies through the kernel
    /// that `owner` serves `endpoint` before it writes the credential.
    Local {
        endpoint: PathBuf,
        owner: EndpointOwner,
    },
}

struct Pool {
    connector: Connector,
    limits: PoolLimits,
    /// One permit per open core connection. A connection takes its permit
    /// before it is dialed and returns it when it closes.
    capacity: Arc<Semaphore>,
    /// Woken when a request lets go of its connection, which may leave that
    /// connection evictable for a request waiting for capacity.
    lease_released: Notify,
    slots: Mutex<HashMap<PoolKey, Arc<Slot>>>,
}

/// A core connection and the capacity it occupies. Held by its slot and by
/// every request using it; the connection closes, and its capacity returns,
/// when the last of them lets go.
struct PooledClient {
    client: RpcClient,
    /// Released on drop, whoever drops the connection last.
    _capacity: OwnedSemaphorePermit,
}

/// One credential's place in the pool.
struct Slot {
    /// Serializes dials for this credential, so concurrent first requests
    /// share one connection.
    dial_lock: tokio::sync::Mutex<()>,
    client: Mutex<Option<Arc<PooledClient>>>,
    last_used: Mutex<Instant>,
}

impl Slot {
    fn new() -> Self {
        Self {
            dial_lock: tokio::sync::Mutex::new(()),
            client: Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
        }
    }

    fn touch(&self) {
        *lock(&self.last_used) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        lock(&self.last_used).elapsed()
    }

    fn live_client(&self) -> Option<Arc<PooledClient>> {
        lock(&self.client)
            .as_ref()
            .filter(|pooled| pooled.client.state() == ConnectionState::Connected)
            .cloned()
    }

    /// Whether a request still holds this slot's connection.
    fn leased(&self) -> bool {
        lock(&self.client)
            .as_ref()
            .is_some_and(|pooled| Arc::strong_count(pooled) > 1)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Pool {
    fn new(dialer: Box<dyn Dial>, limits: PoolLimits) -> Self {
        Self::with_connector(Connector::Duplex(dialer), limits)
    }

    fn local(endpoint: PathBuf, owner: EndpointOwner, limits: PoolLimits) -> Self {
        Self::with_connector(Connector::Local { endpoint, owner }, limits)
    }

    fn with_connector(connector: Connector, limits: PoolLimits) -> Self {
        Self {
            connector,
            limits,
            capacity: Arc::new(Semaphore::new(limits.max_credentials)),
            lease_released: Notify::new(),
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// The connection for `credential`, dialing one if it has none.
    async fn acquire(
        self: Arc<Self>,
        credential: &HttpCredential<'_>,
    ) -> Result<CoreCall, CoreError> {
        let key = credential.key();
        let slot = {
            let mut slots = lock(&self.slots);
            self.sweep_idle_locked(&mut slots);
            Arc::clone(
                slots
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(Slot::new())),
            )
        };
        slot.touch();
        if let Some(pooled) = slot.live_client() {
            return Ok(self.call(key, slot, pooled));
        }

        let dialing_slot = Arc::clone(&slot);
        let _dialing = dialing_slot.dial_lock.lock().await;
        if let Some(pooled) = slot.live_client() {
            return Ok(self.call(key, slot, pooled));
        }
        // A connection that ended gives its capacity back before this
        // credential asks for new capacity.
        drop(lock(&slot.client).take());
        let capacity = match self.reserve(&key).await {
            Ok(capacity) => capacity,
            Err(error) => {
                self.forget(&key, &slot);
                return Err(error);
            }
        };
        match self.dial(credential).await {
            Ok(client) => {
                let pooled = Arc::new(PooledClient {
                    client,
                    _capacity: capacity,
                });
                *lock(&slot.client) = Some(Arc::clone(&pooled));
                Ok(self.call(key, slot, pooled))
            }
            Err(error) => {
                self.forget(&key, &slot);
                Err(error)
            }
        }
    }

    /// Capacity for one more connection. When none is free, close the least
    /// recently used connection no request holds; when every connection is
    /// in use, wait until one closes or a request lets go of one, within
    /// one overall deadline.
    async fn reserve(&self, dialing: &PoolKey) -> Result<OwnedSemaphorePermit, CoreError> {
        let deadline = Instant::now() + self.limits.capacity_wait;
        loop {
            // Register for the wake-up before looking, so a release between
            // the look and the wait is not missed.
            let released = self.lease_released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if let Ok(permit) = Arc::clone(&self.capacity).try_acquire_owned() {
                return Ok(permit);
            }
            if self.evict_one_unleased(dialing) {
                continue;
            }
            tokio::select! {
                permit = Arc::clone(&self.capacity).acquire_owned() => {
                    return permit.map_err(|_| CoreError::Busy);
                }
                () = &mut released => {}
                () = tokio::time::sleep_until(deadline) => return Err(CoreError::Busy),
            }
        }
    }

    /// Close one connection no request holds, ended ones first, then the
    /// least recently used. Returns whether one was closed.
    fn evict_one_unleased(&self, dialing: &PoolKey) -> bool {
        let mut slots = lock(&self.slots);
        let victim = slots
            .iter()
            .filter(|(key, slot)| {
                *key != dialing
                    && slot.dial_lock.try_lock().is_ok()
                    && lock(&slot.client).is_some()
                    && !slot.leased()
            })
            .max_by_key(|(_, slot)| (slot.live_client().is_none(), slot.idle_for()))
            .map(|(key, _)| key.clone());
        victim.is_some_and(|key| slots.remove(&key).is_some())
    }

    fn call(
        self: &Arc<Self>,
        key: PoolKey,
        slot: Arc<Slot>,
        pooled: Arc<PooledClient>,
    ) -> CoreCall {
        CoreCall {
            pool: Arc::clone(self),
            key,
            slot,
            pooled,
            _lease_released: LeaseReleased(Arc::clone(self)),
        }
    }

    /// Open a connection and present `credential` in its handshake.
    async fn dial(&self, credential: &HttpCredential<'_>) -> Result<RpcClient, CoreError> {
        let connected = match &self.connector {
            Connector::Duplex(dialer) => {
                let stream = match tokio::time::timeout(DIAL_TIMEOUT, dialer.dial()).await {
                    Ok(Some(stream)) => stream,
                    Ok(None) | Err(_) => {
                        return Err(CoreError::Unavailable(
                            "the core is not accepting connections".into(),
                        ));
                    }
                };
                RpcClient::connect_over(stream, credential.connect_options()).await
            }
            Connector::Local { endpoint, owner } => {
                let options = ConnectOptions {
                    endpoint_owner: *owner,
                    ..credential.connect_options()
                };
                RpcClient::connect_local(endpoint, options).await
            }
        };
        match connected {
            Ok(client) => Ok(client),
            Err(ClientError::Rpc(error)) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "provider": credential.provider,
                            "code": error.code,
                        })),
                    "core refused a gateway caller's handshake"
                );
                Err(match error.code {
                    AUTH_REQUIRED | FORBIDDEN | VERSION_MISMATCH => CoreError::from_rpc(error),
                    _ => CoreError::Unavailable(error.message),
                })
            }
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "error": error.to_string() })),
                    "gateway could not open a core connection"
                );
                // Nothing was sent: the endpoint is not served by the account
                // the gateway runs as.
                if let ClientError::UntrustedEndpoint { .. } = &error {
                    return Err(CoreError::UntrustedEndpoint(format!(
                        "refusing to send the credential: {error}; run the gateway as the same \
                         OS account as the core"
                    )));
                }
                Err(CoreError::Unavailable(match (&self.connector, &error) {
                    (Connector::Local { endpoint, .. }, _) => format!(
                        "the core is not reachable at {}: {error}",
                        endpoint.display()
                    ),
                    (Connector::Duplex(_), _) => {
                        "the core connection could not be established".into()
                    }
                }))
            }
        }
    }

    /// Remove `slot` after a failed dial, if it is still the one registered
    /// for `key` and no other dial gave it a connection.
    fn forget(&self, key: &PoolKey, slot: &Arc<Slot>) {
        let mut slots = lock(&self.slots);
        if slots
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, slot))
            && slot.live_client().is_none()
        {
            slots.remove(key);
        }
    }

    /// Remove `key`'s slot if it still holds `pooled`.
    fn discard(&self, key: &PoolKey, slot: &Arc<Slot>, pooled: &Arc<PooledClient>) {
        let mut slots = lock(&self.slots);
        let registered = slots
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, slot));
        let holds = lock(&slot.client)
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, pooled));
        if registered && holds {
            slots.remove(key);
        }
    }

    fn sweep_idle(&self) {
        let mut slots = lock(&self.slots);
        self.sweep_idle_locked(&mut slots);
    }

    /// Drop connections unused past the idle timeout and slots whose
    /// connection ended. A slot that is dialing, or whose live connection a
    /// request still holds, is in use and left alone.
    fn sweep_idle_locked(&self, slots: &mut HashMap<PoolKey, Arc<Slot>>) {
        slots.retain(|_, slot| {
            if slot.dial_lock.try_lock().is_err() || (slot.leased() && slot.live_client().is_some())
            {
                return true;
            }
            let expired = slot.idle_for() >= self.limits.idle_timeout;
            let has_connection = lock(&slot.client).is_some();
            let ended = has_connection && slot.live_client().is_none();
            !(expired || ended)
        });
    }

    /// Credentials currently holding a live connection in the pool.
    #[cfg(test)]
    fn pooled(&self) -> usize {
        lock(&self.slots)
            .values()
            .filter(|slot| slot.live_client().is_some())
            .count()
    }

    /// Open core connections, in the pool or still held by a request.
    #[cfg(test)]
    fn open_connections(&self) -> usize {
        self.limits.max_credentials - self.capacity.available_permits()
    }
}

fn spawn_idle_sweep(pool: Weak<Pool>, interval: Duration) {
    zeroclaw_spawn::spawn!(async move {
        let mut ticks = tokio::time::interval(interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            let Some(pool) = pool.upgrade() else {
                return;
            };
            pool.sweep_idle();
        }
    });
}

/// A request's core connection, bound to the credential it presented.
pub struct CoreCall {
    pool: Arc<Pool>,
    key: PoolKey,
    slot: Arc<Slot>,
    pooled: Arc<PooledClient>,
    /// Must stay the last field. Fields drop in declaration order, so by the
    /// time this wakes the waiters, `pooled` above has already released the
    /// request's hold on the connection and they find it evictable.
    _lease_released: LeaseReleased,
}

/// Wakes requests waiting for capacity when a request lets go of its
/// connection, which may have left that connection idle and evictable.
struct LeaseReleased(Arc<Pool>);

impl Drop for LeaseReleased {
    fn drop(&mut self) {
        self.0.lease_released.notify_waiters();
    }
}

impl CoreCall {
    /// The principal the core bound this connection to.
    pub fn principal_id(&self) -> Option<&str> {
        self.pooled.client.handshake().principal_id.as_deref()
    }

    /// Send `method` on this caller's connection.
    ///
    /// `initialize` is refused here without reaching the core: only the pool
    /// initializes a connection, with the credential its key came from, so a
    /// request can never rebind a connection to another credential.
    ///
    /// A reply from the core stands whatever happens to the connection after
    /// it. A credential the core now refuses (`AUTH_REQUIRED`: revoked,
    /// expired or due for re-verification) ends the connection at once; a
    /// missing grant (`FORBIDDEN`) or a lost connection takes it out of the
    /// pool. Either way the next request dials again with the credential it
    /// presents. Nothing is retried here.
    pub async fn request(&self, method: Method, params: Value) -> Result<Value, CoreError> {
        if method == Method::Initialize {
            return Err(CoreError::Rpc(JsonRpcError {
                code: INVALID_REQUEST,
                message: "initialize is sent only by the gateway's connection pool".into(),
                data: None,
            }));
        }
        let client = &self.pooled.client;
        match client.request(method, params).await {
            Ok(value) => {
                self.slot.touch();
                Ok(value)
            }
            // The client synthesizes only INTERNAL_ERROR itself, for a request
            // it could not deliver or whose connection dropped before a reply;
            // every other code is the core's own reply.
            Err(ClientError::Rpc(error)) if error.code != INTERNAL_ERROR => {
                if error.code == AUTH_REQUIRED {
                    self.pool.discard(&self.key, &self.slot, &self.pooled);
                    client.shutdown();
                } else if error.code == FORBIDDEN || client.state() != ConnectionState::Connected {
                    self.pool.discard(&self.key, &self.slot, &self.pooled);
                }
                Err(CoreError::from_rpc(error))
            }
            Err(ClientError::Rpc(error)) if client.state() == ConnectionState::Connected => {
                Err(CoreError::from_rpc(error))
            }
            // A connection that did not answer is not reused: the next
            // request dials again with its credential.
            Err(ClientError::Timeout { .. }) => {
                self.pool.discard(&self.key, &self.slot, &self.pooled);
                Err(CoreError::Timeout)
            }
            Err(error) => {
                self.pool.discard(&self.key, &self.slot, &self.pooled);
                Err(CoreError::Unavailable(format!(
                    "the core connection was lost: {error}"
                )))
            }
        }
    }

    /// [`CoreCall::request`], decoding the result into `T`. A result that
    /// does not decode is the core's fault, reported as a core error.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        params: Value,
    ) -> Result<T, CoreError> {
        let value = self.request(method, params).await?;
        serde_json::from_value(value).map_err(|error| {
            CoreError::Rpc(JsonRpcError {
                code: INTERNAL_ERROR,
                message: format!("undecodable {} result: {error}", method.wire_name()),
                data: None,
            })
        })
    }
}

// ── Subscriptions ────────────────────────────────────────────────

/// A subscription the core opened on one caller's connection, held for as
/// long as this value lives. Dropping it cancels the subscription; if the
/// core refuses that cancel or cannot take it, the connection is retired,
/// which ends every subscription on it.
pub struct CoreSubscription<T> {
    /// The subscribe method's result.
    pub opened: T,
    /// The notifications arriving on the connection, from before the
    /// subscribe request was sent, so none of this subscription's first
    /// frames are missed. Every subscription on the connection shares them:
    /// [`Self::is_mine`] tells this one's apart.
    pub notifications: broadcast::Receiver<Notification>,
    held: Subscribed,
}

impl<T> CoreSubscription<T> {
    /// The id the core gave this subscription.
    pub fn id(&self) -> &str {
        &self.held.id
    }

    /// Whether `notification` is this subscription's: a frame, or a notice
    /// such as `subscription/lagged`, naming its id.
    pub fn is_mine(&self, notification: &Notification) -> bool {
        notification
            .params
            .get("subscription_id")
            .and_then(Value::as_str)
            == Some(self.id())
    }

    /// The caller's connection, for other requests while the subscription
    /// lives.
    pub fn call(&self) -> &CoreCall {
        self.held.call.as_ref().expect("held until dropped")
    }

    /// Resolves once the connection carrying this subscription has ended.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        let pooled = Arc::clone(&self.call().pooled);
        async move { pooled.client.closed().await }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for CoreSubscription<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreSubscription")
            .field("id", &self.held.id)
            .field("opened", &self.opened)
            .finish_non_exhaustive()
    }
}

/// An open subscription and the connection it is on. Dropping it cancels
/// the subscription, retiring the connection if that fails.
struct Subscribed {
    id: String,
    call: Option<CoreCall>,
}

impl Drop for Subscribed {
    fn drop(&mut self) {
        let Some(call) = self.call.take() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            // Nothing left to send a cancel on: ending the connection ends
            // the subscription.
            call.retire();
            return;
        }
        let subscription_id = std::mem::take(&mut self.id);
        zeroclaw_spawn::spawn!(async move {
            let cancelled = call
                .request(
                    Method::SubscriptionCancel,
                    serde_json::json!({ "subscription_id": subscription_id }),
                )
                .await;
            // Refused (a principal may hold the grant to subscribe and not
            // the one to cancel) or not taken (a timeout, a lost
            // connection): the subscription may still be running, so the
            // connection goes, and the subscription with it.
            if cancelled.is_err() {
                call.retire();
            }
        });
    }
}

impl CoreCall {
    /// Open a subscription with `method` on this caller's connection, and
    /// own it across the whole round trip.
    ///
    /// The core creates a subscription before it replies. The request runs
    /// on its own task, so a caller that stops waiting (its future dropped)
    /// does not take the reply with it: a subscription the core opened
    /// anyway is cancelled as soon as the reply arrives, and the connection
    /// is retired if that cancel fails. A reply that names no subscription,
    /// or a request that timed out, leaves one that cannot be cancelled by
    /// id, so the connection is retired then too. A refusal opens nothing.
    pub async fn subscribe<T: DeserializeOwned>(
        self,
        method: Method,
        params: Value,
    ) -> Result<CoreSubscription<T>, CoreError> {
        // Listen first: a frame can reach the connection ahead of the reply.
        let notifications = self.pooled.client.notifications();
        let (deliver, delivered) = oneshot::channel::<Result<(Value, Subscribed), CoreError>>();
        zeroclaw_spawn::spawn!(async move {
            let opened = match self.request(method, params).await {
                Ok(opened) => opened,
                Err(error) => {
                    if matches!(error, CoreError::Timeout) {
                        // The core may have opened it after all.
                        self.retire();
                    }
                    let _ = deliver.send(Err(error));
                    return;
                }
            };
            let Some(id) = opened
                .get("subscription_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                self.retire();
                let _ = deliver.send(Err(CoreError::Rpc(JsonRpcError {
                    code: INTERNAL_ERROR,
                    message: format!("{} named no subscription", method.wire_name()),
                    data: None,
                })));
                return;
            };
            let held = Subscribed {
                id,
                call: Some(self),
            };
            // A caller that has gone hands it back, and dropping it here
            // cancels it. One that goes after this send drops it unread with
            // the channel, which cancels it the same way.
            let _ = deliver.send(Ok((opened, held)));
        });
        let (opened, held) = delivered.await.map_err(|_| {
            CoreError::Unavailable("the subscription request ended without an answer".into())
        })??;
        let opened = serde_json::from_value(opened).map_err(|error| {
            CoreError::Rpc(JsonRpcError {
                code: INTERNAL_ERROR,
                message: format!("undecodable {} result: {error}", method.wire_name()),
                data: None,
            })
        })?;
        Ok(CoreSubscription {
            opened,
            notifications,
            held,
        })
    }

    /// Take this connection out of service: out of the pool and closed, so
    /// the core ends whatever it still holds for it.
    fn retire(&self) {
        self.pool.discard(&self.key, &self.slot, &self.pooled);
        self.pooled.client.shutdown();
    }
}

#[cfg(test)]
mod tests;
