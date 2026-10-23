//! Opt-in dashboard sign-in sessions (`[gateway.dashboard_sign_in]`).
//!
//! A browser that completed an OIDC sign-in holds only an opaque session
//! cookie. The access token it was issued stays here, in memory, and every
//! request carrying the cookie is checked again against the same inbound
//! authority RPC uses: credential expiry, the introspection revalidation
//! deadline, and the grants the principal holds under the policy in force
//! now. The gateway decides nothing of its own; it only narrows.
//!
//! The layer never rejects a request itself. It classifies the caller and
//! leaves the decision to the route: public routes ignore the
//! classification, so an expired cookie never keeps the dashboard shell or
//! the sign-in screen from loading, while every authenticated route reads it
//! through `current_caller` or the request's [`DashboardCaller`] extension.
//!
//! With sign-in off, which is the default, the layer is a no-op that never
//! reads a cookie: a forged one is no credential at all.
//!
//! The store is process-shared rather than held in `AppState`. The gateway
//! is rebuilt on every daemon generation, and a sign-in has to survive a
//! reload; a session verified by an earlier generation's authority is
//! verified again, with the token it holds, against the current one before
//! it is used.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::grants::ResolvedGrants;
use zeroclaw_api::principal::{ActorKind, Principal};
use zeroclaw_runtime::rpc::auth::{ConnectionAuth, RpcInboundAuth};
use zeroize::Zeroizing;

use crate::AppState;
use crate::principal_gate::GatewayInboundAuth;

/// Session cookie on an HTTPS dashboard served at the root path. The
/// `__Host-` prefix makes the browser refuse it unless it is `Secure`,
/// host-only and `Path=/`, so a sibling host cannot plant one.
pub(crate) const HOST_SESSION_COOKIE: &str = "__Host-zc_session";

/// Session cookie everywhere `__Host-` cannot apply: under a path prefix,
/// or over plain HTTP on a loopback host.
pub(crate) const SESSION_COOKIE: &str = "zc_session";

/// Name stem of the short-lived cookie that binds a dashboard sign-in to
/// the browser that started it, so a callback someone else started cannot
/// finish here. Each flow gets its own cookie, named after its `state`, so
/// two sign-ins in one browser (two tabs, a re-auth popup) never overwrite
/// each other's binding.
const BINDING_COOKIE_STEM: &str = "zc_signin_";

/// Header a cookie-authenticated request that changes state must carry,
/// holding the session's CSRF token.
pub(crate) const CSRF_HEADER: &str = "x-zeroclaw-csrf";

/// Sessions held at once across every principal. At the cap a new sign-in
/// is refused rather than evicting somebody else's session, so one account
/// cycling sign-ins cannot sign everyone else out.
pub(crate) const MAX_SESSIONS: usize = 256;

/// Sessions one principal may hold at once (a browser per device, a few
/// profiles). A further sign-in by the same principal replaces its oldest.
pub(crate) const MAX_SESSIONS_PER_PRINCIPAL: usize = 8;

/// Longest display name kept from the verified claims.
const MAX_DISPLAY_NAME_CHARS: usize = 128;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The store's key for a cookie value: the store never holds a value a
/// browser could present.
pub(crate) fn session_key(cookie_value: &str) -> [u8; 32] {
    Sha256::digest(cookie_value.as_bytes()).into()
}

/// The provider a session was signed in with, as configured then. A
/// session whose alias now names another issuer or client is ended rather
/// than verified again: its token must never be sent to a provider that
/// did not issue it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderPin {
    pub(crate) issuer: String,
    pub(crate) client_id: String,
}

impl ProviderPin {
    pub(crate) fn of(config: &zeroclaw_config::schema::OidcConfig) -> Self {
        Self {
            issuer: config.issuer.clone(),
            client_id: config.effective_client_id().to_string(),
        }
    }

    /// The same pin, for the runtime to check its tokens against before
    /// sending them.
    fn binding(&self) -> zeroclaw_runtime::rpc::auth::OidcBinding {
        zeroclaw_runtime::rpc::auth::OidcBinding {
            issuer: self.issuer.clone(),
            client_id: self.client_id.clone(),
        }
    }
}

/// One signed-in browser.
struct Session {
    alias: String,
    provider: ProviderPin,
    /// The access token the sign-in was issued. Memory only; never logged
    /// and never sent anywhere but the inbound authority that verifies it.
    /// Wiped when the session leaves the store, as is every copy taken to
    /// verify it.
    bearer: Zeroizing<String>,
    auth: ConnectionAuth,
    /// The authority `auth` was verified by. A reload builds a new one, and
    /// a session verified by another is verified again before it is used.
    /// Holding the `Weak` keeps the old allocation, so the comparison
    /// cannot be fooled by an address reused for the new authority.
    authority: Weak<RpcInboundAuth>,
    csrf: String,
    display_name: String,
    created: u64,
    hard_expiry: u64,
    /// Cancelled on sign-out, expiry or revocation, so long-lived sockets
    /// the session opened can close.
    ended: CancellationToken,
    /// The chats the session has open on the core.
    core: Arc<crate::dashboard_core::CoreBinding>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("alias", &self.alias)
            .field("bearer", &"<redacted>")
            .field("principal", &self.auth.principal.id)
            .field("csrf", &"<redacted>")
            .field("created", &self.created)
            .field("hard_expiry", &self.hard_expiry)
            .finish()
    }
}

impl Session {
    fn expires_at(&self) -> u64 {
        self.auth
            .principal
            .expires_at
            .map_or(self.hard_expiry, |exp| exp.min(self.hard_expiry))
    }
}

/// A sign-in the store could not take.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StoreFull;

/// A verified sign-in on its way into the store.
struct NewSession {
    alias: String,
    provider: ProviderPin,
    bearer: Zeroizing<String>,
    auth: ConnectionAuth,
    authority: Weak<RpcInboundAuth>,
    display_name: String,
    lifetime_secs: u64,
}

/// How long the store remembers that a session expired, so a browser
/// that comes back with its cookie is told to sign in again rather than
/// that its session never existed.
const EXPIRED_MEMORY_SECS: u64 = 60 * 60;

/// Expired sessions remembered at once; the oldest is forgotten first.
const MAX_EXPIRED_REMEMBERED: usize = 1024;

/// Every live dashboard session in this process.
#[derive(Default)]
pub struct DashboardSessions {
    sessions: parking_lot::Mutex<HashMap<[u8; 32], Session>>,
    /// Keys of sessions that ended by expiring, with when they expired.
    /// Holds no credential; only answers "expired" instead of "invalid".
    expired: parking_lot::Mutex<HashMap<[u8; 32], u64>>,
    /// Whether a session may be held, so the sign-in-off path, which runs
    /// on every request, takes no lock when nothing was ever signed in.
    occupied: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for DashboardSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DashboardSessions")
            .field("sessions", &self.sessions.lock().len())
            .finish()
    }
}

impl DashboardSessions {
    /// The process's one store, shared by every gateway generation.
    pub fn process_shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<DashboardSessions>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(Self::default())))
    }

    fn sweep(&self, sessions: &mut HashMap<[u8; 32], Session>, now: u64) {
        let mut ended = Vec::new();
        sessions.retain(|key, session| {
            let live = session.expires_at() > now;
            if !live {
                session.ended.cancel();
                ended.push(*key);
            }
            live
        });
        for key in ended {
            self.remember_expired(key, now);
        }
    }

    /// Sweep with no request in hand: the idle sweep, so an expired
    /// session's token leaves memory even if nothing names it again.
    fn sweep_now(&self, now: u64) {
        let mut sessions = self.sessions.lock();
        self.sweep(&mut sessions, now);
        drop(sessions);
        self.expired
            .lock()
            .retain(|_, at| now.saturating_sub(*at) < EXPIRED_MEMORY_SECS);
    }

    fn remember_expired(&self, key: [u8; 32], now: u64) {
        let mut expired = self.expired.lock();
        expired.retain(|_, at| now.saturating_sub(*at) < EXPIRED_MEMORY_SECS);
        if expired.len() >= MAX_EXPIRED_REMEMBERED
            && let Some(oldest) = expired.iter().min_by_key(|(_, at)| **at).map(|(k, _)| *k)
        {
            expired.remove(&oldest);
        }
        expired.insert(key, now);
    }

    /// Whether `key` named a session that expired recently.
    fn expired_recently(&self, key: &[u8; 32], now: u64) -> bool {
        self.expired
            .lock()
            .get(key)
            .is_some_and(|at| now.saturating_sub(*at) < EXPIRED_MEMORY_SECS)
    }

    /// End a session because it expired or lost its authority, remembering
    /// that it did.
    fn expire_key(&self, key: &[u8; 32]) {
        if self.remove_key(key) {
            self.remember_expired(*key, now_unix());
        }
    }

    /// Whether a new sign-in could land now. Checked before the code
    /// exchange so a refusal costs no round trip to the identity provider;
    /// [`Self::insert`] checks again.
    pub(crate) fn has_room(&self) -> bool {
        let mut sessions = self.sessions.lock();
        self.sweep(&mut sessions, now_unix());
        sessions.len() < MAX_SESSIONS
    }

    /// Store a verified sign-in and return the cookie value that names it.
    ///
    /// A principal at its own cap gives up its oldest session. Nobody else's
    /// session is ever evicted: at the global cap the sign-in is refused.
    fn insert(&self, new: NewSession, now: u64) -> Result<(String, Arc<SessionAuth>), StoreFull> {
        let NewSession {
            alias,
            provider,
            bearer,
            auth,
            authority,
            display_name,
            lifetime_secs,
        } = new;
        let mut sessions = self.sessions.lock();
        self.sweep(&mut sessions, now);
        let principal = auth.principal.id.clone();
        let mut own: Vec<([u8; 32], u64)> = sessions
            .iter()
            .filter(|(_, session)| session.auth.principal.id == principal)
            .map(|(key, session)| (*key, session.created))
            .collect();
        if own.len() >= MAX_SESSIONS_PER_PRINCIPAL {
            own.sort_by_key(|(_, created)| *created);
            let excess = own.len() + 1 - MAX_SESSIONS_PER_PRINCIPAL;
            for (key, _) in own.into_iter().take(excess) {
                if let Some(evicted) = sessions.remove(&key) {
                    evicted.ended.cancel();
                }
            }
        }
        if sessions.len() >= MAX_SESSIONS {
            return Err(StoreFull);
        }
        let cookie = random_token();
        let key = session_key(&cookie);
        let session = Session {
            alias,
            provider,
            bearer,
            auth,
            authority,
            csrf: random_token(),
            display_name,
            created: now,
            hard_expiry: now.saturating_add(lifetime_secs),
            ended: CancellationToken::new(),
            core: Arc::default(),
        };
        let view = Arc::new(SessionAuth::from_session(key, &session, None));
        sessions.insert(key, session);
        self.occupied
            .store(true, std::sync::atomic::Ordering::Release);
        Ok((cookie, view))
    }

    /// End every session: sign-in was turned off. A later re-offer of the
    /// same provider must not bring them back.
    fn end_all(&self) {
        if !self.occupied.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let mut sessions = self.sessions.lock();
        for (_, session) in sessions.drain() {
            session.ended.cancel();
        }
        self.occupied
            .store(false, std::sync::atomic::Ordering::Release);
    }

    fn remove_key(&self, key: &[u8; 32]) -> bool {
        match self.sessions.lock().remove(key) {
            Some(session) => {
                session.ended.cancel();
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.sessions.lock().len()
    }
}

/// What the routes see of a valid session: who it is and what it may do
/// now, never the token behind it.
#[derive(Clone)]
pub struct SessionAuth {
    key: [u8; 32],
    pub alias: String,
    /// The verified binding, for routes that re-authorize at their effect
    /// (the config routes do so under the config write lock).
    pub connection: ConnectionAuth,
    /// The grants the principal holds under the policy in force when this
    /// request was admitted.
    pub grants: ResolvedGrants,
    pub display_name: String,
    pub csrf: String,
    /// When the session ends unless renewed by signing in again (UNIX
    /// seconds): the earlier of the token's expiry and the session cap.
    pub expires_at: u64,
    /// Cancelled when the session ends.
    pub ended: CancellationToken,
}

impl std::fmt::Debug for SessionAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionAuth")
            .field("alias", &self.alias)
            .field("principal", &self.connection.principal.id)
            .field("admin", &self.grants.admin)
            .field("csrf", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl SessionAuth {
    fn from_session(key: [u8; 32], session: &Session, grants: Option<ResolvedGrants>) -> Self {
        Self {
            key,
            alias: session.alias.clone(),
            connection: session.auth.clone(),
            grants: grants.unwrap_or_else(|| session.auth.grants.clone()),
            display_name: session.display_name.clone(),
            csrf: session.csrf.clone(),
            expires_at: session.expires_at(),
            ended: session.ended.clone(),
        }
    }

    pub fn principal(&self) -> &Principal {
        &self.connection.principal
    }

    /// Operator-equivalent: the grant set the shared operator itself holds.
    pub fn is_admin(&self) -> bool {
        self.grants.admin
    }
}

/// Why a request that carried a session cookie is not signed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRejection {
    /// The session ended: token expiry, the session cap, a refused
    /// revalidation, or a policy that no longer grants the principal
    /// anything. Signing in again may succeed.
    Expired,
    /// No such session (signed out, lost on restart, or never issued), or
    /// its provider is no longer offered.
    Invalid,
    /// A request that changes state without the session's CSRF token.
    Csrf,
    /// A request, or a WebSocket upgrade, from another origin.
    Origin,
}

impl SessionRejection {
    fn reason(self) -> &'static str {
        match self {
            Self::Expired => "session_expired",
            Self::Invalid => "session_invalid",
            Self::Csrf => "csrf",
            Self::Origin => "origin",
        }
    }

    fn status(self) -> StatusCode {
        match self {
            Self::Expired | Self::Invalid => StatusCode::UNAUTHORIZED,
            Self::Csrf | Self::Origin => StatusCode::FORBIDDEN,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Expired => "Your dashboard sign-in has expired. Sign in again.",
            Self::Invalid => "This dashboard sign-in is no longer valid. Sign in again.",
            Self::Csrf => {
                "Signed-in requests that change state must send the X-ZeroClaw-CSRF header"
            }
            Self::Origin => "Signed-in requests must come from the dashboard's own origin",
        }
    }

    pub(crate) fn body(self) -> (StatusCode, Json<serde_json::Value>) {
        (
            self.status(),
            Json(serde_json::json!({ "error": self.message(), "reason": self.reason() })),
        )
    }
}

/// How the request that carried a session cookie is classified. Absent
/// (no extension, no task-local) when sign-in is off, the request carried
/// no cookie, or it carried an `Authorization` header, which always wins.
#[derive(Clone, Debug)]
pub enum DashboardCaller {
    Session(Arc<SessionAuth>),
    Rejected(SessionRejection),
}

tokio::task_local! {
    /// The classification of the request being served, for checks that
    /// see only the request headers (`require_auth`). Set by [`layer`]
    /// alone, around the rest of the request.
    static DASHBOARD_CALLER: DashboardCaller;
}

/// The current request's classification, when it carried a session cookie
/// and sign-in is on.
pub(crate) fn current_caller() -> Option<DashboardCaller> {
    DASHBOARD_CALLER.try_with(Clone::clone).ok()
}

/// The 403 for a signed-in account that is not an administrator, on a
/// route only operators may use.
pub(crate) fn operator_required() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "This page needs an administrator account",
            "reason": "operator_required",
        })),
    )
}

impl DashboardCaller {
    /// Admission to an operator route: an administrator session passes, any
    /// other session gets 403 with a stable reason, and a rejected cookie
    /// answers with its own reason instead of the pairing prompt.
    pub(crate) fn admit_operator(&self) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
        match self {
            Self::Session(session) if session.is_admin() => Ok(()),
            Self::Session(_) => Err(operator_required()),
            Self::Rejected(rejection) => Err(rejection.body()),
        }
    }
}

/// The dashboard's sign-in state for one gateway run: the process-shared
/// sessions and this generation's inbound authority.
pub struct DashboardAuth {
    pub(crate) sessions: Arc<DashboardSessions>,
    pub(crate) inbound: Arc<GatewayInboundAuth>,
    /// The stop signal of the generation this state was built for,
    /// subscribed before that generation served anything. Every clone
    /// therefore sees a stop that came after that, including one that came
    /// before the clone was taken; the shared channel's current value alone
    /// cannot say whether this generation was stopped.
    stopping: Option<tokio::sync::watch::Receiver<bool>>,
    _sweeper: SweeperGuard,
}

/// How often the store is swept when no request arrives: an expired
/// session, and the token it holds, is gone within this long of expiring.
const IDLE_SWEEP: std::time::Duration = std::time::Duration::from_secs(60);

/// A gateway generation's idle sweep, stopped when the generation's
/// [`DashboardAuth`] is dropped, so reloads do not pile up sweepers.
struct SweeperGuard(Option<tokio::task::AbortHandle>);

impl SweeperGuard {
    fn start(
        sessions: &Arc<DashboardSessions>,
        config: zeroclaw_config::live::LiveConfigHandle,
        mut stopping: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        // Built outside a runtime (no requests can arrive either): the
        // sweeps on admission and sign-in are all there is.
        if tokio::runtime::Handle::try_current().is_err() {
            return Self(None);
        }
        let sessions = Arc::downgrade(sessions);
        let task = zeroclaw_spawn::spawn!(async move {
            let start = tokio::time::Instant::now() + IDLE_SWEEP;
            let mut ticks = tokio::time::interval_at(start, IDLE_SWEEP);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                // A stopped generation sweeps no more: its configuration no
                // longer governs the sessions in the shared store.
                tokio::select! {
                    biased;
                    () = SessionWatch::generation_stopped(&mut stopping) => break,
                    _ = ticks.tick() => {}
                }
                let Some(sessions) = sessions.upgrade() else {
                    break;
                };
                // Sign-in turned off with no request since still ends every
                // session, so turning it back on cannot bring them back.
                if !config.read().gateway.dashboard_sign_in.enabled() {
                    sessions.end_all();
                }
                sessions.sweep_now(now_unix());
            }
        });
        Self(Some(task.abort_handle()))
    }
}

impl Drop for SweeperGuard {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// Why a sign-in that the identity provider completed did not become a
/// session. Each maps to a fixed page; details go to the log only.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SignInRefusal {
    /// The core's provider refused the access token, or the policy grants
    /// the identity nothing.
    Denied,
    /// A service principal: services never hold browser sessions.
    Service,
    /// The store is at its global cap.
    Full,
}

impl DashboardAuth {
    /// Sign-in state over `sessions`, verified by `inbound`. A served
    /// gateway binds it to its generation with [`Self::for_generation`].
    pub fn new(sessions: Arc<DashboardSessions>, inbound: Arc<GatewayInboundAuth>) -> Self {
        Self {
            sessions,
            inbound,
            stopping: None,
            _sweeper: SweeperGuard(None),
        }
    }

    /// Bind this state to the gateway generation `state` belongs to, before
    /// that generation serves anything: its stop retires every session watch
    /// and refuses cookie requests from then on, a generation built with
    /// sign-in off ends every session, and the generation's idle sweep runs
    /// until this state is dropped.
    pub fn for_generation(mut self, state: &AppState) -> Self {
        let stopping = state.shutdown_tx.subscribe();
        if !state.config.read().gateway.dashboard_sign_in.enabled() {
            self.sessions.end_all();
        }
        self._sweeper = SweeperGuard::start(&self.sessions, state.config.clone(), stopping.clone());
        self.stopping = Some(stopping);
        self
    }

    /// The stop signal of the generation this state is bound to, for a task
    /// that must end with it. `None` for a state bound to no generation
    /// ([`DashboardAuth::new`] alone, in unit tests): treat it as none. The
    /// clone keeps the unseen version, so a stop sent before the call is
    /// still seen.
    pub(crate) fn generation_stopping(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.stopping.clone()
    }

    /// Whether this state's generation was told to stop. A closed channel
    /// counts: once its sender is gone the generation is over.
    pub(crate) fn is_retired(&self) -> bool {
        self.stopping
            .as_ref()
            .is_some_and(|stopping| stopping.has_changed().unwrap_or(true))
    }

    /// Verify the access token a dashboard sign-in was issued and store the
    /// session. The token is verified by the configured provider exactly as
    /// RPC verifies a bearer; the resolved principal must be a human.
    /// `provider` is the alias's issuer and client as configured now, which
    /// the session is pinned to.
    pub(crate) async fn sign_in(
        &self,
        alias: &str,
        provider: ProviderPin,
        access_token: String,
        lifetime_secs: u64,
    ) -> Result<(String, Arc<SessionAuth>), SignInRefusal> {
        // Wiped on every path out, a refused sign-in included.
        let access_token = Zeroizing::new(access_token);
        let selection = format!("oidc.{alias}");
        let auth = self
            .inbound
            .authenticate_bound(&access_token, &selection, &provider.binding())
            .await
            .map_err(|_| SignInRefusal::Denied)?;
        if auth.principal.actor == ActorKind::Service {
            return Err(SignInRefusal::Service);
        }
        let display_name = display_name(&auth);
        let new = NewSession {
            alias: alias.to_string(),
            provider,
            bearer: access_token,
            auth,
            authority: self.inbound.authority_ref(),
            display_name,
            lifetime_secs,
        };
        self.sessions
            .insert(new, now_unix())
            .map_err(|StoreFull| SignInRefusal::Full)
    }

    /// Check again, from a long-lived socket, the session a request admitted
    /// earlier: the same checks as for a request.
    pub(crate) async fn recheck(&self, state: &AppState, session: &SessionAuth) -> DashboardCaller {
        let offered = offered_providers(state);
        self.admit_key(|alias| provider_for(&offered, alias), session.key)
            .await
    }

    /// The session a cookie names, checked as RPC checks a bound connection
    /// before each operation. A session that fails is ended.
    async fn admit(
        &self,
        offered: impl Fn(&str) -> Option<ProviderPin>,
        cookie: &str,
    ) -> DashboardCaller {
        self.admit_key(offered, session_key(cookie)).await
    }

    /// `offered` answers, for an alias, the provider it names now if the
    /// dashboard still offers it.
    async fn admit_key(
        &self,
        offered: impl Fn(&str) -> Option<ProviderPin>,
        key: [u8; 32],
    ) -> DashboardCaller {
        let now = now_unix();
        let snapshot = {
            let mut sessions = self.sessions.sessions.lock();
            // Expired sessions go now rather than at the next sign-in, so
            // their tokens leave memory with them.
            self.sessions.sweep(&mut sessions, now);
            sessions.get(&key).map(|session| {
                (
                    session.alias.clone(),
                    session.provider.clone(),
                    session.bearer.clone(),
                    session.auth.clone(),
                    session.authority.clone(),
                )
            })
        };
        let Some((alias, pinned, bearer, auth, authority)) = snapshot else {
            return DashboardCaller::Rejected(if self.sessions.expired_recently(&key, now) {
                SessionRejection::Expired
            } else {
                SessionRejection::Invalid
            });
        };
        // Taken off the sign-in screen, removed, or repointed at another
        // issuer or client: the session ends before its token could be sent
        // to a provider that did not issue it.
        if offered(&alias).as_ref() != Some(&pinned) {
            self.sessions.remove_key(&key);
            return DashboardCaller::Rejected(SessionRejection::Invalid);
        }
        let due = auth
            .principal
            .revalidate_by
            .is_some_and(|deadline| deadline <= now);
        let auth = if due || !self.inbound.is_authority(&authority) {
            match self.reverify(&key, &alias, &pinned, &bearer, &auth).await {
                Some(fresh) => fresh,
                None => return DashboardCaller::Rejected(SessionRejection::Expired),
            }
        } else {
            auth
        };
        let grants = match self.inbound.current_grants(&auth) {
            Ok(grants) => grants,
            // The accepted policy can move between a resolution and its
            // check; that is no reason to sign somebody out. Verify once
            // more with the token held, then fail closed.
            Err(_) => match self.reverify(&key, &alias, &pinned, &bearer, &auth).await {
                Some(fresh) => match self.inbound.current_grants(&fresh) {
                    Ok(grants) => grants,
                    Err(denied) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "alias": alias,
                                "principal_id": fresh.principal.id.as_str(),
                                "refusal": {"code": denied.code, "reason": denied.message},
                            })),
                            "dashboard session ended: the current policy grants it nothing"
                        );
                        self.sessions.expire_key(&key);
                        return DashboardCaller::Rejected(SessionRejection::Expired);
                    }
                },
                None => return DashboardCaller::Rejected(SessionRejection::Expired),
            },
        };
        // Verifying again can take long enough for the session's hard
        // expiry to pass meanwhile, with a token that is still valid.
        let checked_at = now_unix();
        let admitted = {
            let sessions = self.sessions.sessions.lock();
            match sessions.get(&key) {
                Some(session) if session.expires_at() <= checked_at => None,
                Some(session) => Some(DashboardCaller::Session(Arc::new(
                    SessionAuth::from_session(key, session, Some(grants)),
                ))),
                // Signed out while this request was being checked.
                None => Some(DashboardCaller::Rejected(SessionRejection::Invalid)),
            }
        };
        admitted.unwrap_or_else(|| {
            self.sessions.expire_key(&key);
            DashboardCaller::Rejected(SessionRejection::Expired)
        })
    }

    /// Verify the held token again against the current authority and keep
    /// the result. A refusal, or a token that now names another principal,
    /// ends the session. The token only ever goes to the provider it was
    /// issued by (`pinned`), checked against the policy that would send it.
    async fn reverify(
        &self,
        key: &[u8; 32],
        alias: &str,
        pinned: &ProviderPin,
        bearer: &str,
        previous: &ConnectionAuth,
    ) -> Option<ConnectionAuth> {
        let provider = format!("oidc.{alias}");
        let binding = pinned.binding();
        let published = self.inbound.generation();
        let mut verified = self
            .inbound
            .authenticate_bound(bearer, &provider, &binding)
            .await;
        // A policy published during the identity provider's round trip fails
        // the verification closed, as it should; that is no reason to sign
        // somebody out. Verify once more with the token held, as the grants
        // check does, then fail closed.
        if verified.is_err() && self.inbound.generation() != published {
            verified = self
                .inbound
                .authenticate_bound(bearer, &provider, &binding)
                .await;
        }
        let refusal = match verified {
            Ok(fresh)
                if fresh.principal.id == previous.principal.id
                    && fresh.principal.actor != ActorKind::Service =>
            {
                let mut sessions = self.sessions.sessions.lock();
                let session = sessions.get_mut(key)?;
                session.auth = fresh.clone();
                session.authority = self.inbound.authority_ref();
                return Some(fresh);
            }
            Ok(_) => ::serde_json::json!({"reason": "the token now names another principal"}),
            Err(denied) => ::serde_json::json!({"code": denied.code, "reason": denied.message}),
        };
        // Why a session ended is otherwise invisible: the browser only learns
        // that it expired. The token itself is never logged.
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "alias": alias,
                    "principal_id": previous.principal.id.as_str(),
                    "refusal": refusal,
                })),
            "dashboard session ended: verifying its token again failed"
        );
        self.sessions.expire_key(key);
        None
    }

    /// End the session behind `session` (sign-out).
    pub(crate) fn end(&self, session: &SessionAuth) -> bool {
        self.sessions.remove_key(&session.key)
    }

    /// The token `session` signed in with and the provider it was issued
    /// by, for its chats' core connections to present, bound to that
    /// provider, and nothing else. The copy is wiped when dropped like the
    /// stored one. `None` once the session has ended.
    pub(crate) fn core_credential(
        &self,
        session: &SessionAuth,
    ) -> Option<crate::dashboard_core::CoreCredential> {
        self.sessions.sessions.lock().get(&session.key).map(|held| {
            crate::dashboard_core::CoreCredential {
                bearer: held.bearer.clone(),
                pin: held.provider.binding(),
            }
        })
    }

    /// The chats `session` has open on the core. `None` once the session
    /// has ended.
    pub(crate) fn core_binding(
        &self,
        session: &SessionAuth,
    ) -> Option<Arc<crate::dashboard_core::CoreBinding>> {
        self.sessions
            .sessions
            .lock()
            .get(&session.key)
            .map(|held| Arc::clone(&held.core))
    }
}

/// A display name for the header, from the verified access-token claims.
/// Cosmetic only: it never authorizes anything.
fn display_name(auth: &ConnectionAuth) -> String {
    ["name", "preferred_username", "email"]
        .iter()
        .filter_map(|claim| auth.identity.claims.get(*claim)?.as_str())
        .map(str::trim)
        .find(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .map(|value| value.chars().take(MAX_DISPLAY_NAME_CHARS).collect())
        .unwrap_or_else(|| auth.principal.display_id.clone())
}

/// Read one cookie's value from the request.
pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The scheme, host and path a dashboard cookie is scoped to.
pub(crate) struct CookieScope<'a> {
    pub https: bool,
    pub path_prefix: &'a str,
}

impl CookieScope<'_> {
    /// Whether this deployment can use the `__Host-` prefix: HTTPS at the
    /// root path. Then only `__Host-` names are read, so a cookie a sibling
    /// host planted under the plain name is never consulted.
    fn host_prefixed(&self) -> bool {
        self.https && self.path_prefix.is_empty()
    }

    fn session_name(&self) -> &'static str {
        if self.host_prefixed() {
            HOST_SESSION_COOKIE
        } else {
            SESSION_COOKIE
        }
    }

    fn secure(&self) -> &'static str {
        if self.https { "; Secure" } else { "" }
    }

    /// The session cookie the request carries, under this scope's one name.
    pub(crate) fn session_value(&self, headers: &HeaderMap) -> Option<String> {
        cookie_value(headers, self.session_name())
    }

    /// `Set-Cookie` for a new session. No `Max-Age`: the browser keeps it
    /// for the browsing session, and the server-side expiry is the
    /// authority.
    pub(crate) fn session_cookie(&self, value: &str) -> HeaderValue {
        let header = format!(
            "{}={value}; Path={}/; HttpOnly; SameSite=Strict{}",
            self.session_name(),
            self.path_prefix,
            self.secure()
        );
        HeaderValue::from_str(&header).unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    /// `Set-Cookie` values that drop the session cookie under both names.
    pub(crate) fn clear_session_cookies(&self) -> Vec<HeaderValue> {
        let mut cleared = vec![format!(
            "{SESSION_COOKIE}=; Path={}/; HttpOnly; SameSite=Strict; Max-Age=0{}",
            self.path_prefix,
            self.secure()
        )];
        if self.https {
            cleared.push(format!(
                "{HOST_SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0; Secure"
            ));
        }
        cleared
            .into_iter()
            .filter_map(|value| HeaderValue::from_str(&value).ok())
            .collect()
    }

    /// The binding cookie's name and path for the flow with `state`. Under
    /// `__Host-` it must be `Path=/`; otherwise it is scoped to the
    /// callback.
    fn binding_name_and_path(&self, state: &str) -> (String, String) {
        let tag = hex::encode(&Sha256::digest(state.as_bytes())[..4]);
        if self.host_prefixed() {
            (
                format!("__Host-{BINDING_COOKIE_STEM}{tag}"),
                "/".to_string(),
            )
        } else {
            (
                format!("{BINDING_COOKIE_STEM}{tag}"),
                format!("{}/oidc/callback", self.path_prefix),
            )
        }
    }

    /// The binding cookie the callback for `state` carries, if any.
    pub(crate) fn binding_value(&self, headers: &HeaderMap, state: &str) -> Option<String> {
        cookie_value(headers, &self.binding_name_and_path(state).0)
    }

    /// `Set-Cookie` for a sign-in's binding. `Lax`, because the callback is
    /// a cross-site top-level navigation from the identity provider and a
    /// `Strict` cookie would not be sent with it; it lives as long as the
    /// flow.
    pub(crate) fn binding_cookie(
        &self,
        state: &str,
        value: &str,
        max_age_secs: u64,
    ) -> HeaderValue {
        let (name, path) = self.binding_name_and_path(state);
        let header = format!(
            "{name}={value}; Path={path}; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{}",
            self.secure()
        );
        HeaderValue::from_str(&header).unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    pub(crate) fn clear_binding_cookie(&self, state: &str) -> HeaderValue {
        let (name, path) = self.binding_name_and_path(state);
        let header = format!(
            "{name}=; Path={path}; HttpOnly; SameSite=Lax; Max-Age=0{}",
            self.secure()
        );
        HeaderValue::from_str(&header).unwrap_or_else(|_| HeaderValue::from_static(""))
    }
}

/// `scheme://host` exactly as a browser would put it in `Origin`: lower
/// case, default port omitted.
pub(crate) fn origin_of(https: bool, host: &str) -> String {
    let scheme = if https { "https" } else { "http" };
    let host = host.trim().to_ascii_lowercase();
    let default_port = if https { ":443" } else { ":80" };
    let host = host.strip_suffix(default_port).unwrap_or(&host);
    format!("{scheme}://{host}")
}

fn is_unsafe(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

/// Where a cookie-carrying request came from, checked before any session
/// state is touched, so a cross-site request costs nothing.
///
/// Cookies ride along on WebSocket handshakes, and `SameSite` does not tell
/// two ports of one host apart, so `Origin` is the check that matters: it
/// must name this dashboard whenever present, and must be present on
/// requests that change state and on upgrades. `Sec-Fetch-Site`, where
/// sent, must say the request came from this origin (or, for a plain
/// navigation, from the user).
fn check_origin(
    method: &Method,
    headers: &HeaderMap,
    own_origin: &str,
) -> Result<(), SessionRejection> {
    let strict = is_unsafe(method) || is_websocket_upgrade(headers);
    match headers.get(header::ORIGIN).map(|v| v.to_str()) {
        Some(Ok(origin)) if origin.eq_ignore_ascii_case(own_origin) => {}
        Some(_) => return Err(SessionRejection::Origin),
        None if strict => return Err(SessionRejection::Origin),
        None => {}
    }
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        None | Some("same-origin") => Ok(()),
        Some("none") if !strict => Ok(()),
        Some(_) => Err(SessionRejection::Origin),
    }
}

fn check_csrf(
    method: &Method,
    headers: &HeaderMap,
    session: &SessionAuth,
) -> Result<(), SessionRejection> {
    if !is_unsafe(method) {
        return Ok(());
    }
    match headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()) {
        Some(presented) if zeroclaw_config::pairing::constant_time_eq(presented, &session.csrf) => {
            Ok(())
        }
        _ => Err(SessionRejection::Csrf),
    }
}

/// The providers the dashboard offers now that are still configured, with
/// the issuer and client each alias names now.
fn offered_providers(state: &AppState) -> Vec<(String, ProviderPin)> {
    let config = state.config.read();
    config
        .gateway
        .dashboard_sign_in
        .oidc
        .iter()
        .filter_map(|alias| {
            let entry = config.oidc.get(alias)?;
            Some((alias.clone(), ProviderPin::of(entry)))
        })
        .collect()
}

fn provider_for(offered: &[(String, ProviderPin)], alias: &str) -> Option<ProviderPin> {
    offered
        .iter()
        .find(|(offered, _)| offered == alias)
        .map(|(_, pin)| pin.clone())
}

/// How often a long-lived socket checks its session again, so a policy
/// change or a removed provider reaches a socket that sends nothing.
const SOCKET_RECHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// WebSocket close code for a session that ended (signed out, expired, or
/// no longer valid): the client must not reconnect on its own.
pub(crate) const WS_CLOSE_SESSION_ENDED: u16 = 4401;

/// WebSocket close code for a session that is still valid but no longer
/// admits this socket (the account lost the access the socket needs).
pub(crate) const WS_CLOSE_ACCESS_LOST: u16 = 4403;

/// A socket's periodic recheck, run in a task of its own. The task ends,
/// and drops the token copy it verifies with, as soon as the session ends,
/// expires or loses its generation, whether or not the socket ever polls
/// for the result: a stalled writer or a response body the transport
/// stopped polling cannot keep it. Dropping this aborts the task.
struct RecheckTask(tokio::task::JoinHandle<Option<DashboardCaller>>);

impl Drop for RecheckTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A task that is aborted when this is dropped.
pub(crate) struct TaskGuard(pub(crate) tokio::task::JoinHandle<()>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// WebSocket close code for a socket whose gateway generation stopped (a
/// reload or shutdown): the client may reconnect, and the next generation's
/// authority decides whether it may.
pub(crate) const WS_CLOSE_GOING_AWAY: u16 = 1001;

/// A long-lived socket's view of the dashboard session that opened it.
///
/// It ends the socket's authority on any of: the session ending (sign-out,
/// eviction, revocation), its expiry, a recheck the current policy fails,
/// or the socket's own gateway generation stopping. The last matters
/// because a reload builds a new authority: a socket that outlived its
/// generation would otherwise keep checking against the policy it was
/// opened under.
pub(crate) struct SessionWatch {
    auth: Arc<DashboardAuth>,
    session: Arc<SessionAuth>,
    require_admin: bool,
    recheck: tokio::time::Interval,
    /// A recheck in progress. Kept here, not in a `lost` future, because
    /// a socket's loop drops that future whenever another branch fires: a
    /// busy socket would otherwise abandon every recheck that needs a round
    /// trip to the identity provider.
    pending: Option<RecheckTask>,
    /// The generation's stop signal (`AppState::shutdown_tx`), subscribed
    /// when the socket opened.
    stopping: tokio::sync::watch::Receiver<bool>,
    stopped: bool,
}

impl SessionWatch {
    pub(crate) fn new(
        auth: Arc<DashboardAuth>,
        session: Arc<SessionAuth>,
        require_admin: bool,
        state: &AppState,
    ) -> Self {
        let start = tokio::time::Instant::now() + SOCKET_RECHECK;
        let mut recheck = tokio::time::interval_at(start, SOCKET_RECHECK);
        recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The generation's own signal when the state is bound to one (every
        // served gateway); otherwise this state's, from now.
        let stopping = auth
            .stopping
            .clone()
            .unwrap_or_else(|| state.shutdown_tx.subscribe());
        Self {
            auth,
            session,
            require_admin,
            recheck,
            pending: None,
            stopping,
            stopped: false,
        }
    }

    pub(crate) fn principal_id(&self) -> &str {
        self.session.principal().id.as_str()
    }

    /// The ends a stream can see by itself, without waiting for the task
    /// that watches it: the session's own cancellation (sign-out, eviction,
    /// revocation) and its generation's stop. Neither holds anything of the
    /// session's credentials.
    pub(crate) fn direct_ends(&self) -> (CancellationToken, tokio::sync::watch::Receiver<bool>) {
        (self.session.ended.clone(), self.stopping.clone())
    }

    /// Resolves when this generation is told to stop. Waits for a new signal
    /// rather than reading the current value, like the gateway's listener;
    /// never resolves if the signal can no longer arrive.
    pub(crate) async fn generation_stopped(stopping: &mut tokio::sync::watch::Receiver<bool>) {
        if stopping.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// The close code if the session no longer admits the socket by what is
    /// already known (ended, expired, generation stopped), without a recheck.
    fn known_lost(&mut self) -> Option<u16> {
        if self.session.ended.is_cancelled() || self.session.expires_at <= now_unix() {
            return Some(WS_CLOSE_SESSION_ENDED);
        }
        if self.stopped || self.stopping.has_changed().unwrap_or(true) {
            self.stopped = true;
            return Some(WS_CLOSE_GOING_AWAY);
        }
        None
    }

    fn judge(&mut self, outcome: DashboardCaller) -> Result<(), u16> {
        match outcome {
            DashboardCaller::Session(session) if !self.require_admin || session.is_admin() => {
                self.session = session;
                Ok(())
            }
            DashboardCaller::Session(_) => Err(WS_CLOSE_ACCESS_LOST),
            DashboardCaller::Rejected(_) => Err(WS_CLOSE_SESSION_ENDED),
        }
    }

    /// Check the session now, before acting on input the socket sent: the
    /// same checks as a request, against the current policy, rather than
    /// waiting for the periodic recheck.
    pub(crate) async fn admits_now(&mut self, state: &AppState) -> Result<(), u16> {
        let verdict = self.check_now(state).await;
        if verdict.is_err() {
            // Nothing of a session that no longer admits the socket is kept.
            self.pending = None;
        }
        verdict
    }

    async fn check_now(&mut self, state: &AppState) -> Result<(), u16> {
        if let Some(code) = self.known_lost() {
            return Err(code);
        }
        self.pending = None;
        // A check that has to verify again can take a while. The session
        // ending, its expiry and the generation's stop each cut it short
        // rather than wait for it to admit the frame afterwards.
        let remaining = self.session.expires_at.saturating_sub(now_unix());
        let outcome = tokio::select! {
            biased;
            () = self.session.ended.cancelled() => return Err(WS_CLOSE_SESSION_ENDED),
            () = tokio::time::sleep(std::time::Duration::from_secs(remaining)) => {
                return Err(WS_CLOSE_SESSION_ENDED);
            }
            () = Self::generation_stopped(&mut self.stopping) => {
                self.stopped = true;
                return Err(WS_CLOSE_GOING_AWAY);
            }
            outcome = self.auth.recheck(state, &self.session) => outcome,
        };
        self.judge(outcome)?;
        // Whatever ended while the check ran still ends the socket.
        self.known_lost().map_or(Ok(()), Err)
    }

    /// Resolves with the close code once the session stops admitting the
    /// socket. Cancel-safe: dropping the future loses nothing, since a
    /// started recheck lives in `self.pending` and the next call resumes it.
    async fn lost(&mut self, state: &AppState) -> u16 {
        let code = self.wait_lost(state).await;
        // Over for good: drop a recheck still in flight, and the token copy
        // it holds, instead of keeping them for as long as the watch lives.
        self.pending = None;
        code
    }

    fn spawn_recheck(&self, state: &AppState) -> RecheckTask {
        let auth = Arc::clone(&self.auth);
        let session = Arc::clone(&self.session);
        let state = state.clone();
        let mut stopping = self.stopping.clone();
        RecheckTask(zeroclaw_spawn::spawn!(async move {
            let remaining = session.expires_at.saturating_sub(now_unix());
            tokio::select! {
                biased;
                () = session.ended.cancelled() => None,
                () = tokio::time::sleep(std::time::Duration::from_secs(remaining)) => None,
                () = SessionWatch::generation_stopped(&mut stopping) => None,
                outcome = auth.recheck(&state, &session) => Some(outcome),
            }
        }))
    }

    async fn wait_lost(&mut self, state: &AppState) -> u16 {
        loop {
            if let Some(code) = self.known_lost() {
                return code;
            }
            let remaining = self.session.expires_at.saturating_sub(now_unix());
            let expiry = tokio::time::sleep(std::time::Duration::from_secs(remaining));
            let joined = match self.pending.as_mut() {
                Some(pending) => tokio::select! {
                    () = self.session.ended.cancelled() => return WS_CLOSE_SESSION_ENDED,
                    () = expiry => return WS_CLOSE_SESSION_ENDED,
                    () = Self::generation_stopped(&mut self.stopping) => {
                        self.stopped = true;
                        return WS_CLOSE_GOING_AWAY;
                    }
                    joined = &mut pending.0 => joined,
                },
                None => {
                    tokio::select! {
                        () = self.session.ended.cancelled() => return WS_CLOSE_SESSION_ENDED,
                        () = expiry => return WS_CLOSE_SESSION_ENDED,
                        () = Self::generation_stopped(&mut self.stopping) => {
                            self.stopped = true;
                            return WS_CLOSE_GOING_AWAY;
                        }
                        _ = self.recheck.tick() => {}
                    }
                    self.pending = Some(self.spawn_recheck(state));
                    continue;
                }
            };
            self.pending = None;
            let outcome = match joined {
                Ok(Some(outcome)) => outcome,
                // The recheck saw the session end, expire or lose its
                // generation first; a recheck that failed to run fails closed.
                Ok(None) | Err(_) => {
                    return self.known_lost().unwrap_or(WS_CLOSE_SESSION_ENDED);
                }
            };
            if let Err(code) = self.judge(outcome) {
                return code;
            }
        }
    }
}

/// [`SessionWatch::lost`] for a socket that may have no session: one opened
/// with a pairing token, or with pairing off, is never closed by this.
pub(crate) async fn session_lost(watch: &mut Option<SessionWatch>, state: &AppState) -> u16 {
    match watch {
        Some(watch) => watch.lost(state).await,
        None => std::future::pending().await,
    }
}

/// [`SessionWatch::admits_now`] for a socket that may have no session; a
/// socket without one is always admitted here (its own credential check
/// already ran when it opened).
pub(crate) async fn session_admits_now(
    watch: &mut Option<SessionWatch>,
    state: &AppState,
) -> Result<(), u16> {
    match watch {
        Some(watch) => watch.admits_now(state).await,
        None => Ok(()),
    }
}

/// Admission for an operator route that checks pairing itself instead of
/// through `require_auth`, called when the request presented no token.
pub(crate) enum OperatorSession {
    /// No dashboard session is in play: the route's own check decides.
    Absent,
    /// An administrator session. For a long-lived stream, the watch that
    /// ends it with the session.
    Admitted(Option<SessionWatch>),
    /// A session that may not use the route, with its answer.
    Denied(Response),
}

pub(crate) fn operator_session(
    dashboard: Option<&Arc<DashboardAuth>>,
    state: &AppState,
) -> OperatorSession {
    let Some(caller) = current_caller() else {
        return OperatorSession::Absent;
    };
    if let Err(denied) = caller.admit_operator() {
        return OperatorSession::Denied(denied.into_response());
    }
    match (caller, dashboard) {
        (DashboardCaller::Session(session), Some(auth)) => OperatorSession::Admitted(Some(
            SessionWatch::new(Arc::clone(auth), session, true, state),
        )),
        _ => OperatorSession::Admitted(None),
    }
}

/// Router-wide state for [`layer`].
#[derive(Clone)]
pub struct DashboardLayer {
    pub state: AppState,
    pub auth: Arc<DashboardAuth>,
}

/// Classify every request that carries a session cookie; see the module
/// docs. Installed on the whole router, so it covers every route,
/// WebSocket upgrades and the fallback included.
pub async fn layer(
    State(layer): State<DashboardLayer>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(Arc::clone(&layer.auth));
    let enabled = layer
        .state
        .config
        .read()
        .gateway
        .dashboard_sign_in
        .enabled();
    if !enabled {
        // Turning sign-in off ends every session, so re-offering a provider
        // later cannot bring them back. No cookie is read. Only the active
        // generation may: a stopped one's configuration is history, and the
        // sessions in the shared store now belong to the one after it.
        if !layer.auth.is_retired() {
            layer.auth.sessions.end_all();
        }
        return next.run(request).await;
    }
    if request.headers().contains_key(header::AUTHORIZATION) {
        return next.run(request).await;
    }
    let https = crate::api_oidc::request_is_https(&layer.state, request.headers());
    let scope = CookieScope {
        https,
        path_prefix: &layer.state.path_prefix,
    };
    let Some(cookie) = scope.session_value(request.headers()) else {
        return next.run(request).await;
    };
    // A stopped generation admits nobody. The transport already refuses a
    // request first served after the stop; this catches one it admitted
    // just before.
    if layer.auth.is_retired() {
        return crate::gateway_restarting(request.version());
    }
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let own_origin = origin_of(https, &host);
    let caller = match check_origin(request.method(), request.headers(), &own_origin) {
        Err(rejection) => DashboardCaller::Rejected(rejection),
        Ok(()) => {
            let offered = offered_providers(&layer.state);
            let caller = layer
                .auth
                .admit(|alias| provider_for(&offered, alias), &cookie)
                .await;
            if layer.auth.is_retired() {
                // The gateway's own restart answer, shaped for the protocol.
                return crate::gateway_restarting(request.version());
            }
            match caller {
                DashboardCaller::Session(session) => {
                    match check_csrf(request.method(), request.headers(), &session) {
                        Ok(()) => DashboardCaller::Session(session),
                        Err(rejection) => DashboardCaller::Rejected(rejection),
                    }
                }
                rejected => rejected,
            }
        }
    };
    if let DashboardCaller::Session(session) = &caller {
        request.extensions_mut().insert(Arc::clone(session));
    }
    request.extensions_mut().insert(caller.clone());
    // A cookie whose session ended is left in place, not cleared: it names
    // nothing and holds no token, and keeping it lets the browser's other
    // tabs and later requests hear why ("expired") instead of the answer for
    // no credential at all. Sign-out clears it; a new sign-in replaces it.
    DASHBOARD_CALLER.scope(caller, next.run(request)).await
}

/// A rejected session's answer as a full response, for routes that answer
/// with [`Response`] rather than the `require_auth` tuple.
pub(crate) fn rejection_response(rejection: SessionRejection) -> Response {
    rejection.body().into_response()
}

/// Shared test scaffolding for dashboard sign-in: a mock identity provider,
/// the sign-in config block, and helpers to drive a real served gateway over
/// sockets with a session cookie.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::time::Duration;

    use futures_util::StreamExt as _;
    use wiremock::matchers::{method as http_method, path as http_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::schema::{Config, OidcConfig, OidcValidation, PermissionProfileConfig};

    pub(crate) type WsClient = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// An identity provider with discovery, an authorization endpoint the
    /// test never visits, and an introspection endpoint that vouches for any
    /// token as `sub` with `groups`, issued to `client_id`.
    pub(crate) async fn idp(groups: &[&str], client_id: &str) -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(http_method("GET"))
            .and(http_path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "introspection_endpoint": format!("{issuer}/introspect"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "token_type": "Bearer",
                "client_id": client_id,
                "iss": issuer,
                "sub": "alice",
                "aud": "zeroclaw",
                "exp": super::now_unix() + 600,
                "groups": groups,
                "preferred_username": "alice@example.com",
            })))
            .mount(&server)
            .await;
        server
    }

    /// From now on, `server` answers introspection only after `delay`, so a
    /// check that has to verify the session again takes that long.
    pub(crate) async fn slow_introspection(
        server: &MockServer,
        groups: &[&str],
        client_id: &str,
        delay: Duration,
    ) {
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({
                        "active": true,
                        "token_type": "Bearer",
                        "client_id": client_id,
                        "iss": server.uri(),
                        "sub": "alice",
                        "aud": "zeroclaw",
                        "exp": super::now_unix() + 600,
                        "groups": groups,
                        "preferred_username": "alice@example.com",
                    }))
                    .set_delay(delay),
            )
            .with_priority(1)
            .mount(server)
            .await;
    }

    /// Verify a `corp` session again once `secs` have passed since it was
    /// last verified. A check after that verifies once and, if the provider
    /// still vouches for the token, admits the session for `secs` more.
    pub(crate) fn revalidate_after(config: &mut Config, secs: u64) {
        config
            .oidc
            .get_mut("corp")
            .expect("sign-in is configured")
            .revalidation_secs = secs;
    }

    /// How long after a verification its revalidation deadline has surely
    /// passed, for a `revalidate_after(config, 2)` session.
    pub(crate) const UNTIL_DUE: Duration = Duration::from_millis(3100);

    /// Turn sign-in on with one `corp` provider at `issuer`: group `admins`
    /// maps to an administrator profile, group `ops` to a config reader.
    pub(crate) fn add_sign_in(config: &mut Config, issuer: &str) {
        config.gateway.require_pairing = true;
        config.gateway.dashboard_sign_in.oidc = vec!["corp".into()];
        config.oidc.insert(
            "corp".into(),
            OidcConfig {
                issuer: issuer.to_string(),
                audience: "zeroclaw".into(),
                client_id: "gw".into(),
                client_secret: Some("s3cret".into()),
                validation: OidcValidation::Introspection,
                claim_path: "groups".into(),
                profile_map: HashMap::from([
                    ("admins".to_string(), "admin".to_string()),
                    ("ops".to_string(), "config-reader".to_string()),
                ]),
                interactive_clients: vec!["gw".into()],
                service_clients: vec!["svc".into()],
                ..OidcConfig::default()
            },
        );
        config.permission_profiles.insert(
            "admin".into(),
            PermissionProfileConfig {
                admin: true,
                ..PermissionProfileConfig::default()
            },
        );
        config.permission_profiles.insert(
            "config-reader".into(),
            PermissionProfileConfig {
                grants: HashMap::from([(Resource::Config, vec![Verb::Read])]),
                ..PermissionProfileConfig::default()
            },
        );
    }

    /// The same config with the `admins` group no longer mapped: an account
    /// in both groups keeps only the config reader's grants.
    pub(crate) fn without_admin_mapping(config: &Config) -> Config {
        let mut revoked = config.clone();
        if let Some(corp) = revoked.oidc.get_mut("corp") {
            corp.profile_map.remove("admins");
        }
        revoked
    }

    /// Serve `router` on a loopback port with peer addresses, as the gateway
    /// serves itself.
    pub(crate) async fn serve(router: axum::Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback test gateway");
        let addr = listener.local_addr().expect("test gateway address");
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test gateway serves");
        });
        (addr, server)
    }

    /// Open a WebSocket on `path` presenting the session `cookie` from the
    /// gateway's own origin, as the dashboard does. `Err` carries the HTTP
    /// status when the upgrade is refused.
    pub(crate) async fn connect(
        addr: SocketAddr,
        path: &str,
        cookie: &str,
    ) -> Result<WsClient, u16> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
        // This URL connects only to the test's loopback listener.
        let mut request = format!("ws://{addr}{path}") // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
            .into_client_request()
            .expect("WebSocket request");
        let headers = request.headers_mut();
        headers.insert("cookie", cookie.parse().expect("cookie header"));
        headers.insert(
            "origin",
            format!("http://{addr}").parse().expect("origin header"),
        );
        match tokio_tungstenite::connect_async(request).await {
            Ok((client, _)) => Ok(client),
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                Err(response.status().as_u16())
            }
            Err(other) => panic!("WebSocket transport failed: {other}"),
        }
    }

    /// Read frames until the server closes; the close code, or `None` when
    /// the deadline passes with the socket still open. Text frames seen on
    /// the way are returned too.
    pub(crate) async fn until_closed(
        client: &mut WsClient,
        deadline: Duration,
    ) -> (Option<u16>, Vec<String>) {
        use tokio_tungstenite::tungstenite::Message as ClientMessage;
        let mut texts = Vec::new();
        let outcome = tokio::time::timeout(deadline, async {
            loop {
                match client.next().await {
                    Some(Ok(ClientMessage::Close(frame))) => {
                        break Some(frame.map_or(1005, |frame| u16::from(frame.code)));
                    }
                    Some(Ok(ClientMessage::Text(text))) => texts.push(text.to_string()),
                    Some(Ok(_)) => {}
                    // The transport ended without a close frame.
                    Some(Err(_)) | None => break Some(1006),
                }
            }
        })
        .await;
        (outcome.ok().flatten(), texts)
    }

    /// `POST /auth/sign-out` over a real connection, as the dashboard sends
    /// it. Returns the HTTP status.
    pub(crate) async fn sign_out_over_http(addr: SocketAddr, cookie: &str, csrf: &str) -> u16 {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect to the test gateway");
        let request = format!(
            "POST /auth/sign-out HTTP/1.1\r\nHost: {addr}\r\nCookie: {cookie}\r\n\
             Origin: http://{addr}\r\n{}: {csrf}\r\nContent-Length: 0\r\n\
             Connection: close\r\n\r\n",
            super::CSRF_HEADER
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("send sign-out");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("read sign-out answer");
        response
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .expect("an HTTP status line")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use http_body_util::BodyExt as _;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tower::ServiceExt as _;
    use wiremock::matchers::{method as http_method, path as http_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::pairing::{PairingCodePolicy, PairingGuard};
    use zeroclaw_config::schema::Config;

    const HOST: &str = "localhost:42617";
    const OWN_ORIGIN: &str = "http://localhost:42617";

    fn loopback() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 39999))
    }

    fn remote() -> SocketAddr {
        SocketAddr::from(([203, 0, 113, 7], 40000))
    }

    fn unix_now() -> u64 {
        now_unix()
    }

    use super::test_support::idp;

    fn sign_in_config(issuer: &str) -> Config {
        let mut config = Config::default();
        config.gateway.paired_tokens = vec!["zc_paired".into()];
        super::test_support::add_sign_in(&mut config, issuer);
        config
    }

    /// Stands in for any operator route: answers exactly as `require_auth`
    /// decides.
    async fn operator_probe(State(state): State<AppState>, headers: HeaderMap) -> Response {
        match crate::api::require_auth(&state, &headers) {
            Ok(()) => StatusCode::OK.into_response(),
            Err(denied) => denied.into_response(),
        }
    }

    struct Fixture {
        router: Router,
        auth: Arc<DashboardAuth>,
        state: AppState,
    }

    /// The sign-in routes, the config route group and an operator route,
    /// under the session layer, wired as `run_gateway` wires them, with a
    /// store of the test's own.
    fn fixture(config: Config) -> Fixture {
        let pairing = Arc::new(PairingGuard::new(
            config.gateway.require_pairing,
            &config.gateway.paired_tokens,
            PairingCodePolicy::default(),
        ));
        let inbound = Arc::new(
            GatewayInboundAuth::from_config(&config, Arc::clone(&pairing))
                .expect("inbound auth builds"),
        );
        fixture_with(
            config,
            pairing,
            inbound,
            Arc::new(DashboardSessions::default()),
        )
    }

    fn fixture_with(
        config: Config,
        pairing: Arc<PairingGuard>,
        inbound: Arc<GatewayInboundAuth>,
        sessions: Arc<DashboardSessions>,
    ) -> Fixture {
        generation(config, pairing, inbound, sessions, None)
    }

    /// One gateway generation. A reload builds the next one over the same
    /// process-shared session store and, as the daemon does, the same canvas
    /// store.
    fn generation(
        config: Config,
        pairing: Arc<PairingGuard>,
        inbound: Arc<GatewayInboundAuth>,
        sessions: Arc<DashboardSessions>,
        canvas_store: Option<zeroclaw_runtime::tools::CanvasStore>,
    ) -> Fixture {
        build_generation(config, pairing, inbound, sessions, canvas_store, None)
    }

    type SharedSopEngine = Arc<std::sync::Mutex<zeroclaw_runtime::sop::SopEngine>>;

    fn build_generation(
        config: Config,
        pairing: Arc<PairingGuard>,
        inbound: Arc<GatewayInboundAuth>,
        sessions: Arc<DashboardSessions>,
        canvas_store: Option<zeroclaw_runtime::tools::CanvasStore>,
        sop_engine: Option<SharedSopEngine>,
    ) -> Fixture {
        let base = crate::api::tests::test_state(config);
        let state = AppState {
            pairing,
            canvas_store: canvas_store.unwrap_or_else(|| base.canvas_store.clone()),
            sop_engine,
            ..base
        };
        let auth =
            Arc::new(DashboardAuth::new(sessions, Arc::clone(&inbound)).for_generation(&state));
        let router = Router::new()
            .merge(crate::config_admin_router(&inbound))
            .merge(crate::api_oidc::routes())
            .route("/api/operator-probe", get(operator_probe))
            .route("/ws/canvas/{id}", get(crate::canvas::handle_ws_canvas))
            .route("/api/events", get(crate::sse::handle_sse_events))
            .route("/ws/sop/runs", get(crate::ws_sop_runs::handle_ws_sop_runs))
            .with_state(state.clone())
            .layer(axum::middleware::from_fn_with_state(
                DashboardLayer {
                    state: state.clone(),
                    auth: Arc::clone(&auth),
                },
                layer,
            ));
        Fixture {
            router,
            auth,
            state,
        }
    }

    fn request(
        method: &str,
        path: &str,
        peer: SocketAddr,
        headers: &[(&str, &str)],
    ) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder()
            .method(method)
            .uri(path)
            .header("host", HOST);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut request = builder.body(Body::empty()).unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    async fn send(router: &Router, request: HttpRequest<Body>) -> Response {
        router.clone().oneshot(request).await.unwrap()
    }

    async fn json(response: Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    fn set_cookies(response: &Response) -> Vec<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
    }

    fn cookie_from(response: &Response, name: &str) -> Option<String> {
        set_cookies(response).into_iter().find_map(|cookie| {
            let value = cookie.strip_prefix(&format!("{name}="))?;
            let value = value.split(';').next().unwrap_or_default();
            (!value.is_empty()).then(|| value.to_string())
        })
    }

    fn query_param(url: &str, name: &str) -> String {
        url.split_once('?')
            .unwrap()
            .1
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
            .unwrap()
            .to_string()
    }

    fn id_token(claims: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{payload}.c2ln")
    }

    /// A started sign-in: the binding cookie and the flow's `state` and
    /// `nonce`, read back from the redirect to the identity provider.
    struct Started {
        binding_name: String,
        binding: String,
        state: String,
        nonce: String,
    }

    async fn start(fixture: &Fixture, path: &str) -> Started {
        let response = send(&fixture.router, request("GET", path, loopback(), &[])).await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT, "{path}");
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let binding_cookie = set_cookies(&response)
            .into_iter()
            .find(|c| c.starts_with(BINDING_COOKIE_STEM))
            .expect("binding cookie set");
        assert!(binding_cookie.contains("HttpOnly"), "{binding_cookie}");
        assert!(binding_cookie.contains("SameSite=Lax"), "{binding_cookie}");
        assert!(
            binding_cookie.contains("Path=/oidc/callback"),
            "{binding_cookie}"
        );
        let (binding_name, rest) = binding_cookie.split_once('=').unwrap();
        let binding = rest.split(';').next().unwrap().to_string();
        let state = query_param(&location, "state");
        let scope = CookieScope {
            https: false,
            path_prefix: "",
        };
        assert_eq!(
            binding_name,
            scope.binding_name_and_path(&state).0,
            "the binding cookie is named after its flow"
        );
        Started {
            binding_name: binding_name.to_string(),
            binding,
            state,
            nonce: query_param(&location, "nonce"),
        }
    }

    async fn mount_token(server: &MockServer, nonce: &str, with_id_token: bool) {
        let issuer = server.uri();
        let mut body = serde_json::json!({
            "access_token": "at-alice",
            "token_type": "Bearer",
            "expires_in": 600,
        });
        if with_id_token {
            body["id_token"] = serde_json::json!(id_token(serde_json::json!({
                "iss": issuer,
                "aud": "gw",
                "sub": "alice",
                "exp": unix_now() + 600,
                "nonce": nonce,
            })));
        }
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    fn callback(started: &Started, binding: Option<&str>) -> HttpRequest<Body> {
        callback_with(started, binding, "c1", &[])
    }

    fn callback_with(
        started: &Started,
        binding: Option<&str>,
        code: &str,
        other_cookies: &[String],
    ) -> HttpRequest<Body> {
        let mut cookies: Vec<String> = other_cookies.to_vec();
        if let Some(binding) = binding {
            cookies.push(format!("{}={binding}", started.binding_name));
        }
        let cookie = cookies.join("; ");
        let mut headers = vec![];
        if !cookie.is_empty() {
            headers.push(("cookie", cookie.as_str()));
        }
        request(
            "GET",
            &format!("/oidc/callback?state={}&code={code}", started.state),
            loopback(),
            &headers,
        )
    }

    struct SignedIn {
        cookie: String,
        csrf: String,
        session: serde_json::Value,
    }

    impl SignedIn {
        fn cookie_header(&self) -> String {
            format!("{SESSION_COOKIE}={}", self.cookie)
        }
    }

    async fn sign_in(fixture: &Fixture, server: &MockServer) -> SignedIn {
        let started = start(fixture, "/auth/sign-in/corp?return=/chat").await;
        mount_token(server, &started.nonce, true).await;
        let response = send(&fixture.router, callback(&started, Some(&started.binding))).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/chat");
        let session_cookie = set_cookies(&response)
            .into_iter()
            .find(|c| c.starts_with(SESSION_COOKIE))
            .expect("session cookie set");
        assert!(session_cookie.contains("HttpOnly"), "{session_cookie}");
        assert!(
            session_cookie.contains("SameSite=Strict"),
            "{session_cookie}"
        );
        assert!(session_cookie.contains("Path=/;"), "{session_cookie}");
        assert!(!session_cookie.contains("Secure"), "plain HTTP on loopback");
        assert!(
            set_cookies(&response)
                .iter()
                .any(|c| c.starts_with(&started.binding_name) && c.contains("Max-Age=0")),
            "the binding cookie is cleared"
        );
        assert!(
            !response
                .headers()
                .keys()
                .any(|name| name.as_str().starts_with("access-control-")),
            "no CORS grant ever rides on a cookie response"
        );
        let cookie = cookie_from(&response, SESSION_COOKIE).unwrap();
        let cookie_header = format!("{SESSION_COOKIE}={cookie}");
        let response = send(
            &fixture.router,
            request(
                "GET",
                "/api/auth/session",
                loopback(),
                &[
                    ("cookie", &cookie_header),
                    ("sec-fetch-site", "same-origin"),
                ],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session = json(response).await;
        SignedIn {
            cookie,
            csrf: session["csrf_token"].as_str().unwrap().to_string(),
            session,
        }
    }

    async fn probe(fixture: &Fixture, headers: &[(&str, &str)]) -> (StatusCode, serde_json::Value) {
        let response = send(
            &fixture.router,
            request("GET", "/api/operator-probe", loopback(), headers),
        )
        .await;
        let status = response.status();
        (status, json(response).await)
    }

    // ── Off by default ───────────────────────────────────────────────

    #[tokio::test]
    async fn with_sign_in_off_a_session_cookie_is_no_credential_at_all() {
        let server = idp(&["admins"], "gw").await;
        let mut config = sign_in_config(&server.uri());
        config.gateway.dashboard_sign_in.oidc.clear();
        let fixture = fixture(config);

        let (status, body) = probe(&fixture, &[("cookie", "zc_session=forged")]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            body.get("reason").is_none(),
            "the pairing denial, unchanged: {body}"
        );

        let response = send(
            &fixture.router,
            request("GET", "/api/auth/methods", loopback(), &[]),
        )
        .await;
        assert_eq!(json(response).await["oidc"], serde_json::json!([]));

        let response = send(
            &fixture.router,
            request("GET", "/auth/sign-in/corp", loopback(), &[]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(set_cookies(&response).is_empty());
    }

    // ── The flow ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn an_administrator_signs_in_reaches_operator_routes_and_signs_out() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));

        let methods = json(
            send(
                &fixture.router,
                request("GET", "/api/auth/methods", loopback(), &[]),
            )
            .await,
        )
        .await;
        assert_eq!(
            methods["oidc"],
            serde_json::json!([{"alias": "corp", "provider": "oidc.corp"}])
        );

        let signed_in = sign_in(&fixture, &server).await;
        assert_eq!(signed_in.session["kind"], "oidc");
        assert_eq!(signed_in.session["provider"], "oidc.corp");
        assert_eq!(signed_in.session["admin"], true);
        assert_eq!(signed_in.session["display_name"], "alice@example.com");
        assert!(signed_in.session["expires_at"].as_u64().unwrap() > unix_now());

        let cookie = signed_in.cookie_header();
        let (status, _) = probe(&fixture, &[("cookie", &cookie)]).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an administrator is operator-equivalent"
        );

        // Sign-out changes state: the CSRF token and the own origin are required.
        let response = send(
            &fixture.router,
            request(
                "POST",
                "/auth/sign-out",
                loopback(),
                &[("cookie", &cookie), ("origin", OWN_ORIGIN)],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(json(response).await["reason"], "csrf");

        let response = send(
            &fixture.router,
            request(
                "POST",
                "/auth/sign-out",
                loopback(),
                &[
                    ("cookie", &cookie),
                    ("origin", OWN_ORIGIN),
                    (CSRF_HEADER, &signed_in.csrf),
                ],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            set_cookies(&response)
                .iter()
                .any(|c| c.starts_with(SESSION_COOKIE) && c.contains("Max-Age=0"))
        );

        let (status, body) = probe(&fixture, &[("cookie", &cookie)]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["reason"], "session_invalid");
    }

    #[tokio::test]
    async fn a_scoped_account_gets_config_but_not_operator_routes() {
        let server = idp(&["ops"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        assert_eq!(signed_in.session["admin"], false);
        assert_eq!(signed_in.session["features"]["chat"], false);

        let cookie = signed_in.cookie_header();
        let (status, body) = probe(&fixture, &[("cookie", &cookie)]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["reason"], "operator_required");

        // The config route group takes the session as the principal.
        let response = send(
            &fixture.router,
            request(
                "GET",
                "/api/quickstart/state",
                loopback(),
                &[("cookie", &cookie), ("sec-fetch-site", "same-origin")],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = send(
            &fixture.router,
            request(
                "POST",
                "/api/quickstart/fields",
                loopback(),
                &[
                    ("cookie", &cookie),
                    ("origin", OWN_ORIGIN),
                    (CSRF_HEADER, &signed_in.csrf),
                ],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "Config:Read only");
    }

    #[tokio::test]
    async fn a_sign_in_without_an_id_token_is_refused() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let started = start(&fixture, "/auth/sign-in/corp").await;
        mount_token(&server, &started.nonce, false).await;
        let response = send(&fixture.router, callback(&started, Some(&started.binding))).await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(cookie_from(&response, SESSION_COOKIE).is_none());
        assert_eq!(fixture.auth.sessions.len(), 0);
    }

    #[tokio::test]
    async fn a_callback_from_another_browser_cannot_finish_the_sign_in() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let started = start(&fixture, "/auth/sign-in/corp").await;
        mount_token(&server, &started.nonce, true).await;

        for binding in [None, Some("someone-elses-binding")] {
            let response = send(&fixture.router, callback(&started, binding)).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{binding:?}");
            assert!(cookie_from(&response, SESSION_COOKIE).is_none());
        }
        // The first refusal consumed the flow: the right cookie is too late.
        let response = send(&fixture.router, callback(&started, Some(&started.binding))).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(fixture.auth.sessions.len(), 0);
        // No code ever reached the identity provider.
        let exchanges = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/token")
            .count();
        assert_eq!(exchanges, 0);
    }

    /// A service client is refused a dashboard session even when the
    /// policy maps it to a profile, so the refusal is the actor check's,
    /// not the resolver's.
    #[tokio::test]
    async fn a_service_principal_gets_no_session_even_with_a_mapped_profile() {
        let server = idp(&["admins"], "svc").await;
        let mut config = sign_in_config(&server.uri());
        config
            .oidc
            .get_mut("corp")
            .unwrap()
            .service_profile_map
            .insert("svc".into(), "admin".into());
        let fixture = fixture(config);
        let service = fixture
            .auth
            .inbound
            .authenticate_scoped("at-svc", "oidc.corp")
            .await
            .expect("the policy maps the service client");
        assert_eq!(service.principal.actor, ActorKind::Service);
        let started = start(&fixture, "/auth/sign-in/corp").await;
        mount_token(&server, &started.nonce, true).await;
        let response = send(&fixture.router, callback(&started, Some(&started.binding))).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(cookie_from(&response, SESSION_COOKIE).is_none());
        assert_eq!(fixture.auth.sessions.len(), 0);
    }

    #[tokio::test]
    async fn an_unmapped_identity_gets_no_session() {
        let server = idp(&["nobody"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let started = start(&fixture, "/auth/sign-in/corp").await;
        mount_token(&server, &started.nonce, true).await;
        let response = send(&fixture.router, callback(&started, Some(&started.binding))).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(cookie_from(&response, SESSION_COOKIE).is_none());
        assert_eq!(fixture.auth.sessions.len(), 0);
    }

    #[tokio::test]
    async fn sign_in_needs_https_off_the_loopback_host() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let response = send(
            &fixture.router,
            request("GET", "/auth/sign-in/corp", remote(), &[]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(set_cookies(&response).is_empty());
    }

    #[tokio::test]
    async fn an_off_site_return_path_lands_on_the_dashboard_root() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        for target in ["//evil.example", "https://evil.example", "/\\evil.example"] {
            let started = start(
                &fixture,
                &format!("/auth/sign-in/corp?return={}", target.replace('\\', "%5C")),
            )
            .await;
            // A fresh token answer for this flow's nonce.
            server.reset().await;
            mount_discovery_and_introspection(&server, &["admins"], "gw").await;
            mount_token(&server, &started.nonce, true).await;
            let response = send(&fixture.router, callback(&started, Some(&started.binding))).await;
            assert_eq!(response.status(), StatusCode::SEE_OTHER, "{target}");
            assert_eq!(response.headers()[header::LOCATION], "/", "{target}");
        }
    }

    async fn mount_discovery_and_introspection(server: &MockServer, groups: &[&str], client: &str) {
        let issuer = server.uri();
        Mock::given(http_method("GET"))
            .and(http_path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "introspection_endpoint": format!("{issuer}/introspect"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "token_type": "Bearer",
                "client_id": client,
                "iss": issuer,
                "sub": "alice",
                "aud": "zeroclaw",
                "exp": unix_now() + 600,
                "groups": groups,
            })))
            .mount(server)
            .await;
    }

    // ── Where a cookie-carrying request came from ────────────────────

    #[tokio::test]
    async fn cross_origin_and_cross_site_use_of_the_cookie_is_refused() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        let cookie = signed_in.cookie_header();

        // A page on another port of the same host is same-site, so the
        // browser sends the cookie; its Origin gives it away.
        let other = "http://localhost:3000";
        let (status, body) = probe(&fixture, &[("cookie", &cookie), ("origin", other)]).await;
        assert_eq!(
            (status, body["reason"].clone()),
            (StatusCode::FORBIDDEN, "origin".into())
        );

        let (status, body) = probe(
            &fixture,
            &[("cookie", &cookie), ("sec-fetch-site", "same-site")],
        )
        .await;
        assert_eq!(
            (status, body["reason"].clone()),
            (StatusCode::FORBIDDEN, "origin".into())
        );

        // WebSocket upgrades must name the dashboard's origin.
        let (status, body) =
            probe(&fixture, &[("cookie", &cookie), ("upgrade", "websocket")]).await;
        assert_eq!(
            (status, body["reason"].clone()),
            (StatusCode::FORBIDDEN, "origin".into())
        );
        let (status, _) = probe(
            &fixture,
            &[
                ("cookie", &cookie),
                ("upgrade", "websocket"),
                ("origin", OWN_ORIGIN),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // A state-changing request with a foreign origin fails even with the token.
        let response = send(
            &fixture.router,
            request(
                "POST",
                "/auth/sign-out",
                loopback(),
                &[
                    ("cookie", &cookie),
                    ("origin", other),
                    (CSRF_HEADER, &signed_in.csrf),
                ],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(json(response).await["reason"], "origin");
        assert_eq!(
            fixture.auth.sessions.len(),
            1,
            "a refused request ends nothing"
        );
    }

    #[tokio::test]
    async fn an_explicit_bearer_beats_the_cookie() {
        let server = idp(&["ops"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        let cookie = signed_in.cookie_header();
        let (status, _) = probe(
            &fixture,
            &[("cookie", &cookie), ("authorization", "Bearer zc_paired")],
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the paired bearer decides, not the scoped cookie"
        );
        let (status, _) = probe(
            &fixture,
            &[("cookie", &cookie), ("authorization", "Bearer wrong")],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ── The store ────────────────────────────────────────────────────

    async fn verified_auth(fixture: &Fixture) -> ConnectionAuth {
        fixture
            .auth
            .inbound
            .authenticate_scoped("at-alice", "oidc.corp")
            .await
            .expect("the mock provider vouches for the token")
    }

    fn new_session(
        auth: ConnectionAuth,
        inbound: &GatewayInboundAuth,
        lifetime: u64,
    ) -> NewSession {
        NewSession {
            alias: "corp".into(),
            provider: ProviderPin {
                issuer: "unused-by-store-tests".into(),
                client_id: "gw".into(),
            },
            bearer: "at-alice".to_string().into(),
            auth,
            authority: inbound.authority_ref(),
            display_name: "alice".into(),
            lifetime_secs: lifetime,
        }
    }

    #[tokio::test]
    async fn a_full_store_refuses_new_sign_ins_instead_of_evicting_others() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let auth = verified_auth(&fixture).await;
        let mut cookies = Vec::new();
        for i in 0..MAX_SESSIONS {
            let mut other = auth.clone();
            other.principal.id = zeroclaw_api::principal::PrincipalId(format!("principal-{i}"));
            let (cookie, _) = fixture
                .auth
                .sessions
                .insert(new_session(other, &fixture.auth.inbound, 3600), unix_now())
                .expect("room for every distinct principal up to the cap");
            cookies.push(cookie);
        }
        assert!(!fixture.auth.sessions.has_room());
        let refused = fixture.auth.sessions.insert(
            new_session(auth.clone(), &fixture.auth.inbound, 3600),
            unix_now(),
        );
        assert_eq!(refused.err(), Some(StoreFull));

        // The starter refuses before any round trip to the provider.
        let response = send(
            &fixture.router,
            request("GET", "/auth/sign-in/corp", loopback(), &[]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(response.headers().contains_key(header::RETRY_AFTER));
        assert_eq!(fixture.auth.sessions.len(), MAX_SESSIONS);
        assert!(
            cookies.iter().all(|cookie| {
                fixture
                    .auth
                    .sessions
                    .sessions
                    .lock()
                    .contains_key(&session_key(cookie))
            }),
            "every existing session survives"
        );
    }

    #[tokio::test]
    async fn one_principal_past_its_cap_gives_up_only_its_own_oldest_session() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let auth = verified_auth(&fixture).await;
        let mut bystander = auth.clone();
        bystander.principal.id = zeroclaw_api::principal::PrincipalId("bystander".into());
        let (bystander_cookie, _) = fixture
            .auth
            .sessions
            .insert(
                new_session(bystander, &fixture.auth.inbound, 3600),
                unix_now(),
            )
            .unwrap();
        let mut own = Vec::new();
        for i in 0..=MAX_SESSIONS_PER_PRINCIPAL {
            let (cookie, view) = fixture
                .auth
                .sessions
                .insert(
                    new_session(auth.clone(), &fixture.auth.inbound, 3600),
                    unix_now() + i as u64,
                )
                .unwrap();
            own.push((cookie, view));
        }
        let held = |cookie: &str| {
            fixture
                .auth
                .sessions
                .sessions
                .lock()
                .contains_key(&session_key(cookie))
        };
        assert!(
            !held(&own[0].0),
            "the oldest of this principal's sessions went"
        );
        assert!(own[0].1.ended.is_cancelled(), "and its sockets were told");
        assert!(own[1..].iter().all(|(cookie, _)| held(cookie)));
        assert!(held(&bystander_cookie), "nobody else's session is touched");
    }

    #[tokio::test]
    async fn an_expired_session_ends_and_says_so() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let auth = verified_auth(&fixture).await;
        let (cookie, view) = fixture
            .auth
            .sessions
            .insert(new_session(auth, &fixture.auth.inbound, 0), unix_now())
            .unwrap();
        let cookie_header = format!("{SESSION_COOKIE}={cookie}");
        let response = send(
            &fixture.router,
            request(
                "GET",
                "/api/operator-probe",
                loopback(),
                &[("cookie", &cookie_header)],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // The cookie stays: the browser's other tabs, and its next request,
        // must hear "expired" too, not the answer for no credential at all.
        assert!(
            !set_cookies(&response)
                .iter()
                .any(|c| c.starts_with(SESSION_COOKIE)),
            "an expired session's answer does not drop the cookie"
        );
        assert_eq!(json(response).await["reason"], "session_expired");
        assert!(view.ended.is_cancelled());
        assert_eq!(fixture.auth.sessions.len(), 0);
        let (status, body) = probe(&fixture, &[("cookie", &cookie_header)]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body["reason"], "session_expired",
            "the next request hears it too"
        );
    }

    /// A session ended by a policy change answers "expired" in every tab of
    /// the browser, not only to whichever request saw the change first.
    #[tokio::test]
    async fn a_revoked_session_answers_expired_to_every_later_request() {
        let server = idp(&["admins"], "gw").await;
        let config = sign_in_config(&server.uri());
        let pairing = Arc::new(PairingGuard::new(
            true,
            &config.gateway.paired_tokens,
            PairingCodePolicy::default(),
        ));
        let rpc = Arc::new(RpcInboundAuth::from_config(&config, Arc::clone(&pairing)).unwrap());
        let inbound = Arc::new(GatewayInboundAuth::from_shared(Arc::clone(&rpc)));
        let fixture = fixture_with(
            config.clone(),
            pairing,
            inbound,
            Arc::new(DashboardSessions::default()),
        );
        let signed_in = sign_in(&fixture, &server).await;
        let mut revoked = config;
        revoked
            .oidc
            .get_mut("corp")
            .unwrap()
            .profile_map
            .remove("admins");
        rpc.publish_accepted(&revoked, rpc.accepted_revision() + 1)
            .unwrap();

        let first = send(
            &fixture.router,
            request(
                "GET",
                "/api/operator-probe",
                loopback(),
                &[("cookie", &signed_in.cookie_header())],
            ),
        )
        .await;
        assert_eq!(first.status(), StatusCode::UNAUTHORIZED);
        assert!(
            !set_cookies(&first)
                .iter()
                .any(|c| c.starts_with(SESSION_COOKIE)),
            "a revoked session's answer does not drop the cookie"
        );
        assert_eq!(json(first).await["reason"], "session_expired");
        let (status, body) = probe(&fixture, &[("cookie", &signed_in.cookie_header())]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body["reason"], "session_expired",
            "another tab hears it too"
        );
    }

    /// Another browser's sign-in sweeps expired sessions out of the store.
    /// A browser that comes back afterwards with its cookie must still be
    /// told its sign-in expired (so it offers to sign in again), not that
    /// the session never existed.
    #[tokio::test]
    async fn a_swept_session_still_answers_expired() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let auth = verified_auth(&fixture).await;
        let (cookie, _) = fixture
            .auth
            .sessions
            .insert(new_session(auth, &fixture.auth.inbound, 0), unix_now())
            .unwrap();
        assert!(fixture.auth.sessions.has_room(), "the sweep runs here");
        assert_eq!(fixture.auth.sessions.len(), 0);
        let caller = fixture.auth.admit(|_| None, &cookie).await;
        assert!(matches!(
            caller,
            DashboardCaller::Rejected(SessionRejection::Expired)
        ));
        // A cookie that never named a session is still invalid.
        let caller = fixture.auth.admit(|_| None, "never-issued").await;
        assert!(matches!(
            caller,
            DashboardCaller::Rejected(SessionRejection::Invalid)
        ));
    }

    #[tokio::test]
    async fn a_provider_no_longer_offered_ends_its_sessions() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        let caller = fixture.auth.admit(|_| None, &signed_in.cookie).await;
        assert!(matches!(
            caller,
            DashboardCaller::Rejected(SessionRejection::Invalid)
        ));
        assert_eq!(fixture.auth.sessions.len(), 0);
    }

    #[tokio::test]
    async fn a_policy_that_no_longer_maps_the_account_ends_the_session() {
        let server = idp(&["admins"], "gw").await;
        let config = sign_in_config(&server.uri());
        let pairing = Arc::new(PairingGuard::new(
            true,
            &config.gateway.paired_tokens,
            PairingCodePolicy::default(),
        ));
        let rpc = Arc::new(RpcInboundAuth::from_config(&config, Arc::clone(&pairing)).unwrap());
        let inbound = Arc::new(GatewayInboundAuth::from_shared(Arc::clone(&rpc)));
        let fixture = fixture_with(
            config.clone(),
            pairing,
            inbound,
            Arc::new(DashboardSessions::default()),
        );
        let signed_in = sign_in(&fixture, &server).await;
        let (status, _) = probe(&fixture, &[("cookie", &signed_in.cookie_header())]).await;
        assert_eq!(status, StatusCode::OK);

        // The operator drops the group mapping; the next request sees it.
        let mut revoked = config;
        revoked
            .oidc
            .get_mut("corp")
            .unwrap()
            .profile_map
            .remove("admins");
        rpc.publish_accepted(&revoked, rpc.accepted_revision() + 1)
            .unwrap();
        let (status, body) = probe(&fixture, &[("cookie", &signed_in.cookie_header())]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["reason"], "session_expired");
    }

    #[tokio::test]
    async fn a_session_from_an_earlier_generation_is_verified_again_before_use() {
        let server = idp(&["admins"], "gw").await;
        let config = sign_in_config(&server.uri());
        let sessions = Arc::new(DashboardSessions::default());
        let generation = |config: &Config| {
            let pairing = Arc::new(PairingGuard::new(
                true,
                &config.gateway.paired_tokens,
                PairingCodePolicy::default(),
            ));
            let inbound =
                Arc::new(GatewayInboundAuth::from_config(config, Arc::clone(&pairing)).unwrap());
            fixture_with(config.clone(), pairing, inbound, Arc::clone(&sessions))
        };
        let first = generation(&config);
        let signed_in = sign_in(&first, &server).await;

        // A reload: a new gateway generation with a new authority, the same store.
        let second = generation(&config);
        let introspections = |requests: &[wiremock::Request]| {
            requests
                .iter()
                .filter(|r| r.url.path() == "/introspect")
                .count()
        };
        let before = introspections(&server.received_requests().await.unwrap());
        let (status, _) = probe(&second, &[("cookie", &signed_in.cookie_header())]).await;
        assert_eq!(status, StatusCode::OK, "the sign-in survives the reload");
        let after = introspections(&server.received_requests().await.unwrap());
        assert_eq!(after, before + 1, "verified once against the new authority");
        let (status, _) = probe(&second, &[("cookie", &signed_in.cookie_header())]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            introspections(&server.received_requests().await.unwrap()),
            after,
            "and not again while the new authority stands"
        );
        drop(first);
    }

    // ── Long-lived sockets ───────────────────────────────────────────

    #[tokio::test]
    async fn a_socket_watch_fires_when_its_session_is_signed_out() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        let DashboardCaller::Session(session) = fixture
            .auth
            .admit(|alias| Some(pin_for(&fixture, alias)), &signed_in.cookie)
            .await
        else {
            panic!("signed in");
        };
        let mut watch = Some(SessionWatch::new(
            Arc::clone(&fixture.auth),
            Arc::clone(&session),
            true,
            &fixture.state,
        ));
        assert!(fixture.auth.end(&session));
        let code = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session_lost(&mut watch, &fixture.state),
        )
        .await
        .expect("the watch fires at once");
        assert_eq!(code, WS_CLOSE_SESSION_ENDED);

        let mut none: Option<SessionWatch> = None;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                session_lost(&mut none, &fixture.state)
            )
            .await
            .is_err(),
            "a socket without a session is never closed by it"
        );
    }

    #[test]
    fn origins_compare_as_browsers_send_them() {
        assert_eq!(
            origin_of(false, "LocalHost:42617"),
            "http://localhost:42617"
        );
        assert_eq!(
            origin_of(true, "zc.example.com:443"),
            "https://zc.example.com"
        );
        assert_eq!(
            origin_of(false, "zc.example.com:80"),
            "http://zc.example.com"
        );
        assert_eq!(
            origin_of(true, "zc.example.com:8443"),
            "https://zc.example.com:8443"
        );
    }

    #[test]
    fn cookies_carry_the_attributes_their_scope_needs() {
        let https_root = CookieScope {
            https: true,
            path_prefix: "",
        };
        let value = https_root.session_cookie("v").to_str().unwrap().to_string();
        assert_eq!(
            value,
            "__Host-zc_session=v; Path=/; HttpOnly; SameSite=Strict; Secure"
        );
        let prefixed = CookieScope {
            https: true,
            path_prefix: "/zc",
        };
        let value = prefixed.session_cookie("v").to_str().unwrap().to_string();
        assert_eq!(
            value,
            "zc_session=v; Path=/zc/; HttpOnly; SameSite=Strict; Secure"
        );
        assert_eq!(https_root.clear_session_cookies().len(), 2);

        // The binding cookie: `__Host-` at an HTTPS root (so `Path=/`),
        // scoped to the callback otherwise; named after its flow either way.
        let (name, _) = prefixed.binding_name_and_path("state-a");
        let binding = prefixed.binding_cookie("state-a", "b", 600);
        assert_eq!(
            binding.to_str().unwrap(),
            format!(
                "{name}=b; Path=/zc/oidc/callback; HttpOnly; SameSite=Lax; Max-Age=600; Secure"
            )
        );
        assert!(name.starts_with("zc_signin_"), "{name}");
        let (root_name, root_path) = https_root.binding_name_and_path("state-a");
        assert!(root_name.starts_with("__Host-zc_signin_"), "{root_name}");
        assert_eq!(root_path, "/");
        assert_ne!(
            prefixed.binding_name_and_path("state-b").0,
            name,
            "two flows never share a binding cookie"
        );
    }

    /// At an HTTPS root only the `__Host-` name is read, so a plain-named
    /// cookie planted from a sibling host is never consulted.
    #[test]
    fn a_plain_named_cookie_is_ignored_where_host_prefixed_applies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("zc_session=planted"),
        );
        let https_root = CookieScope {
            https: true,
            path_prefix: "",
        };
        assert_eq!(https_root.session_value(&headers), None);
        let plain_http = CookieScope {
            https: false,
            path_prefix: "",
        };
        assert_eq!(
            plain_http.session_value(&headers).as_deref(),
            Some("planted")
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("zc_session=planted; __Host-zc_session=real"),
        );
        assert_eq!(https_root.session_value(&headers).as_deref(), Some("real"));
    }

    #[tokio::test]
    async fn two_sign_ins_started_in_one_browser_both_finish() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let first = start(&fixture, "/auth/sign-in/corp?return=/a").await;
        let second = start(&fixture, "/auth/sign-in/corp?return=/b").await;
        assert_ne!(first.binding_name, second.binding_name);
        for (started, code) in [(&first, "code-a"), (&second, "code-b")] {
            Mock::given(http_method("POST"))
                .and(http_path("/token"))
                .and(wiremock::matchers::body_string_contains(format!(
                    "code={code}"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "at-alice",
                    "token_type": "Bearer",
                    "expires_in": 600,
                    "id_token": id_token(serde_json::json!({
                        "iss": server.uri(),
                        "aud": "gw",
                        "sub": "alice",
                        "exp": unix_now() + 600,
                        "nonce": started.nonce,
                    })),
                })))
                .mount(&server)
                .await;
        }
        // The browser holds both binding cookies and sends both.
        let both = [
            format!("{}={}", first.binding_name, first.binding),
            format!("{}={}", second.binding_name, second.binding),
        ];
        let response = send(
            &fixture.router,
            callback_with(&second, None, "code-b", &both),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/b");
        let response = send(
            &fixture.router,
            callback_with(&first, None, "code-a", &both),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/a");
    }

    #[tokio::test]
    async fn a_session_whose_alias_now_names_another_provider_ends_unverified() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        let introspections = || async {
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|r| r.url.path() == "/introspect")
                .count()
        };
        let before = introspections().await;
        let repointed = ProviderPin {
            issuer: "https://other-idp.example.com".into(),
            client_id: "gw".into(),
        };
        let caller = fixture
            .auth
            .admit(|_| Some(repointed.clone()), &signed_in.cookie)
            .await;
        assert!(matches!(
            caller,
            DashboardCaller::Rejected(SessionRejection::Invalid)
        ));
        assert_eq!(fixture.auth.sessions.len(), 0);
        assert_eq!(
            introspections().await,
            before,
            "the held token was never sent anywhere"
        );
    }

    #[tokio::test]
    async fn turning_sign_in_off_ends_every_session() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        assert_eq!(fixture.auth.sessions.len(), 1);

        // Turned off live, in the same generation: the next request ends
        // every session, without waiting for a sweep or a reload.
        set_sign_in(&fixture.state, false).await;
        let (status, body) = probe(&fixture, &[("cookie", &signed_in.cookie_header())]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            body.get("reason").is_none(),
            "with sign-in off the cookie is no credential"
        );
        assert_eq!(fixture.auth.sessions.len(), 0, "and the sessions are gone");

        // Offering the provider again does not bring the old session back.
        set_sign_in(&fixture.state, true).await;
        let (status, body) = probe(&fixture, &[("cookie", &signed_in.cookie_header())]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["reason"], "session_invalid");
    }

    /// A busy socket keeps cancelling the future `session_lost` returns; a
    /// recheck that needs a round trip must still complete across those
    /// cancellations instead of starting over every time.
    #[tokio::test]
    async fn a_recheck_survives_the_socket_loop_dropping_its_future() {
        let server = idp(&["admins"], "gw").await;
        let config = sign_in_config(&server.uri());
        let fixture = fixture(config.clone());
        let signed_in = sign_in(&fixture, &server).await;
        let DashboardCaller::Session(session) = fixture
            .auth
            .admit(|alias| Some(pin_for(&fixture, alias)), &signed_in.cookie)
            .await
        else {
            panic!("signed in");
        };
        // A reload: the next recheck must verify the token again against a
        // new authority, and the provider now answers slowly, refusing it.
        server.reset().await;
        let issuer = server.uri();
        Mock::given(http_method("GET"))
            .and(http_path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "introspection_endpoint": format!("{issuer}/introspect"),
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(300))
                    .set_body_json(serde_json::json!({"active": false})),
            )
            .mount(&server)
            .await;
        let reloaded = Arc::new(DashboardAuth::new(
            Arc::clone(&fixture.auth.sessions),
            Arc::new(
                GatewayInboundAuth::from_config(&config, Arc::clone(&fixture.state.pairing))
                    .unwrap(),
            ),
        ));
        let mut watch = SessionWatch::new(reloaded, session, true, &fixture.state);
        watch.recheck = tokio::time::interval(std::time::Duration::from_millis(1));
        let mut watch = Some(watch);
        // Each 20 ms slice is one turn of a busy socket's loop.
        let mut closed = None;
        for _ in 0..100 {
            if let Ok(code) = tokio::time::timeout(
                std::time::Duration::from_millis(20),
                session_lost(&mut watch, &fixture.state),
            )
            .await
            {
                closed = Some(code);
                break;
            }
        }
        assert_eq!(closed, Some(WS_CLOSE_SESSION_ENDED));
    }

    fn pin_for(fixture: &Fixture, alias: &str) -> ProviderPin {
        ProviderPin::of(&fixture.state.config.read().oidc[alias])
    }

    // ── Generations, token custody and the idle sweep ─────────────────

    fn pairing_for(config: &Config) -> Arc<PairingGuard> {
        Arc::new(PairingGuard::new(
            config.gateway.require_pairing,
            &config.gateway.paired_tokens,
            PairingCodePolicy::default(),
        ))
    }

    /// A socket opened in one gateway generation must not keep that
    /// generation's grants after a reload: it closes when its generation
    /// stops, a frame the next generation publishes never reaches it, and
    /// reconnecting is judged by the new policy.
    #[tokio::test]
    async fn a_canvas_socket_does_not_outlive_its_gateway_generation() {
        let server = idp(&["admins", "ops"], "gw").await;
        let config = sign_in_config(&server.uri());
        let sessions = Arc::new(DashboardSessions::default());
        let pairing = pairing_for(&config);
        let old = generation(
            config.clone(),
            Arc::clone(&pairing),
            Arc::new(GatewayInboundAuth::from_config(&config, Arc::clone(&pairing)).unwrap()),
            Arc::clone(&sessions),
            None,
        );
        let (cookie, _) = old
            .auth
            .sign_in("corp", pin_for(&old, "corp"), "at-alice".to_string(), 3600)
            .await
            .expect("an administrator signs in");
        let cookie = format!("{SESSION_COOKIE}={cookie}");
        let (old_addr, old_server) = test_support::serve(old.router.clone()).await;
        let mut socket = test_support::connect(old_addr, "/ws/canvas/c1", &cookie)
            .await
            .expect("an administrator opens the canvas feed");

        // The reload: the old generation is told to stop, and the next one
        // no longer maps the admin group. Both share the canvas store.
        old.state.shutdown_tx.send_replace(true);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let revoked = test_support::without_admin_mapping(&config);
        let pairing = pairing_for(&revoked);
        let new = generation(
            revoked.clone(),
            Arc::clone(&pairing),
            Arc::new(GatewayInboundAuth::from_config(&revoked, pairing).unwrap()),
            Arc::clone(&sessions),
            Some(old.state.canvas_store.clone()),
        );
        new.state
            .canvas_store
            .render("c1", "text/plain", "published after the reload");

        let (code, texts) = test_support::until_closed(&mut socket, Duration::from_secs(3)).await;
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("published after the reload")),
            "a frame published after the reload reached the old socket: {texts:?}"
        );
        assert_eq!(
            code,
            Some(WS_CLOSE_GOING_AWAY),
            "the old generation's socket closes for its generation's stop"
        );

        let (new_addr, new_server) = test_support::serve(new.router.clone()).await;
        let reconnect = test_support::connect(new_addr, "/ws/canvas/c1", &cookie).await;
        assert_eq!(
            reconnect.err(),
            Some(403),
            "the new policy no longer grants the canvas feed"
        );
        old_server.abort();
        new_server.abort();
    }

    #[tokio::test]
    async fn the_session_store_holds_the_token_in_a_zeroizing_buffer() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let new = new_session(verified_auth(&fixture).await, &fixture.auth.inbound, 3600);
        let staged = std::any::type_name_of_val(&new.bearer);
        assert!(
            staged.contains("Zeroizing"),
            "a staged sign-in holds the token as {staged}"
        );
        fixture.auth.sessions.insert(new, unix_now()).unwrap();
        let sessions = fixture.auth.sessions.sessions.lock();
        let held = sessions.values().next().expect("the session is stored");
        let stored = std::any::type_name_of_val(&held.bearer);
        assert!(
            stored.contains("Zeroizing"),
            "a stored session holds the token as {stored}"
        );
    }

    /// Nothing but time passes: no request, no sign-in. An expired session
    /// and its token must still leave the store within the sweep interval.
    #[tokio::test]
    async fn an_idle_expired_session_is_swept_without_any_request() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let auth = verified_auth(&fixture).await;
        // Its hard expiry (lifetime 0) comes before the token's own expiry.
        let (_cookie, view) = fixture
            .auth
            .sessions
            .insert(new_session(auth, &fixture.auth.inbound, 0), unix_now())
            .unwrap();
        assert_eq!(
            fixture.auth.sessions.len(),
            1,
            "nothing has checked the store yet"
        );
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.auth.sessions.len(), 0, "the idle sweep removed it");
        assert!(view.ended.is_cancelled(), "and told its sockets");
    }

    // ── Checks that outlast their session, late watches, sign-out ─────

    fn inbound_for(config: &Config) -> Arc<GatewayInboundAuth> {
        Arc::new(
            GatewayInboundAuth::from_config(config, pairing_for(config))
                .expect("inbound auth builds"),
        )
    }

    fn cookie_for(cookie: &str) -> String {
        format!("{SESSION_COOKIE}={cookie}")
    }

    /// A request whose session has to be verified again, slowly, is refused
    /// when the session's hard expiry passes while it waits, although the
    /// token itself is still valid.
    #[tokio::test]
    async fn an_http_admission_verified_past_hard_expiry_is_refused() {
        let server = idp(&["admins"], "gw").await;
        let mut config = sign_in_config(&server.uri());
        test_support::revalidate_after(&mut config, 2);
        let fixture = fixture(config);
        let (cookie, _) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 5)
            .await
            .expect("alice signs in");
        // Due for verification, which now takes past the hard expiry.
        tokio::time::sleep(test_support::UNTIL_DUE).await;
        test_support::slow_introspection(&server, &["admins"], "gw", Duration::from_secs(4)).await;
        let (status, body) = probe(&fixture, &[("cookie", &cookie_for(&cookie))]).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a request was admitted past the session's hard expiry"
        );
        assert_eq!(body["reason"], "session_expired");
    }

    /// A frame's check that has to verify the session again, slowly, ends
    /// at the session's hard expiry instead of admitting the frame after it.
    #[tokio::test]
    async fn a_frame_check_cut_short_by_expiry_refuses_the_frame() {
        let server = idp(&["admins"], "gw").await;
        let mut config = sign_in_config(&server.uri());
        test_support::revalidate_after(&mut config, 2);
        let fixture = fixture(config);
        let (_, session) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 5)
            .await
            .expect("alice signs in");
        let mut watch = SessionWatch::new(Arc::clone(&fixture.auth), session, true, &fixture.state);
        tokio::time::sleep(test_support::UNTIL_DUE).await;
        test_support::slow_introspection(&server, &["admins"], "gw", Duration::from_secs(8)).await;
        let started = std::time::Instant::now();
        let verdict = watch.admits_now(&fixture.state).await;
        assert_eq!(
            verdict,
            Err(WS_CLOSE_SESSION_ENDED),
            "a frame was admitted past the session's hard expiry"
        );
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "the refusal waited for the slow check: {:?}",
            started.elapsed()
        );
    }

    /// The same check ends when the socket's gateway generation stops.
    #[tokio::test]
    async fn a_frame_check_cut_short_by_generation_stop_refuses_the_frame() {
        let server = idp(&["admins"], "gw").await;
        let mut config = sign_in_config(&server.uri());
        test_support::revalidate_after(&mut config, 2);
        let fixture = fixture(config);
        let (_, session) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in");
        let mut watch = SessionWatch::new(Arc::clone(&fixture.auth), session, true, &fixture.state);
        tokio::time::sleep(test_support::UNTIL_DUE).await;
        test_support::slow_introspection(&server, &["admins"], "gw", Duration::from_secs(8)).await;
        let stop = fixture.state.shutdown_tx.clone();
        let started = std::time::Instant::now();
        let (verdict, ()) = tokio::join!(watch.admits_now(&fixture.state), async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            stop.send_replace(true);
        });
        assert_eq!(
            verdict,
            Err(WS_CLOSE_GOING_AWAY),
            "a frame was admitted across its generation's stop"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the refusal waited for the slow check: {:?}",
            started.elapsed()
        );
    }

    /// A stream whose watch is built after its generation was told to stop
    /// (the request was still being admitted when the reload began) sees
    /// that stop.
    #[tokio::test]
    async fn a_watch_built_after_its_generation_stopped_is_retired() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let (_, session) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in");
        fixture.state.shutdown_tx.send_replace(true);
        let mut watch = SessionWatch::new(Arc::clone(&fixture.auth), session, true, &fixture.state);
        assert_eq!(
            watch.admits_now(&fixture.state).await,
            Err(WS_CLOSE_GOING_AWAY),
            "a watch built after the stop admitted a frame"
        );
        let code = tokio::time::timeout(Duration::from_secs(2), watch.lost(&fixture.state)).await;
        assert_eq!(code.ok(), Some(WS_CLOSE_GOING_AWAY));
    }

    /// A request that still reaches a stopped generation's router (an open
    /// keep-alive or TLS connection outlives the listener) is refused rather
    /// than admitted under that generation's authority.
    #[tokio::test]
    async fn a_retired_generation_refuses_cookie_requests() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        fixture.state.shutdown_tx.send_replace(true);
        for path in ["/api/operator-probe", "/api/events"] {
            let response = send(
                &fixture.router,
                request(
                    "GET",
                    path,
                    loopback(),
                    &[("cookie", &signed_in.cookie_header())],
                ),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{path} was admitted by a stopped generation"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CONNECTION)
                    .and_then(|value| value.to_str().ok()),
                Some("close"),
                "{path}: the client is told to reconnect, not to reuse this connection"
            );
            assert_eq!(json(response).await["reason"], "gateway_restarting");
        }
    }

    /// A re-verification that a policy publish straddles is tried once more
    /// with the token held, as the grants check is: the new policy still
    /// admits the account, so the session goes on instead of ending.
    #[tokio::test]
    async fn a_reverification_straddling_a_policy_publish_is_tried_again() {
        let server = idp(&["admins"], "gw").await;
        let mut config = sign_in_config(&server.uri());
        test_support::revalidate_after(&mut config, 2);
        let pairing = Arc::new(PairingGuard::new(
            true,
            &config.gateway.paired_tokens,
            PairingCodePolicy::default(),
        ));
        let rpc = Arc::new(RpcInboundAuth::from_config(&config, Arc::clone(&pairing)).unwrap());
        let inbound = Arc::new(GatewayInboundAuth::from_shared(Arc::clone(&rpc)));
        let fixture = fixture_with(
            config.clone(),
            pairing,
            inbound,
            Arc::new(DashboardSessions::default()),
        );
        let signed_in = sign_in(&fixture, &server).await;
        tokio::time::sleep(test_support::UNTIL_DUE).await;
        test_support::slow_introspection(&server, &["admins"], "gw", Duration::from_millis(600))
            .await;
        // Another group mapped: an authorization input alice doesn't depend on.
        let mut republished = config;
        republished
            .oidc
            .get_mut("corp")
            .unwrap()
            .profile_map
            .insert("auditors".into(), "config-reader".into());
        let before = rpc.generation();
        let publisher = Arc::clone(&rpc);
        let (response, ()) = tokio::join!(
            send(
                &fixture.router,
                request(
                    "GET",
                    "/api/operator-probe",
                    loopback(),
                    &[("cookie", &signed_in.cookie_header())],
                ),
            ),
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                publisher
                    .publish_accepted(&republished, publisher.accepted_revision() + 1)
                    .unwrap();
            }
        );
        assert_ne!(rpc.generation(), before, "the publish moved the policy");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a publish during re-verification ended a session the new policy admits"
        );
    }

    /// Introspection requests to `server` that carried alice's access token.
    async fn token_requests(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| request.url.path() == "/introspect")
            .filter(|request| String::from_utf8_lossy(&request.body).contains("at-alice"))
            .count()
    }

    /// Publish `config` as the live configuration and as the accepted policy.
    async fn publish_both(state: &AppState, rpc: &RpcInboundAuth, config: &Config) {
        let commit = state
            .config_authority
            .begin_config_commit()
            .await
            .expect("config commit");
        commit
            .publish(commit.next_revision().expect("revision"), config.clone())
            .expect("publish");
        rpc.publish_accepted(config, rpc.accepted_revision() + 1)
            .unwrap();
    }

    /// The sign-in callback names its provider (the pin) and exchanges the
    /// code from one reading of the configuration, so A's token never goes
    /// out pinned to B however publications interleave. Here the alias is
    /// repointed to B before the callback, back to A between naming the
    /// provider and building the exchange client, and to B again while A
    /// answers the exchange. Another issuer, then another client.
    #[tokio::test]
    async fn a_sign_in_callback_never_pins_a_token_to_another_provider() {
        let gw2_basic = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("gw2:s3cret")
        );
        for repoint in ["issuer", "client"] {
            let a = idp(&["admins"], "gw").await;
            let b = idp(&["admins"], "gw").await;
            let config_a = sign_in_config(&a.uri());
            let mut config_b = config_a.clone();
            let corp = config_b.oidc.get_mut("corp").unwrap();
            if repoint == "issuer" {
                corp.issuer = b.uri();
            } else {
                corp.client_id = "gw2".into();
                corp.interactive_clients = vec!["gw2".into()];
            }
            let pairing = Arc::new(PairingGuard::new(
                true,
                &config_a.gateway.paired_tokens,
                PairingCodePolicy::default(),
            ));
            let rpc =
                Arc::new(RpcInboundAuth::from_config(&config_a, Arc::clone(&pairing)).unwrap());
            let inbound = Arc::new(GatewayInboundAuth::from_shared(Arc::clone(&rpc)));
            let fixture = fixture_with(
                config_a.clone(),
                pairing,
                inbound,
                Arc::new(DashboardSessions::default()),
            );
            let started = start(&fixture, "/auth/sign-in/corp?return=/chat").await;
            let issuer = a.uri();
            Mock::given(http_method("POST"))
                .and(http_path("/token"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({
                            "access_token": "at-alice",
                            "token_type": "Bearer",
                            "expires_in": 600,
                            "id_token": id_token(serde_json::json!({
                                "iss": issuer,
                                "aud": "gw",
                                "sub": "alice",
                                "exp": unix_now() + 600,
                                "nonce": started.nonce,
                            })),
                        }))
                        .set_delay(Duration::from_millis(600)),
                )
                .mount(&a)
                .await;

            publish_both(&fixture.state, &rpc, &config_b).await;
            let (state, publisher, restored) =
                (fixture.state.clone(), Arc::clone(&rpc), config_a.clone());
            *crate::api_oidc::BEFORE_EXCHANGE_CLIENT.lock() = Some((
                started.state.clone(),
                Box::pin(async move { publish_both(&state, &publisher, &restored).await }),
            ));
            let (state, publisher, repointed) =
                (fixture.state.clone(), Arc::clone(&rpc), config_b.clone());
            let (response, ()) = tokio::join!(
                send(&fixture.router, callback(&started, Some(&started.binding))),
                async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    publish_both(&state, &publisher, &repointed).await;
                }
            );
            assert!(
                !set_cookies(&response)
                    .iter()
                    .any(|c| c.starts_with(SESSION_COOKIE)),
                "{repoint}: the callback signed somebody in"
            );
            assert_eq!(
                token_requests(&b).await,
                0,
                "{repoint}: A's token was sent to another issuer"
            );
            let sent_as_gw2 = a
                .received_requests()
                .await
                .unwrap_or_default()
                .iter()
                .filter(|request| request.url.path() == "/introspect")
                .filter(|request| String::from_utf8_lossy(&request.body).contains("at-alice"))
                .filter(|request| {
                    request
                        .headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        == Some(gw2_basic.as_str())
                })
                .count();
            assert_eq!(
                sent_as_gw2, 0,
                "{repoint}: A's token was sent as another client"
            );
        }
    }

    /// A sign-in whose provider was repointed before its token was verified
    /// (the pin it carries no longer matches the accepted policy) is
    /// refused, and its token is sent to no provider.
    #[tokio::test]
    async fn a_sign_in_never_sends_its_token_to_a_repointed_provider() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        for (case, pin) in [
            (
                "another issuer",
                ProviderPin {
                    issuer: "https://elsewhere.example".into(),
                    client_id: "gw".into(),
                },
            ),
            (
                "another client",
                ProviderPin {
                    issuer: server.uri(),
                    client_id: "gw2".into(),
                },
            ),
        ] {
            let refused = fixture
                .auth
                .sign_in("corp", pin, "at-alice".into(), 3600)
                .await;
            assert!(refused.is_err(), "{case}: the sign-in was admitted");
        }
        assert_eq!(token_requests(&server).await, 0, "the token was sent");
    }

    /// A re-verification that a publish straddles never sends the held
    /// token to the provider the alias names afterwards. Repointed at
    /// another issuer, or at another client of the same issuer, the session
    /// ends, and the token reaches no provider it wasn't issued for.
    #[tokio::test]
    async fn a_reverification_never_sends_the_token_to_a_repointed_provider() {
        for repoint in ["issuer", "client"] {
            let first = idp(&["admins"], "gw").await;
            let second = idp(&["admins"], "gw").await;
            let mut config = sign_in_config(&first.uri());
            test_support::revalidate_after(&mut config, 2);
            let pairing = Arc::new(PairingGuard::new(
                true,
                &config.gateway.paired_tokens,
                PairingCodePolicy::default(),
            ));
            let rpc = Arc::new(RpcInboundAuth::from_config(&config, Arc::clone(&pairing)).unwrap());
            let inbound = Arc::new(GatewayInboundAuth::from_shared(Arc::clone(&rpc)));
            let fixture = fixture_with(
                config.clone(),
                pairing,
                inbound,
                Arc::new(DashboardSessions::default()),
            );
            let signed_in = sign_in(&fixture, &first).await;
            tokio::time::sleep(test_support::UNTIL_DUE).await;
            test_support::slow_introspection(&first, &["admins"], "gw", Duration::from_millis(600))
                .await;
            let mut repointed = config;
            let corp = repointed.oidc.get_mut("corp").unwrap();
            if repoint == "issuer" {
                corp.issuer = second.uri();
            } else {
                corp.client_id = "gw2".into();
                corp.interactive_clients = vec!["gw2".into()];
            }
            let sent_before = token_requests(&first).await;
            let publisher = Arc::clone(&rpc);
            let (response, ()) = tokio::join!(
                send(
                    &fixture.router,
                    request(
                        "GET",
                        "/api/operator-probe",
                        loopback(),
                        &[("cookie", &signed_in.cookie_header())],
                    ),
                ),
                async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    publisher
                        .publish_accepted(&repointed, publisher.accepted_revision() + 1)
                        .unwrap();
                }
            );
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{repoint}: a session survived its provider being repointed"
            );
            assert_eq!(
                token_requests(&second).await,
                0,
                "{repoint}: the token was sent to the provider the alias names now"
            );
            assert_eq!(
                token_requests(&first).await - sent_before,
                1,
                "{repoint}: the token was sent again, under the repointed client"
            );
        }
    }

    /// A generation whose stop channel has closed is over: nothing can serve
    /// it or tell it anything more. So it reads as retired, and so does a
    /// socket's watch on it, even when the stop it was sent went unseen.
    #[tokio::test]
    async fn a_closed_stop_channel_reads_as_retired() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let (_, session) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in");
        let mut watch = SessionWatch::new(Arc::clone(&fixture.auth), session, true, &fixture.state);
        let mut auth = DashboardAuth::new(
            Arc::new(DashboardSessions::default()),
            inbound_for(&sign_in_config(&server.uri())),
        );
        let (stop, stopping) = tokio::sync::watch::channel(false);
        auth.stopping = Some(stopping.clone());
        watch.stopping = stopping;
        stop.send_replace(true);
        drop(stop);
        assert!(
            auth.is_retired(),
            "a closed channel read as a live generation"
        );
        assert_eq!(
            watch.known_lost(),
            Some(WS_CLOSE_GOING_AWAY),
            "a socket's watch on a closed generation saw no stop"
        );
    }

    /// A cookie request still verifying its session when its generation
    /// stops gets the gateway's restart answer, shaped for its protocol:
    /// HTTP/1.1 is told to close, HTTP/2 gets no `Connection` header (its
    /// connection gets GOAWAY instead). The route never runs.
    #[tokio::test]
    async fn a_cookie_request_held_across_the_stop_gets_its_protocols_restart_answer() {
        use axum::http::Version;
        for version in [Version::HTTP_11, Version::HTTP_2] {
            let server = idp(&["admins"], "gw").await;
            let mut config = sign_in_config(&server.uri());
            test_support::revalidate_after(&mut config, 2);
            let fixture = fixture(config);
            let signed_in = sign_in(&fixture, &server).await;
            tokio::time::sleep(test_support::UNTIL_DUE).await;
            test_support::slow_introspection(&server, &["admins"], "gw", Duration::from_secs(2))
                .await;
            let mut held = request(
                "GET",
                "/api/operator-probe",
                loopback(),
                &[("cookie", &signed_in.cookie_header())],
            );
            *held.version_mut() = version;
            let stop = fixture.state.shutdown_tx.clone();
            let (response, ()) = tokio::join!(send(&fixture.router, held), async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                stop.send_replace(true);
            });
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{version:?}: a request held across the stop was admitted"
            );
            let headers = response.headers();
            assert_eq!(
                headers
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
                Some("1"),
                "{version:?}"
            );
            let connection = headers
                .get(header::CONNECTION)
                .and_then(|value| value.to_str().ok());
            if version == Version::HTTP_2 {
                assert_eq!(connection, None, "HTTP/2 has no Connection header");
            } else {
                assert_eq!(connection, Some("close"), "{version:?}");
            }
            assert_eq!(
                json(response).await,
                serde_json::json!({
                    "error": "The gateway is restarting. Try again.",
                    "reason": "gateway_restarting",
                }),
                "{version:?}"
            );
        }
    }

    /// A socket whose session ends while its periodic recheck waits on the
    /// identity provider drops that recheck, and the token copy it holds,
    /// at once.
    #[tokio::test]
    async fn a_lost_watch_drops_its_pending_recheck() {
        let server = idp(&["admins"], "gw").await;
        let mut config = sign_in_config(&server.uri());
        test_support::revalidate_after(&mut config, 2);
        let fixture = fixture(config);
        let (_, session) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in");
        let mut watch = SessionWatch::new(
            Arc::clone(&fixture.auth),
            Arc::clone(&session),
            true,
            &fixture.state,
        );
        tokio::time::sleep(test_support::UNTIL_DUE).await;
        watch.recheck = tokio::time::interval(Duration::from_millis(1));
        test_support::slow_introspection(&server, &["admins"], "gw", Duration::from_secs(30)).await;
        let mut watch = Some(watch);
        let _ = tokio::time::timeout(
            Duration::from_millis(200),
            session_lost(&mut watch, &fixture.state),
        )
        .await;
        assert!(
            watch.as_ref().unwrap().pending.is_some(),
            "a recheck is waiting on the provider"
        );
        assert!(fixture.auth.end(&session));
        let code = tokio::time::timeout(
            Duration::from_secs(2),
            session_lost(&mut watch, &fixture.state),
        )
        .await
        .expect("the watch ends at once");
        assert_eq!(code, WS_CLOSE_SESSION_ENDED);
        assert!(
            watch.as_ref().unwrap().pending.is_none(),
            "the ended watch still holds its pending recheck"
        );
    }

    /// Sign-out also clears the cookie of a session that already ended, so
    /// signing out from the re-auth prompt leaves the browser no cookie. A
    /// foreign page still cannot.
    #[tokio::test]
    async fn sign_out_clears_the_cookie_of_an_ended_session() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let (cookie, _) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 0)
            .await
            .expect("alice signs in");
        let cookie = cookie_for(&cookie);
        for attempt in ["after expiry", "again"] {
            let response = send(
                &fixture.router,
                request(
                    "POST",
                    "/auth/sign-out",
                    loopback(),
                    &[("cookie", &cookie), ("origin", OWN_ORIGIN)],
                ),
            )
            .await;
            assert!(
                set_cookies(&response)
                    .iter()
                    .any(|c| c.starts_with(SESSION_COOKIE) && c.contains("Max-Age=0")),
                "sign-out {attempt} left the cookie in place"
            );
        }
        let foreign = send(
            &fixture.router,
            request(
                "POST",
                "/auth/sign-out",
                loopback(),
                &[("cookie", &cookie), ("origin", "http://evil.example")],
            ),
        )
        .await;
        assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
        assert!(
            !set_cookies(&foreign)
                .iter()
                .any(|c| c.starts_with(SESSION_COOKIE)),
            "a foreign page cleared the cookie"
        );
        assert_eq!(json(foreign).await["reason"], "origin");
    }

    /// The event stream a signed-in administrator opened ends with the
    /// session (sign-out, expiry) and with its generation (reload), and
    /// delivers nothing published after that.
    #[tokio::test]
    async fn an_event_stream_ends_with_its_session_and_generation() {
        let server = idp(&["admins"], "gw").await;
        for case in ["sign-out", "expiry", "reload"] {
            let fixture = fixture(sign_in_config(&server.uri()));
            let lifetime = if case == "expiry" { 2 } else { 3600 };
            let (cookie, session) = fixture
                .auth
                .sign_in(
                    "corp",
                    pin_for(&fixture, "corp"),
                    "at-alice".into(),
                    lifetime,
                )
                .await
                .expect("alice signs in");
            let response = send(
                &fixture.router,
                request(
                    "GET",
                    "/api/events",
                    loopback(),
                    &[("cookie", &cookie_for(&cookie))],
                ),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{case}: the stream opens"
            );
            let mut body = response.into_body();
            match case {
                "sign-out" => assert!(fixture.auth.end(&session)),
                "reload" => {
                    fixture.state.shutdown_tx.send_replace(true);
                }
                _ => tokio::time::sleep(Duration::from_secs(3)).await,
            }
            let _ = fixture
                .state
                .event_tx
                .send(serde_json::json!({"type": "published-after-the-end"}));
            let delivered = tokio::time::timeout(Duration::from_secs(5), async {
                let mut seen = String::new();
                while let Some(frame) = body.frame().await {
                    if let Ok(data) = frame.expect("stream frame").into_data() {
                        seen.push_str(&String::from_utf8_lossy(&data));
                    }
                }
                seen
            })
            .await
            .unwrap_or_else(|_| panic!("{case}: the stream outlived its session"));
            assert!(
                !delivered.contains("published-after-the-end"),
                "{case}: an event published after the end was delivered"
            );
        }
    }

    /// The SOP runs feed closes with its session and with its generation,
    /// whether or not the engine has a run notifier attached.
    #[tokio::test]
    async fn a_sop_runs_feed_closes_with_its_session_and_generation() {
        let server = idp(&["admins"], "gw").await;
        for notifier in [true, false] {
            for case in ["sign-out", "reload"] {
                let config = sign_in_config(&server.uri());
                let mut engine = zeroclaw_runtime::sop::SopEngine::new(
                    zeroclaw_config::schema::SopConfig::default(),
                );
                if notifier {
                    engine = engine.with_run_notifier(tokio::sync::broadcast::channel(8).0);
                }
                let fixture = build_generation(
                    config.clone(),
                    pairing_for(&config),
                    inbound_for(&config),
                    Arc::new(DashboardSessions::default()),
                    None,
                    Some(Arc::new(std::sync::Mutex::new(engine))),
                );
                let (cookie, session) = fixture
                    .auth
                    .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 3600)
                    .await
                    .expect("alice signs in");
                let (addr, served) = test_support::serve(fixture.router.clone()).await;
                let mut socket = test_support::connect(addr, "/ws/sop/runs", &cookie_for(&cookie))
                    .await
                    .expect("an administrator opens the SOP runs feed");
                let expected = if case == "sign-out" {
                    assert!(fixture.auth.end(&session));
                    WS_CLOSE_SESSION_ENDED
                } else {
                    fixture.state.shutdown_tx.send_replace(true);
                    WS_CLOSE_GOING_AWAY
                };
                let (code, _) =
                    test_support::until_closed(&mut socket, Duration::from_secs(3)).await;
                assert_eq!(code, Some(expected), "notifier={notifier}, {case}");
                served.abort();
            }
        }
    }

    /// Each generation's idle sweep stops when that generation's sign-in
    /// state is dropped, and the next generation's sweep carries on over
    /// the same store.
    #[tokio::test]
    async fn a_dropped_generations_sweep_stops_and_the_next_one_sweeps() {
        let server = idp(&["admins"], "gw").await;
        let config = sign_in_config(&server.uri());
        let sessions = Arc::new(DashboardSessions::default());
        let old = fixture_with(
            config.clone(),
            pairing_for(&config),
            inbound_for(&config),
            Arc::clone(&sessions),
        );
        let old_sweep = old
            .auth
            ._sweeper
            .0
            .clone()
            .expect("a generation in a runtime sweeps");
        drop(old);
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(
            old_sweep.is_finished(),
            "a dropped generation's sweep keeps running"
        );

        let new = fixture_with(
            config.clone(),
            pairing_for(&config),
            inbound_for(&config),
            Arc::clone(&sessions),
        );
        let auth = verified_auth(&new).await;
        let (_cookie, view) = sessions
            .insert(new_session(auth, &new.auth.inbound, 0), unix_now())
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(sessions.len(), 0, "the next generation's sweep removed it");
        assert!(view.ended.is_cancelled());
    }

    /// An unsafe request carrying the cookie and the right CSRF token but no
    /// `Origin`, or a cross-site `Sec-Fetch-Site`, is refused.
    #[tokio::test]
    async fn an_unsafe_request_without_its_own_origin_is_refused() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let signed_in = sign_in(&fixture, &server).await;
        let cookie = signed_in.cookie_header();
        let cases: [(&str, Vec<(&str, &str)>); 2] = [
            (
                "no Origin",
                vec![
                    ("cookie", cookie.as_str()),
                    (CSRF_HEADER, signed_in.csrf.as_str()),
                ],
            ),
            (
                "cross-site fetch",
                vec![
                    ("cookie", cookie.as_str()),
                    (CSRF_HEADER, signed_in.csrf.as_str()),
                    ("origin", OWN_ORIGIN),
                    ("sec-fetch-site", "cross-site"),
                ],
            ),
        ];
        for (case, headers) in cases {
            let response = send(
                &fixture.router,
                request("POST", "/auth/sign-out", loopback(), &headers),
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{case}");
            assert_eq!(json(response).await["reason"], "origin", "{case}");
        }
        let (status, _) = probe(&fixture, &[("cookie", &cookie)]).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the refused sign-outs ended nothing"
        );
    }

    async fn set_sign_in(state: &AppState, on: bool) {
        let commit = state
            .config_authority
            .begin_config_commit()
            .await
            .expect("config commit");
        let mut config = commit.current_config();
        config.gateway.dashboard_sign_in.oidc = if on { vec!["corp".into()] } else { Vec::new() };
        commit
            .publish(commit.next_revision().expect("revision"), config)
            .expect("publish");
    }

    /// Sign-in turned off ends every session even when no request arrives
    /// before it is turned back on: the next sweep sees it off.
    #[tokio::test]
    async fn sign_in_turned_off_ends_sessions_without_a_request() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let (cookie, session) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in");
        set_sign_in(&fixture.state, false).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::resume();
        set_sign_in(&fixture.state, true).await;
        assert!(
            session.ended.is_cancelled(),
            "a session outlived sign-in being turned off"
        );
        let (status, body) = probe(&fixture, &[("cookie", &cookie_for(&cookie))]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["reason"], "session_invalid");
    }

    /// A gateway generation built with sign-in off (the reload that turned
    /// it off) ends every session at once, before any request.
    #[tokio::test]
    async fn a_generation_built_with_sign_in_off_ends_every_session() {
        let server = idp(&["admins"], "gw").await;
        let config = sign_in_config(&server.uri());
        let sessions = Arc::new(DashboardSessions::default());
        let on = fixture_with(
            config.clone(),
            pairing_for(&config),
            inbound_for(&config),
            Arc::clone(&sessions),
        );
        let (cookie, session) = on
            .auth
            .sign_in("corp", pin_for(&on, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in");
        let mut off_config = config.clone();
        off_config.gateway.dashboard_sign_in.oidc.clear();
        let _off = fixture_with(
            off_config.clone(),
            pairing_for(&off_config),
            inbound_for(&off_config),
            Arc::clone(&sessions),
        );
        assert!(
            session.ended.is_cancelled(),
            "a session outlived a generation with sign-in off"
        );
        assert_eq!(sessions.len(), 0);
        let again = fixture_with(
            config.clone(),
            pairing_for(&config),
            inbound_for(&config),
            Arc::clone(&sessions),
        );
        let (status, body) = probe(&again, &[("cookie", &cookie_for(&cookie))]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["reason"], "session_invalid");
    }

    // ── A retired generation and an ended session's recheck ───────────

    /// A retired generation whose sign-in is off (a reload turned it off and
    /// a kept-alive connection still holds it) never ends the sessions of
    /// the generation that replaced it: not through a request, not through
    /// its sweep.
    #[tokio::test]
    async fn a_retired_generation_never_ends_the_next_generations_sessions() {
        let server = idp(&["admins"], "gw").await;
        let config = sign_in_config(&server.uri());
        let sessions = Arc::new(DashboardSessions::default());
        let mut off_config = config.clone();
        off_config.gateway.dashboard_sign_in.oidc.clear();
        let old = fixture_with(
            off_config.clone(),
            pairing_for(&off_config),
            inbound_for(&off_config),
            Arc::clone(&sessions),
        );
        old.state.shutdown_tx.send_replace(true);
        let new = fixture_with(
            config.clone(),
            pairing_for(&config),
            inbound_for(&config),
            Arc::clone(&sessions),
        );
        let (cookie, session) = new
            .auth
            .sign_in("corp", pin_for(&new, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in to the new generation");

        let _ = send(
            &old.router,
            request(
                "GET",
                "/api/operator-probe",
                loopback(),
                &[("cookie", &cookie_for(&cookie))],
            ),
        )
        .await;
        assert!(
            !session.ended.is_cancelled(),
            "a request through the retired generation ended the new generation's session"
        );

        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(61)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::resume();
        assert!(
            !session.ended.is_cancelled(),
            "the retired generation's sweep ended the new generation's session"
        );
        let (status, _) = probe(&new, &[("cookie", &cookie_for(&cookie))]).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the session still admits its request"
        );
    }

    /// An ended session's recheck, with the token copy it holds, goes at
    /// once even when the socket never polls its watch again: a stalled
    /// writer, or a response body the transport stopped polling.
    #[tokio::test]
    async fn an_ended_sessions_recheck_goes_without_being_polled() {
        let server = idp(&["admins"], "gw").await;
        let mut config = sign_in_config(&server.uri());
        test_support::revalidate_after(&mut config, 2);
        let fixture = fixture(config);
        let (_, session) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in");
        let mut watch = SessionWatch::new(
            Arc::clone(&fixture.auth),
            Arc::clone(&session),
            true,
            &fixture.state,
        );
        watch.recheck = tokio::time::interval(Duration::from_millis(1));
        tokio::time::sleep(test_support::UNTIL_DUE).await;
        test_support::slow_introspection(&server, &["admins"], "gw", Duration::from_secs(30)).await;
        let held = Arc::strong_count(&session);
        let mut watch = Some(watch);
        let _ = tokio::time::timeout(
            Duration::from_millis(200),
            session_lost(&mut watch, &fixture.state),
        )
        .await;
        assert!(
            Arc::strong_count(&session) > held,
            "a recheck is waiting on the provider"
        );

        assert!(fixture.auth.end(&session));
        let released = tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&session) > held {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .is_ok();
        assert!(
            released,
            "the ended session's recheck waits for the socket to poll it"
        );
        drop(watch);
    }

    /// An event stream whose body the transport stopped polling still lets
    /// go of its session the moment the session ends: nothing of it waits
    /// for the body to be polled again.
    #[tokio::test]
    async fn a_suspended_event_stream_lets_go_of_its_ended_session() {
        let server = idp(&["admins"], "gw").await;
        let fixture = fixture(sign_in_config(&server.uri()));
        let (cookie, session) = fixture
            .auth
            .sign_in("corp", pin_for(&fixture, "corp"), "at-alice".into(), 3600)
            .await
            .expect("alice signs in");
        let baseline = Arc::strong_count(&fixture.auth);
        let response = send(
            &fixture.router,
            request(
                "GET",
                "/api/events",
                loopback(),
                &[("cookie", &cookie_for(&cookie))],
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "the stream opens");
        // The body is held and never polled again, as when the transport
        // waits for room to write.
        let body = response.into_body();
        assert!(
            Arc::strong_count(&fixture.auth) > baseline,
            "the open stream watches its session"
        );
        assert!(fixture.auth.end(&session));
        let released = tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&fixture.auth) > baseline {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .is_ok();
        assert!(
            released,
            "a suspended event stream kept its ended session's watch"
        );
        drop(body);
    }
}
