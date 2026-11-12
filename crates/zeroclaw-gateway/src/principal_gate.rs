//! Route-layer authentication for the gateway's configuration and
//! onboarding surfaces.
//!
//! Structural enforcement: the middleware is attached via `route_layer`
//! on the route group, so a handler added to the group cannot forget the
//! check (the per-handler `require_auth` convention this replaces was
//! enforced only by reviewer vigilance).
//!
//! The layer also consumes the shared principal model (RFC 7141): a
//! paired native bearer resolves to the shared operator exactly as
//! before, while a bearer presented with the `X-ZeroClaw-Auth-Provider`
//! header naming an `oidc.<alias>` provider is verified by that provider
//! and resolved to a scoped principal whose Config grants gate the
//! request. Provider selection is explicit, mirroring the RPC handshake's
//! `auth_provider` field: the named provider's denial is authoritative,
//! and there is no fallback between providers.
//!
//! Grants are enforced in two places. The route layer applies a coarse
//! floor per HTTP method (a read needs `Read`; anything else needs some
//! mutating verb) so a read-only principal never reaches a mutating
//! handler. Each mutating handler then authorizes its complete write set
//! before its first side effect: every config path it will persist,
//! classified by effect (`Create` for a path it brings into being,
//! `Delete` for one it removes, `Update` otherwise) and matched against
//! the principal's config path selectors, with the persist boundary
//! refusing anything the handler did not authorize
//! ([`ConfigWriteAuthorization`]). That handler check runs under the config
//! write lock against the grants the principal holds then, not the ones
//! stamped at admission, so a revocation, expiry or unpairing that lands
//! while the request body arrives or the lock is awaited binds the write.
//!
//! Policy itself moves only at that persist boundary: the handler that
//! writes a configuration publishes the policy compiled from it as the
//! next accepted revision, and requests are verified and resolved against
//! the accepted snapshot as it stands. Nothing on the request path
//! compiles policy, so a request that read the configuration before a
//! concurrent persist can never reinstall the older policy over the
//! newer one.
//!
//! Verifying an OIDC bearer is work an unauthenticated caller makes the
//! gateway do, and with `validation = "introspection"` it is one
//! authenticated round trip to the identity provider under the daemon's
//! own client credentials. Two limits bound it, held in
//! [`BearerVerificationLimits`] and scoped to this path alone: a
//! per-client attempt ledger that books refused credentials (the same
//! thresholds and loopback exemption as the pairing and enrollment
//! ledgers, on a ledger of its own, so a token replayed past its expiry
//! never locks its operator out of pairing), and a process-wide cap on
//! the verifications in flight at once, so a burst cannot hold many
//! connections open to the provider or have the daemon's client throttled
//! there. A transport failure on the gateway's own leg to the provider,
//! and a verified identity the policy grants nothing, say nothing about
//! the caller and are not booked. Dashboard sessions are classified
//! before this path and verified again by their own layer, which never
//! competes for these limits.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::sync::Semaphore;
use zeroclaw_api::grants::{ResolvedGrants, Resource, Verb, WILDCARD};
use zeroclaw_api::jsonrpc::error_codes::{AUTH_REQUIRED, FORBIDDEN};
use zeroclaw_config::live::LiveConfigHandle;
use zeroclaw_config::pairing::PairingGuard;
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::rpc::auth::{AuthDenied, ConnectionAuth, RpcInboundAuth};
use zeroclaw_runtime::rpc::transport::TransportKind;
use zeroclaw_runtime::security::auth_provider::Credential;

use crate::ConfigWriteGuard;
use crate::dashboard_session::{DashboardCaller, SessionAuth, SessionRejection};

/// Header naming the auth provider to verify the bearer with, mirroring
/// the RPC handshake's `auth_provider` field (e.g. `oidc.corp`). Absent
/// means the native pairing provider, exactly as before this layer.
pub const AUTH_PROVIDER_HEADER: &str = "x-zeroclaw-auth-provider";

/// Bearer verifications allowed in flight at once, across all clients.
/// Sized like the enrollment relay's outbound cap: it is the bound that
/// still applies where the per-client ledger does not (a loopback caller,
/// or a reverse proxy that collapses every client onto one address
/// because forwarded headers are not trusted). Exceeding it refuses the
/// request instead of queueing it.
pub(crate) const OUTBOUND_BEARER_CAP: usize = 16;

/// `Retry-After` for the capacity refusal. A slot frees as soon as a
/// verification in flight finishes, so the hint is short.
const CAPACITY_RETRY_AFTER_SECS: u64 = 5;

/// The two limits that bound what an unauthenticated bearer can make this
/// route group do; see the module docs.
pub struct BearerVerificationLimits {
    /// Refused credentials per client. Its own ledger, deliberately not
    /// `AppState::auth_limiter`: a lockout earned here must not reach
    /// pairing or webhook auth, nor theirs reach bearer verification.
    pub(crate) attempts: crate::auth_rate_limit::AuthRateLimiter,
    /// Verifications in flight. A permit is held for the verification
    /// alone, never across the handler the request goes on to.
    pub(crate) outbound: Semaphore,
}

impl Default for BearerVerificationLimits {
    fn default() -> Self {
        Self {
            attempts: crate::auth_rate_limit::AuthRateLimiter::new(),
            outbound: Semaphore::new(OUTBOUND_BEARER_CAP),
        }
    }
}

/// Router-wide state for [`config_route_auth`]: the inbound-auth authority
/// the route group verifies against, the limits on verifying, and how the
/// client behind a request is told apart (the forwarded address only where
/// the deployment trusts its proxy, the peer address otherwise, as every
/// other rate-limited gateway surface decides it).
#[derive(Clone)]
pub struct ConfigRouteAuth {
    pub inbound: Arc<GatewayInboundAuth>,
    pub limits: Arc<BearerVerificationLimits>,
    /// Shared canonical configuration, read when deriving each bearer key.
    pub config: LiveConfigHandle,
}

impl ConfigRouteAuth {
    /// The route state over `inbound` with fresh limits.
    pub fn new(inbound: &Arc<GatewayInboundAuth>, config: &LiveConfigHandle) -> Self {
        Self {
            inbound: Arc::clone(inbound),
            limits: Arc::new(BearerVerificationLimits::default()),
            config: config.clone(),
        }
    }
}

/// The gateway's inbound-auth authority: the provider registry and
/// principal resolver the RPC layer uses.
///
/// Under the daemon it is the RPC context's own authority
/// ([`Self::from_shared`]): one accepted policy per daemon generation, into
/// which both surfaces publish under the process-wide config write lock. A
/// revocation persisted through the gateway or through RPC therefore binds
/// the other surface before the writer returns. A standalone gateway builds
/// its own from config ([`Self::from_config`]).
///
/// The accepted policy (providers, their verification settings, profile
/// mappings, grants) is compiled at construction and thereafter only by a
/// persist boundary publishing the configuration it has just written: the
/// gateway's `publish_persisted`, or the RPC context's save.
pub struct GatewayInboundAuth {
    inner: Arc<RpcInboundAuth>,
}

impl GatewayInboundAuth {
    /// A standalone gateway's own authority, compiled from `config`.
    pub fn from_config(config: &Config, pairing: Arc<PairingGuard>) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Arc::new(RpcInboundAuth::from_config(config, pairing)?),
        })
    }

    /// The daemon generation's authority, shared with the RPC context. The
    /// gateway then authenticates against, and publishes into, the same
    /// accepted policy RPC does.
    pub fn from_shared(inner: Arc<RpcInboundAuth>) -> Self {
        Self { inner }
    }

    fn pairing(&self) -> &Arc<PairingGuard> {
        self.inner.pairing()
    }

    /// The authorization-policy generation currently in force.
    pub fn generation(&self) -> u64 {
        self.inner.generation()
    }

    /// Publish the policy compiled from a configuration the persist
    /// boundary has just written, as the next accepted revision.
    ///
    /// Callers hold the process-wide config write lock, the same lock the
    /// RPC context's save holds, so revisions from both surfaces are issued
    /// in one persist order. A writer that was slower to publish can never
    /// reinstall policy a later persist superseded: a revision that is not
    /// newer than the accepted one is refused as a no-op.
    pub(crate) fn publish_persisted(&self, persisted: &Config) -> anyhow::Result<u64> {
        let revision = self.inner.accepted_revision().saturating_add(1);
        self.inner.publish_accepted(persisted, revision)
    }

    /// The grants `auth` holds under the accepted policy in force now; see
    /// [`RpcInboundAuth::current_grants`].
    pub(crate) fn current_grants(
        &self,
        auth: &ConnectionAuth,
    ) -> Result<zeroclaw_api::grants::ResolvedGrants, AuthDenied> {
        self.inner.current_grants(auth)
    }

    /// A handle naming this authority, for a binding that must notice when
    /// a later generation's authority replaced it.
    pub(crate) fn authority_ref(&self) -> std::sync::Weak<RpcInboundAuth> {
        Arc::downgrade(&self.inner)
    }

    /// Whether `authority` names this authority.
    pub(crate) fn is_authority(&self, authority: &std::sync::Weak<RpcInboundAuth>) -> bool {
        std::sync::Weak::ptr_eq(authority, &Arc::downgrade(&self.inner))
    }

    /// Verify a bearer against an explicitly selected provider and
    /// resolve it to a principal with grants, both against the accepted
    /// policy snapshot as it stands.
    pub(crate) async fn authenticate_scoped(
        &self,
        token: &str,
        provider: &str,
    ) -> Result<ConnectionAuth, AuthDenied> {
        self.inner
            .authenticate(
                TransportKind::Wss,
                Credential::None,
                Some(token),
                Some(provider),
            )
            .await
    }

    /// [`Self::authenticate_scoped`] for a bearer bound to the provider as
    /// it was configured when the bearer was issued: refused before it is
    /// sent if the accepted policy's provider by that name is another one
    /// now. See [`RpcInboundAuth::authenticate_bound`].
    pub(crate) async fn authenticate_bound(
        &self,
        token: &str,
        provider: &str,
        binding: &zeroclaw_runtime::rpc::auth::OidcBinding,
    ) -> Result<ConnectionAuth, AuthDenied> {
        self.inner
            .authenticate_bound(TransportKind::Wss, token, provider, binding)
            .await
    }

    /// Hold the accepted policy through a synchronous certificate issuance,
    /// so a concurrent provider/profile revocation cannot race the effect.
    pub(crate) fn hold_authority(&self) -> zeroclaw_runtime::rpc::auth::AuthorityLease<'_> {
        self.inner.hold_authority()
    }

    /// Verify a native pairing bearer (the pre-existing gateway
    /// credential) into a shared-operator principal.
    async fn authenticate_native(&self, token: &str) -> Result<ConnectionAuth, AuthDenied> {
        self.inner
            .authenticate(TransportKind::Wss, Credential::None, Some(token), None)
            .await
    }
}

/// The exact denial the per-handler `require_auth` produced, preserved
/// for every native-path failure so clients observe no shape change.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
        })),
    )
        .into_response()
}

fn set_retry_after(response: &mut Response, secs: u64) {
    if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
}

/// Refusal for a client the bearer ledger has locked out: nothing is sent
/// to the provider.
fn too_many_attempts(retry_after_secs: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "error": format!(
                "Too many failed bearer verifications. Try again in {retry_after_secs}s."
            ),
            "retry_after": retry_after_secs,
        })),
    )
        .into_response();
    set_retry_after(&mut response, retry_after_secs);
    response
}

/// Refusal when the outbound cap is reached: nothing is sent to the
/// provider, and the client is told when a slot is likely to be free.
fn verification_busy() -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": "bearer verification is busy; retry shortly" })),
    )
        .into_response();
    set_retry_after(&mut response, CAPACITY_RETRY_AFTER_SECS);
    response
}

/// The client a bearer verification is billed to. The peer address comes
/// from the connection (`ConnectInfo`), which the gateway's serving
/// attaches to every request; a request without one keys as `unknown`,
/// which is not loopback and so not exempt.
fn bearer_client_key(route: &ConfigRouteAuth, request: &Request) -> String {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    crate::client_key_from_request(
        peer,
        request.headers(),
        route.config.read().gateway.trust_forwarded_headers,
    )
}

fn denied_response(denied: &AuthDenied) -> Response {
    let status = if denied.code == FORBIDDEN {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::UNAUTHORIZED
    };
    (status, Json(serde_json::json!({ "error": denied.message }))).into_response()
}

fn forbidden(message: impl Into<String>) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response()
}

/// A refused config write: the response the handler returns in place of
/// the mutation. A 403 carries which path or grant fell short; a 401 means
/// the credential that admitted the request is no longer live.
#[derive(Debug)]
pub struct WriteDenied {
    status: StatusCode,
    message: String,
    /// The session layer's reason, for a write whose dashboard session
    /// ended, so the dashboard answers it as it answers any ended session.
    reason: Option<&'static str>,
}

impl WriteDenied {
    fn forbidden(message: String) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message,
            reason: None,
        }
    }

    fn authority_lost(denied: AuthDenied) -> Self {
        let status = if denied.code == FORBIDDEN {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        };
        Self {
            status,
            message: denied.message,
            reason: None,
        }
    }

    fn session_ended(rejection: SessionRejection) -> Self {
        Self {
            status: rejection.status(),
            message: rejection.message().to_owned(),
            reason: Some(rejection.reason()),
        }
    }
}

impl IntoResponse for WriteDenied {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "error": self.message });
        if let Some(reason) = self.reason {
            body["reason"] = reason.into();
        }
        (self.status, Json(body)).into_response()
    }
}

/// The coarse floor a request must clear at the route layer for its HTTP
/// method: a plain read needs `Read`; any other method must hold at least
/// one mutating verb on the Config resource, so a read-only principal
/// never reaches a mutating handler (compute-only POST routes included).
/// Which mutating verb a request really needs is decided per config path
/// by the handler's complete write set, see [`ConfigWriteSet`]. A method
/// outside the known set is refused outright.
fn method_floor(method: &Method) -> Option<&'static [Verb]> {
    match *method {
        Method::GET | Method::HEAD => Some(&[Verb::Read]),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE => {
            Some(&[Verb::Create, Verb::Update, Verb::Delete])
        }
        _ => None,
    }
}

/// Whether `grants` clear `floor`: some verb in it is permitted on the
/// Config resource.
fn clears_floor(floor: &[Verb], grants: &ResolvedGrants) -> bool {
    floor
        .iter()
        .any(|verb| grants.permits(Resource::Config, *verb))
}

/// Whether `grants` may read configuration: the floor the config routes
/// apply to a `GET`. The dashboard's session response reports it, so the
/// dashboard offers the config pages only to an account these routes admit.
pub(crate) fn may_read_config(grants: &ResolvedGrants) -> bool {
    method_floor(&Method::GET).is_some_and(|floor| clears_floor(floor, grants))
}

/// What the route layer hands every admitted request, for the handlers
/// that mutate configuration: the principal the request was authenticated
/// as (`None` in the open posture, where the transport is trusted exactly
/// as before this layer existed) and the authority whose accepted policy
/// admitted it, so the persist boundary can publish the policy of the
/// configuration it writes.
#[derive(Clone)]
pub struct RequestAuth {
    principal: Option<Arc<ConnectionAuth>>,
    authority: Arc<GatewayInboundAuth>,
    /// The dashboard session that admitted the request, when its cookie
    /// did. Ending the session (sign-out, its hard expiry, eviction, its
    /// provider taken off sign-in) is that credential's revocation, so the
    /// check at the write binds it as it binds the token's.
    session: Option<Arc<SessionAuth>>,
}

/// The extractor a mutating handler declares to receive its
/// [`RequestAuth`]. Absent only when the handler is invoked outside the
/// route group (unit tests call handlers directly); nothing is enforced
/// or published then.
pub type RequestPrincipal = Option<axum::Extension<RequestAuth>>;

/// One config mutation's complete write set: every dotted path the
/// mutation persists, each with the verb its effect requires.
#[derive(Clone, Debug, Default)]
pub struct ConfigWriteSet {
    writes: Vec<(String, Verb)>,
}

impl ConfigWriteSet {
    /// Classify `paths` by effect: `before` is the configuration being
    /// replaced and `after` the mutated working copy. A path declared only
    /// after is a `Create`, one declared only before a `Delete`, anything
    /// else an `Update`. Declaration follows the property tree, so a map
    /// entry (`agents.<alias>`) exists exactly while it has fields, and
    /// creating one implicitly (a `PUT` under a new alias) classifies as
    /// the creation it is.
    pub fn by_effect<'a>(
        before: &Config,
        after: &Config,
        paths: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let before = declared_paths(before);
        let after = declared_paths(after);
        let writes = paths
            .into_iter()
            .map(|path| {
                let verb = match (contains_path(&before, path), contains_path(&after, path)) {
                    (false, true) => Verb::Create,
                    (true, false) => Verb::Delete,
                    _ => Verb::Update,
                };
                (path.to_owned(), verb)
            })
            .collect();
        Self { writes }
    }

    /// Pin the verb for a path whose route semantics the effect diff does
    /// not show: a `DELETE` of a scalar prop clears it rather than removing
    /// the declared field, and a JSON Patch `remove` likewise.
    pub fn with(mut self, path: impl Into<String>, verb: Verb) -> Self {
        let path = path.into();
        self.writes.retain(|(existing, _)| *existing != path);
        self.writes.push((path, verb));
        self
    }

    fn covers(&self, path: &str) -> bool {
        self.writes.iter().any(|(authorized, _)| {
            path == authorized
                || path
                    .strip_prefix(authorized.as_str())
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    }
}

fn declared_paths(config: &Config) -> HashSet<String> {
    config
        .prop_fields()
        .into_iter()
        .map(|info| info.name)
        .collect()
}

fn contains_path(declared: &HashSet<String>, path: &str) -> bool {
    declared.contains(path)
        || declared.iter().any(|name| {
            name.strip_prefix(path)
                .is_some_and(|rest| rest.starts_with('.'))
        })
}

fn verb_name(verb: Verb) -> &'static str {
    match verb {
        Verb::Create => "create",
        Verb::Read => "read",
        Verb::Update => "update",
        Verb::Delete => "delete",
        Verb::Execute => "execute",
    }
}

/// Proof that a mutation's complete write set was authorized before its
/// first side effect. The persist boundary requires one and re-checks that
/// nothing outside the authorized set is about to be written, then
/// publishes the policy of what it wrote through the admitting authority.
pub struct ConfigWriteAuthorization {
    /// `None` when nothing is enforced: the open posture, an admin
    /// principal, or a whole-configuration rewrite already cleared by the
    /// wildcard selector.
    enforced: Option<ConfigWriteSet>,
    authority: Option<Arc<GatewayInboundAuth>>,
}

impl ConfigWriteAuthorization {
    /// Refuse when `dirty`, the paths about to be persisted, reaches
    /// beyond the authorized write set: the handler could not identify its
    /// complete write set up front, and a scoped principal is refused
    /// rather than trusted with the remainder.
    pub fn covers_all<'a>(
        &self,
        dirty: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), WriteDenied> {
        let Some(writes) = &self.enforced else {
            return Ok(());
        };
        for path in dirty {
            if !writes.covers(path) {
                return Err(WriteDenied::forbidden(format!(
                    "Config path `{path}` is outside the write set authorized for this mutation"
                )));
            }
        }
        Ok(())
    }

    /// Publish the policy compiled from the configuration just persisted.
    /// The persist boundary proves the staged policy compiles before it
    /// writes, so a failure here is an invariant violation: it is logged,
    /// and the previously accepted policy stays in force.
    pub fn publish_persisted(&self, persisted: &Config) {
        let Some(authority) = &self.authority else {
            return;
        };
        if let Err(error) = authority.publish_persisted(persisted) {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{error}") })),
                "persisted configuration did not compile into an accepted policy; the previous policy stays in force"
            );
        }
    }
}

fn unenforced(authority: Option<Arc<GatewayInboundAuth>>) -> ConfigWriteAuthorization {
    ConfigWriteAuthorization {
        enforced: None,
        authority,
    }
}

/// The grants the request's principal holds now, under the config write
/// lock, not the ones stamped on it when the route layer admitted it.
///
/// Between admission and the write the handler extracts the body, which the
/// caller paces, and waits for the config write lock. A revocation, an
/// expiry, or an unpairing that lands in that window must bind the write.
/// Every accepted policy is published under the same process-wide lock
/// (the gateway's persist boundary and the RPC context's save both hold
/// it), so no publication can land between this resolution and the save
/// that follows it. `_guard` is the witness that the caller holds it.
fn current_grants(
    request: &RequestAuth,
    conn: &ConnectionAuth,
    _guard: &ConfigWriteGuard,
) -> Result<ResolvedGrants, WriteDenied> {
    if let Some(rejection) = request.session.as_deref().and_then(SessionAuth::ended_now) {
        return Err(WriteDenied::session_ended(rejection));
    }
    request
        .authority
        .inner
        .current_grants(conn)
        .map_err(WriteDenied::authority_lost)
}

/// Require an authenticated administrator under the current config revision.
pub(crate) fn require_config_operator(
    request: &RequestPrincipal,
    guard: &ConfigWriteGuard,
) -> Result<(), WriteDenied> {
    if let Some(axum::Extension(request)) = request
        && let Some(conn) = &request.principal
        && current_grants(request, conn, guard)?.admin
    {
        return Ok(());
    }
    Err(WriteDenied::forbidden(
        "An authenticated operator is required".to_owned(),
    ))
}

/// Authorize one mutation's complete write set for the request's
/// principal, before the mutation's first side effect and under the config
/// write lock. Every path needs both the Config verb its effect requires
/// and a matching config path selector, checked against the principal's
/// current grants. The open posture and an admin principal are
/// unrestricted, as everywhere else.
pub fn authorize_config_write(
    request: &RequestPrincipal,
    writes: ConfigWriteSet,
    guard: &ConfigWriteGuard,
) -> Result<ConfigWriteAuthorization, WriteDenied> {
    let Some(axum::Extension(request)) = request else {
        return Ok(unenforced(None));
    };
    let authority = Some(Arc::clone(&request.authority));
    let Some(conn) = &request.principal else {
        return Ok(unenforced(authority));
    };
    let grants = current_grants(request, conn, guard)?;
    if grants.admin {
        return Ok(unenforced(authority));
    }
    for (path, verb) in &writes.writes {
        if !grants.permits(Resource::Config, *verb) {
            return Err(WriteDenied::forbidden(format!(
                "Principal lacks the config `{}` grant this mutation needs for `{path}`",
                verb_name(*verb)
            )));
        }
        if !grants.may_write_config(path) {
            return Err(WriteDenied::forbidden(format!(
                "Principal's config path selectors do not cover `{path}`"
            )));
        }
    }
    Ok(ConfigWriteAuthorization {
        enforced: Some(writes),
        authority,
    })
}

/// A rewrite whose write set cannot be enumerated before it runs (a schema
/// migration of the file, a Quickstart apply): a scoped principal needs
/// the wildcard selector and every verb the rewrite may exercise. Nothing
/// narrower can be honoured, so nothing narrower is accepted.
pub fn authorize_whole_config_write(
    request: &RequestPrincipal,
    verbs: &[Verb],
    guard: &ConfigWriteGuard,
) -> Result<ConfigWriteAuthorization, WriteDenied> {
    let Some(axum::Extension(request)) = request else {
        return Ok(unenforced(None));
    };
    let authority = Some(Arc::clone(&request.authority));
    let Some(conn) = &request.principal else {
        return Ok(unenforced(authority));
    };
    let grants = current_grants(request, conn, guard)?;
    if grants.admin {
        return Ok(unenforced(authority));
    }
    for verb in verbs {
        if !grants.permits(Resource::Config, *verb) {
            return Err(WriteDenied::forbidden(format!(
                "Principal lacks the config `{}` grant this operation needs",
                verb_name(*verb)
            )));
        }
    }
    if !grants.may_write_config(WILDCARD) {
        return Err(WriteDenied::forbidden(
            "This operation rewrites the configuration as a whole; the principal's config path selectors would need `*`"
                .to_owned(),
        ));
    }
    Ok(unenforced(authority))
}

/// Route-layer middleware for the config/onboarding route group.
pub async fn config_route_auth(
    State(route): State<ConfigRouteAuth>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth = Arc::clone(&route.inbound);
    // CORS preflight carries no Authorization header; the per-handler
    // convention this replaces never authenticated OPTIONS either (the
    // explicit `handle_options_*` handlers carry no check).
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }

    // A signed-in dashboard session carries no bearer: the session layer
    // already verified it and resolved the grants in force now, and put
    // its verdict on the request. An explicit credential always wins, so
    // this applies only when the request presents none.
    if !request
        .headers()
        .contains_key(axum::http::header::AUTHORIZATION)
        && !request.headers().contains_key(AUTH_PROVIDER_HEADER)
        && let Some(caller) = request.extensions().get::<DashboardCaller>().cloned()
    {
        let session = match caller {
            DashboardCaller::Session(session) => session,
            DashboardCaller::Rejected(rejection) => {
                return crate::dashboard_session::rejection_response(rejection);
            }
        };
        let Some(floor) = method_floor(request.method()) else {
            return forbidden("Unsupported method for the config surface");
        };
        if !clears_floor(floor, &session.grants) {
            return forbidden("Principal lacks the config grant required for this method");
        }
        let mut conn = session.connection.clone();
        conn.grants = session.grants.clone();
        request.extensions_mut().insert(RequestAuth {
            principal: Some(Arc::new(conn)),
            authority: Arc::clone(&auth),
            session: Some(session),
        });
        return next.run(request).await;
    }

    // A provider header that is present but blank or not text is a
    // malformed selection, not an absent one: it never falls through to
    // the native provider or the open posture.
    let provider = match request.headers().get(AUTH_PROVIDER_HEADER) {
        None => None,
        Some(value) => match value.to_str().ok().map(str::trim).filter(|s| !s.is_empty()) {
            Some(name) => Some(name.to_owned()),
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "Invalid auth_provider selection" })),
                )
                    .into_response();
            }
        },
    };

    // Open posture preserved: with pairing disabled and no explicit
    // provider selection, the transport is trusted exactly as before
    // this layer existed.
    if provider.is_none() && !auth.pairing().require_pairing() {
        request.extensions_mut().insert(RequestAuth {
            principal: None,
            authority: Arc::clone(&auth),
            session: None,
        });
        return next.run(request).await;
    }

    let Some(token) = crate::api::extract_bearer_token(request.headers()) else {
        return unauthorized();
    };
    if token.is_empty() {
        return unauthorized();
    }
    let token = token.to_owned();

    let conn = match provider {
        Some(provider) => {
            // Both limits apply before anything reaches the provider. The
            // ledger refusal is free; the permit is held for the
            // verification alone and released before the handler runs, so
            // a slow handler never keeps a provider slot.
            let key = bearer_client_key(&route, &request);
            if let Err(e) = route.limits.attempts.check_rate_limit(&key) {
                return too_many_attempts(e.retry_after_secs);
            }
            let verified = {
                let Ok(_permit) = route.limits.outbound.try_acquire() else {
                    return verification_busy();
                };
                auth.authenticate_scoped(&token, &provider).await
            };
            match verified {
                Ok(conn) => conn,
                Err(denied) => {
                    // A refused credential is booked. A `FORBIDDEN` denial
                    // is not: it is a verified identity the policy grants
                    // nothing, or the provider unreachable, throttling or
                    // misconfigured, none of which the caller controls.
                    if denied.code == AUTH_REQUIRED {
                        route.limits.attempts.record_attempt(&key);
                    }
                    return denied_response(&denied);
                }
            }
        }
        None => match auth.authenticate_native(&token).await {
            Ok(conn) => conn,
            // Preserve the historical native denial shape verbatim.
            Err(_) => return unauthorized(),
        },
    };

    let Some(floor) = method_floor(request.method()) else {
        return forbidden("Unsupported method for the config surface");
    };
    if !clears_floor(floor, &conn.grants) {
        return forbidden("Principal lacks the config grant required for this method");
    }

    request.extensions_mut().insert(RequestAuth {
        principal: Some(Arc::new(conn)),
        authority: Arc::clone(&auth),
        session: None,
    });
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt as _;
    use std::collections::HashMap;
    use tower::ServiceExt as _;
    use wiremock::matchers::{method as http_method, path as http_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::schema::{OidcConfig, OidcValidation, PermissionProfileConfig};

    use crate::AppState;

    const LEGACY_DENIAL: &str =
        "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>";

    fn paired_config() -> Config {
        let mut config = Config::default();
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec!["zc_paired".into()];
        config
    }

    /// The REAL route group under test, built exactly as `run_gateway`
    /// builds it (same constructor, same layer).
    fn router_for(config: Config) -> Router {
        router_and_authority_for(config).0
    }

    fn router_and_authority_for(config: Config) -> (Router, Arc<GatewayInboundAuth>) {
        let (router, auth, _) = router_authority_and_limits_for(config, false);
        (router, auth)
    }

    /// The route group with its bearer limits exposed, so a test can fill
    /// the outbound cap or read the ledger, and with the forwarded-header
    /// trust the gateway would take from `[gateway] trust_forwarded_headers`.
    fn router_authority_and_limits_for(
        mut config: Config,
        trust_forwarded_headers: bool,
    ) -> (
        Router,
        Arc<GatewayInboundAuth>,
        Arc<BearerVerificationLimits>,
    ) {
        config.gateway.trust_forwarded_headers = trust_forwarded_headers;
        let state = AppState {
            pairing: Arc::new(PairingGuard::new(
                config.gateway.require_pairing,
                &config.gateway.paired_tokens,
                zeroclaw_config::pairing::PairingCodePolicy::default(),
            )),
            trust_forwarded_headers,
            ..crate::api::tests::test_state(config.clone())
        };
        let auth = Arc::new(
            GatewayInboundAuth::from_config(&config, Arc::clone(&state.pairing))
                .expect("inbound auth builds from a valid config"),
        );
        let route_auth = ConfigRouteAuth::new(&auth, &state.config);
        let limits = Arc::clone(&route_auth.limits);
        (
            crate::config_admin_router(route_auth).with_state(state),
            auth,
            limits,
        )
    }

    /// The route group wired the way the daemon wires a supervised gateway:
    /// its authority is the RPC context's (`from_shared`) and its
    /// configuration state is the RPC context's live configuration. Returns
    /// the router with the two RPC-side handles a test drives directly.
    fn router_sharing_rpc_authority(
        config: Config,
    ) -> (
        Router,
        Arc<RpcInboundAuth>,
        zeroclaw_runtime::LiveConfigAuthority,
    ) {
        let pairing = Arc::new(PairingGuard::new(
            config.gateway.require_pairing,
            &config.gateway.paired_tokens,
            zeroclaw_config::pairing::PairingCodePolicy::default(),
        ));
        let rpc_auth = Arc::new(
            RpcInboundAuth::from_config(&config, Arc::clone(&pairing))
                .expect("inbound auth builds from a valid config"),
        );
        let state = AppState {
            pairing,
            ..crate::api::tests::test_state(config)
        };
        let authority = state.config_authority.clone();
        let auth = Arc::new(GatewayInboundAuth::from_shared(Arc::clone(&rpc_auth)));
        (
            crate::config_admin_router(ConfigRouteAuth::new(&auth, &state.config))
                .with_state(state),
            rpc_auth,
            authority,
        )
    }

    async fn send(
        router: &Router,
        http_method: &str,
        path: &str,
        bearer: Option<&str>,
        provider: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let (status, _, json) =
            send_from(router, None, &[], http_method, path, bearer, provider, body).await;
        (status, json)
    }

    /// [`send`] from a given peer (none = no `ConnectInfo`, as a request
    /// built outside the gateway's serving has) with extra headers,
    /// returning the response headers too.
    async fn send_from(
        router: &Router,
        peer: Option<SocketAddr>,
        headers: &[(&str, &str)],
        http_method: &str,
        path: &str,
        bearer: Option<&str>,
        provider: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let mut builder = HttpRequest::builder().method(http_method).uri(path);
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        if let Some(provider) = provider {
            builder = builder.header(AUTH_PROVIDER_HEADER, provider);
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut request = match body {
            Some(json) => builder
                .header("content-type", "application/json")
                .body(Body::from(json.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        if let Some(peer) = peer {
            request.extensions_mut().insert(ConnectInfo(peer));
        }
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, headers, json)
    }

    #[tokio::test]
    async fn unauthenticated_requests_get_the_legacy_denial_shape() {
        let router = router_for(paired_config());
        for path in [
            "/api/config",
            "/api/quickstart/state",
            "/api/config/sections",
        ] {
            let (status, body) = send(&router, "GET", path, None, None, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(body["error"], LEGACY_DENIAL, "{path}");
        }
    }

    #[tokio::test]
    async fn invalid_native_bearer_keeps_the_legacy_denial_shape() {
        let router = router_for(paired_config());
        let (status, body) =
            send(&router, "GET", "/api/config", Some("zc_wrong"), None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], LEGACY_DENIAL);
    }

    #[tokio::test]
    async fn paired_native_bearer_reaches_the_handler_with_full_access() {
        let router = router_for(paired_config());
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("zc_paired"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = send(
            &router,
            "POST",
            "/api/quickstart/fields",
            Some("zc_paired"),
            None,
            Some(serde_json::json!({"section": "channel", "type_key": "telegram"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "shared operator may mutate");
    }

    #[tokio::test]
    async fn options_preflight_stays_unauthenticated() {
        let router = router_for(paired_config());
        // A CORS preflight without credentials gets the intended answer:
        // 204 with the allow list, not a denial.
        let request = HttpRequest::builder()
            .method("OPTIONS")
            .uri("/api/config")
            .header("access-control-request-method", "PATCH")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-methods")
                .and_then(|v| v.to_str().ok()),
            Some("GET, PUT, PATCH, OPTIONS")
        );
        // A bare OPTIONS serves the schema document, also unauthenticated.
        let (status, _) = send(&router, "OPTIONS", "/api/config", None, None, None).await;
        assert_eq!(status, StatusCode::OK);
    }

    fn open_config() -> Config {
        let mut config = Config::default();
        config.gateway.require_pairing = false;
        config
    }

    #[tokio::test]
    async fn open_mode_without_provider_selection_stays_open() {
        let router = router_for(open_config());
        let (status, _) = send(&router, "GET", "/api/quickstart/state", None, None, None).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_provider_selection_is_denied_even_in_open_mode() {
        let router = router_for(open_config());
        let (status, body) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("whatever"),
            Some("oidc.nope"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "Unknown auth_provider selection");
    }

    #[tokio::test]
    async fn a_present_but_blank_or_non_text_provider_header_is_refused() {
        // Open posture, so an "absent" header would pass: a blank one must
        // not be read as absent.
        let router = router_for(open_config());
        let (status, body) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("whatever"),
            Some("   "),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "Invalid auth_provider selection");

        let request = HttpRequest::builder()
            .method("GET")
            .uri("/api/quickstart/state")
            .header("authorization", "Bearer whatever")
            .header(
                AUTH_PROVIDER_HEADER,
                axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
            )
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    fn now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    async fn introspection_idp(groups: &[&str]) -> MockServer {
        let server = MockServer::start().await;
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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true,
                "token_type": "Bearer",
                "client_id": "gw",
                "iss": issuer,
                "sub": "alice",
                "aud": "zeroclaw",
                "exp": now_unix() + 600,
                "groups": groups,
            })))
            .mount(&server)
            .await;
        server
    }

    fn oidc_config_with_reader_profile(issuer: &str) -> Config {
        let mut config = paired_config();
        config.oidc.insert(
            "test".into(),
            OidcConfig {
                issuer: issuer.to_string(),
                audience: "zeroclaw".into(),
                client_id: "gw".into(),
                client_secret: Some("s3cret".into()),
                validation: OidcValidation::Introspection,
                claim_path: "groups".into(),
                profile_map: HashMap::from([("ops".to_string(), "config-reader".to_string())]),
                // The provider classifies the actor from an operator
                // declaration; without one a verified token has no declared
                // kind and is refused. This fixture's client is interactive.
                interactive_clients: vec!["gw".into()],
                ..OidcConfig::default()
            },
        );
        config.permission_profiles.insert(
            "config-reader".into(),
            PermissionProfileConfig {
                grants: HashMap::from([(Resource::Config, vec![Verb::Read])]),
                ..PermissionProfileConfig::default()
            },
        );
        config
    }

    #[tokio::test]
    async fn scoped_oidc_principal_is_gated_by_config_grants() {
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(oidc_config_with_reader_profile(&idp.uri()));

        // Read passes: the profile grants Config:Read.
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("opaque-token"),
            Some("oidc.test"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Mutation is refused at the layer: no Config:Update grant.
        let (status, body) = send(
            &router,
            "POST",
            "/api/quickstart/fields",
            Some("opaque-token"),
            Some("oidc.test"),
            Some(serde_json::json!({"section": "channel", "type_key": "telegram"})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            body["error"],
            "Principal lacks the config grant required for this method"
        );
    }

    #[tokio::test]
    async fn oidc_identity_with_no_mapped_profile_is_refused() {
        let idp = introspection_idp(&["unmapped-group"]).await;
        let router = router_for(oidc_config_with_reader_profile(&idp.uri()));
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            Some("opaque-token"),
            Some("oidc.test"),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "deny-by-default: no profile, no access"
        );
    }

    // ── Write sets: verbs by effect, selectors over the complete mutation ──

    const SCOPED: (Option<&str>, Option<&str>) = (Some("opaque-token"), Some("oidc.test"));
    const OPERATOR: (Option<&str>, Option<&str>) = (Some("zc_paired"), None);

    #[tokio::test]
    async fn plugin_tls_credential_api_is_operator_only_and_write_only() {
        use zeroclaw_config::schema::{PluginEntryConfig, PluginTlsProfileConfig};

        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let mut config = editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Read, Verb::Create, Verb::Update, Verb::Delete],
            &["*"],
        );
        let instance = "zpi1_test";
        let property = "ca.pem";
        config.secrets.encrypt = true;
        config.plugins.entries.push(PluginEntryConfig {
            name: instance.into(),
            egress_hosts: vec!["example.com".into()],
            tls_profiles: vec![PluginTlsProfileConfig {
                name: "custom".into(),
                hosts: vec!["example.com".into()],
                system_roots: true,
                custom_ca_secret: Some(property.into()),
                client_certificate_secret: None,
                client_private_key_secret: None,
            }],
            ..PluginEntryConfig::default()
        });
        let router = router_for(config);
        let url = "/api/config/plugin-tls-credentials";
        let create = serde_json::json!({
            "instance": instance, "property": property,
            "operation": "create", "value": "synthetic-credential"
        });
        assert_eq!(
            send(&router, "GET", url, SCOPED.0, SCOPED.1, None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(
                &router,
                "POST",
                url,
                SCOPED.0,
                SCOPED.1,
                Some(create.clone())
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(
                &router,
                "POST",
                url,
                OPERATOR.0,
                OPERATOR.1,
                Some(create.clone())
            )
            .await
            .0,
            StatusCode::PRECONDITION_FAILED
        );
        zeroclaw_config::secrets::SecretStore::new(tmp.path(), true)
            .encrypt("initialize-fixture-key")
            .unwrap();
        let (status, body) = send(&router, "GET", url, OPERATOR.0, OPERATOR.1, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["credentials"][0]["configured"], false);
        let (status, body) = send(
            &router,
            "POST",
            url,
            OPERATOR.0,
            OPERATOR.1,
            Some(create.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"configured": true}));
        let disk = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
        assert!(disk.contains("enc2:"));
        assert!(!disk.contains("synthetic-credential"));
        assert_eq!(
            send(&router, "POST", url, OPERATOR.0, OPERATOR.1, Some(create))
                .await
                .0,
            StatusCode::CONFLICT
        );
        let (status, body) = send(&router, "GET", url, OPERATOR.0, OPERATOR.1, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["credentials"][0]["configured"], true);
        assert!(!body.to_string().contains("synthetic-credential"));
        let replace = serde_json::json!({
            "instance": instance, "property": property,
            "operation": "replace", "value": "rotated-synthetic-credential"
        });
        let (status, body) =
            send(&router, "POST", url, OPERATOR.0, OPERATOR.1, Some(replace)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"configured": true}));
        let disk = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
        assert!(!disk.contains("rotated-synthetic-credential"));
        let delete =
            serde_json::json!({"instance": instance, "property": property, "operation": "delete"});
        let (status, body) = send(&router, "POST", url, OPERATOR.0, OPERATOR.1, Some(delete)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"configured": false}));
        let disk = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
        let config: toml::Value = toml::from_str(&disk).unwrap();
        let entry = &config["plugins"]["entries"][0];
        assert!(
            entry
                .get("config")
                .and_then(|map| map.get(property))
                .is_none()
        );
    }

    /// The mapped profile with the given Config verbs and path selectors,
    /// on a configuration that persists into `tmp` rather than the home
    /// directory.
    fn editor_config(
        tmp: &tempfile::TempDir,
        issuer: &str,
        verbs: &[Verb],
        paths: &[&str],
    ) -> Config {
        let mut config = oidc_config_with_reader_profile(issuer);
        config.permission_profiles.insert(
            "config-reader".into(),
            PermissionProfileConfig {
                grants: HashMap::from([(Resource::Config, verbs.to_vec())]),
                config_write_paths: paths.iter().map(|p| (*p).to_string()).collect(),
                ..PermissionProfileConfig::default()
            },
        );
        config.config_path = tmp.path().join("config.toml");
        config.data_dir = tmp.path().join("data");
        config
    }

    async fn create_agent(router: &Router, alias: &str) {
        let (status, body) = send(
            router,
            "POST",
            &format!("/api/config/map-key?path=agents&key={alias}"),
            OPERATOR.0,
            OPERATOR.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "fixture agent `{alias}`: {body}");
        assert_eq!(body["created"], true, "fixture agent `{alias}`: {body}");
    }

    async fn agent_model(router: &Router, alias: &str) -> (StatusCode, serde_json::Value) {
        let (status, body) = send(
            router,
            "GET",
            &format!("/api/config/prop?path=agents.{alias}.workspace.path"),
            OPERATOR.0,
            OPERATOR.1,
            None,
        )
        .await;
        (status, body["value"].clone())
    }

    fn put_model(alias: &str, model: &str) -> serde_json::Value {
        serde_json::json!({ "path": format!("agents.{alias}.workspace.path"), "value": model })
    }

    #[tokio::test]
    async fn verbs_follow_the_effect_of_the_mutation_not_the_http_method() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Update],
            &["agents.*"],
        ));
        create_agent(&router, "alpha").await;

        // Updating an existing path is what the grant covers.
        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            SCOPED.0,
            SCOPED.1,
            Some(put_model("alpha", "m2")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(agent_model(&router, "alpha").await.1, "m2");

        // A map-key creation is a POST, but the operation is a `create`.
        let (status, body) = send(
            &router,
            "POST",
            "/api/config/map-key?path=agents&key=beta",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["error"].as_str().unwrap().contains("`create`"),
            "{body}"
        );
        assert_eq!(agent_model(&router, "beta").await.0, StatusCode::NOT_FOUND);

        // So is a PUT that would bring a new map key into being.
        let (status, _) = send(
            &router,
            "PUT",
            "/api/config/prop",
            SCOPED.0,
            SCOPED.1,
            Some(put_model("gamma", "m1")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(agent_model(&router, "gamma").await.0, StatusCode::NOT_FOUND);

        // Removing the entry is a `delete`, whatever the method.
        let (status, body) = send(
            &router,
            "DELETE",
            "/api/config/map-key?path=agents&key=alpha",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["error"].as_str().unwrap().contains("`delete`"),
            "{body}"
        );
        assert_eq!(agent_model(&router, "alpha").await.1, "m2");

        // A JSON Patch `remove` is a delete too, inside a PATCH.
        let (status, body) = send(
            &router,
            "PATCH",
            "/api/config",
            SCOPED.0,
            SCOPED.1,
            Some(serde_json::json!([{ "op": "remove", "path": "/agents/alpha/workspace/path" }])),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["error"].as_str().unwrap().contains("`delete`"),
            "{body}"
        );
        assert_eq!(agent_model(&router, "alpha").await.1, "m2");
    }

    #[tokio::test]
    async fn explicit_create_and_delete_grants_admit_those_operations() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Create, Verb::Update, Verb::Delete],
            &["agents.*"],
        ));

        let (status, body) = send(
            &router,
            "POST",
            "/api/config/map-key?path=agents&key=beta",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["created"], true);

        let (status, body) = send(
            &router,
            "PATCH",
            "/api/config",
            SCOPED.0,
            SCOPED.1,
            Some(serde_json::json!([{ "op": "remove", "path": "/agents/beta/workspace/path" }])),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = send(
            &router,
            "DELETE",
            "/api/config/map-key?path=agents&key=beta",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(agent_model(&router, "beta").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn path_selectors_are_enforced_on_the_complete_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Create, Verb::Update, Verb::Delete],
            &["agents.alpha.*"],
        ));
        create_agent(&router, "alpha").await;
        create_agent(&router, "beta").await;

        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            SCOPED.0,
            SCOPED.1,
            Some(put_model("alpha", "m2")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            SCOPED.0,
            SCOPED.1,
            Some(put_model("beta", "m2")),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("do not cover `agents.beta.workspace.path`"),
            "{body}"
        );

        // One out-of-scope member refuses the whole batch: the in-scope
        // member does not land either.
        let (status, _) = send(
            &router,
            "PATCH",
            "/api/config",
            SCOPED.0,
            SCOPED.1,
            Some(serde_json::json!([
                { "op": "replace", "path": "/agents/alpha/workspace/path", "value": "m3" },
                { "op": "replace", "path": "/agents/beta/workspace/path", "value": "m3" },
            ])),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(agent_model(&router, "alpha").await.1, "m2");

        // A rename writes its destination as well as its source.
        let (status, _) = send(
            &router,
            "POST",
            "/api/config/rename-map-key",
            SCOPED.0,
            SCOPED.1,
            Some(serde_json::json!({ "path": "agents", "from": "alpha", "to": "gamma" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(agent_model(&router, "alpha").await.0, StatusCode::OK);
        assert_eq!(agent_model(&router, "gamma").await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn whole_configuration_rewrites_need_the_wildcard_selector() {
        let idp = introspection_idp(&["ops"]).await;
        let verbs = [Verb::Create, Verb::Update, Verb::Delete];

        let tmp = tempfile::tempdir().unwrap();
        let router = router_for(editor_config(&tmp, &idp.uri(), &verbs, &["agents.*"]));
        create_agent(&router, "alpha").await;
        let (status, body) = send(
            &router,
            "POST",
            "/api/config/migrate",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().unwrap().contains("`*`"), "{body}");

        let tmp = tempfile::tempdir().unwrap();
        let router = router_for(editor_config(&tmp, &idp.uri(), &verbs, &["*"]));
        create_agent(&router, "alpha").await;
        let (status, body) = send(
            &router,
            "POST",
            "/api/config/migrate",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[tokio::test]
    async fn methods_outside_the_known_set_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Read, Verb::Create, Verb::Update, Verb::Delete],
            &["*"],
        ));
        let (status, body) = send(&router, "TRACE", "/api/config", SCOPED.0, SCOPED.1, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "Unsupported method for the config surface");
    }

    // ── Policy moves only at the persist boundary ────────────────────────

    #[tokio::test]
    async fn policy_is_bound_to_the_accepted_revision_not_to_the_request() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let (router, authority) =
            router_and_authority_for(editor_config(&tmp, &idp.uri(), &[Verb::Read], &[]));
        let generation = authority.generation();

        // C0 admits the principal through its `groups` claim.
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // C1, persisted by the operator, tightens the provider: the claim
        // the profile map reads no longer exists in the token.
        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            OPERATOR.0,
            OPERATOR.1,
            Some(serde_json::json!({ "path": "oidc.test.claim_path", "value": "roles" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            authority.generation(),
            generation + 1,
            "the persist published exactly one new accepted revision"
        );

        // The same bearer is now refused against the accepted C1 policy,
        // with no request having recompiled anything.
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // A later persist that touches no authorization input publishes no
        // new generation, and crucially does not roll C1's tightening back.
        create_agent(&router, "alpha").await;
        assert_eq!(
            authority.generation(),
            generation + 1,
            "an unrelated persist republishes without moving the generation"
        );
        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// A revocation persisted through RPC binds the gateway's next request
    /// with no reload: the supervised gateway authenticates against the RPC
    /// context's accepted policy, not a second copy compiled when it started.
    #[tokio::test]
    async fn rpc_revocation_binds_the_gateway_without_a_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let (router, rpc_auth, live_config) =
            router_sharing_rpc_authority(editor_config(&tmp, &idp.uri(), &[Verb::Read], &[]));

        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // The RPC context's save, in the order `save_and_publish_config` runs
        // it: publish the live configuration, then publish the policy compiled
        // from it as the next accepted revision. The mapped profile keeps a
        // grant but loses Config.
        let commit = live_config.begin_config_commit().await.unwrap();
        let mut revoked = commit.current_config();
        revoked.permission_profiles.insert(
            "config-reader".into(),
            PermissionProfileConfig {
                grants: HashMap::from([(Resource::Sessions, vec![Verb::Read])]),
                ..PermissionProfileConfig::default()
            },
        );
        let config_revision = commit.next_revision().unwrap();
        commit.publish(config_revision, revoked.clone()).unwrap();
        let revision = rpc_auth.accepted_revision().saturating_add(1);
        rpc_auth
            .publish_accepted(&revoked, revision)
            .expect("the revoked policy compiles");
        drop(commit);

        let (status, _) = send(
            &router,
            "GET",
            "/api/quickstart/state",
            SCOPED.0,
            SCOPED.1,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "the gateway must enforce a revocation persisted through RPC"
        );
    }

    /// The reverse direction: a gateway persist publishes into the RPC
    /// context's own authority, so established RPC connections see the new
    /// generation at their next privileged operation, with no reload.
    #[tokio::test]
    async fn gateway_persist_publishes_into_the_rpc_authority() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let (router, rpc_auth, _live_config) =
            router_sharing_rpc_authority(editor_config(&tmp, &idp.uri(), &[Verb::Read], &[]));
        let generation = rpc_auth.generation();

        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            OPERATOR.0,
            OPERATOR.1,
            Some(serde_json::json!({ "path": "oidc.test.claim_path", "value": "roles" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            rpc_auth.generation(),
            generation + 1,
            "the gateway persist published into the RPC context's authority"
        );
    }

    // ── Authority is rechecked at the write, not only at admission ──

    /// A config write whose body the caller withholds after the route layer
    /// admitted it. The route layer reads only headers, so the handler's
    /// first poll of the body is strictly after admission: `admitted` fires
    /// there, and the body is delivered only once `release` is sent.
    /// Returns the in-flight request with its two ends.
    fn parked_write(
        router: &Router,
        bearer: &str,
        provider: Option<&str>,
        body: serde_json::Value,
    ) -> (
        tokio::task::JoinHandle<StatusCode>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (admitted_tx, admitted_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let stream = futures_util::stream::once(async move {
            let _ = admitted_tx.send(());
            let _ = release_rx.await;
            Ok::<_, std::convert::Infallible>(body.to_string())
        });
        let mut builder = HttpRequest::builder()
            .method("PUT")
            .uri("/api/config/prop")
            .header("authorization", format!("Bearer {bearer}"))
            .header("content-type", "application/json");
        if let Some(provider) = provider {
            builder = builder.header(AUTH_PROVIDER_HEADER, provider);
        }
        let request = builder.body(Body::from_stream(stream)).unwrap();
        let router = router.clone();
        let write =
            ::zeroclaw_spawn::spawn!(
                async move { router.oneshot(request).await.unwrap().status() }
            );
        (write, admitted_rx, release_tx)
    }

    /// The control for the two tests below: parking the body on its own
    /// changes nothing, so a refusal there is the recheck's doing.
    #[tokio::test]
    async fn a_parked_write_with_its_authority_intact_lands() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Read, Verb::Update],
            &["agents.*"],
        ));
        create_agent(&router, "alpha").await;

        let (write, admitted, release) = parked_write(
            &router,
            "opaque-token",
            Some("oidc.test"),
            put_model("alpha", "parked"),
        );
        admitted
            .await
            .expect("the handler polled the body after admission");
        release.send(()).unwrap();

        assert_eq!(write.await.unwrap(), StatusCode::OK);
        assert_eq!(agent_model(&router, "alpha").await.1, "parked");
    }

    /// An operator revocation that lands while an admitted scoped write is
    /// still receiving its body binds that write: the grants stamped at
    /// admission are not the authority it persists under.
    #[tokio::test]
    async fn a_revocation_after_admission_binds_the_in_flight_write() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp(&["ops"]).await;
        let router = router_for(editor_config(
            &tmp,
            &idp.uri(),
            &[Verb::Read, Verb::Update],
            &["agents.*"],
        ));
        create_agent(&router, "alpha").await;
        let (_, before) = agent_model(&router, "alpha").await;

        let (write, admitted, release) = parked_write(
            &router,
            "opaque-token",
            Some("oidc.test"),
            put_model("alpha", "after-revocation"),
        );
        admitted
            .await
            .expect("the handler polled the body after admission");

        // The operator revokes through the gateway and gets success: the
        // claim the profile map reads no longer exists in the token.
        let (status, body) = send(
            &router,
            "PUT",
            "/api/config/prop",
            OPERATOR.0,
            OPERATOR.1,
            Some(serde_json::json!({ "path": "oidc.test.claim_path", "value": "roles" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        release.send(()).unwrap();
        assert_eq!(write.await.unwrap(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            agent_model(&router, "alpha").await.1,
            before,
            "the revoked write must not reach the configuration"
        );
    }

    /// The same window for a native token unpaired after admission: pairing
    /// liveness is checked at the write, not only when the request arrived.
    #[tokio::test]
    async fn a_native_token_unpaired_after_admission_cannot_write() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = paired_config();
        config.gateway.paired_tokens.push("zc_second".into());
        config.config_path = tmp.path().join("config.toml");
        config.data_dir = tmp.path().join("data");
        let (router, authority) = router_and_authority_for(config);
        create_agent(&router, "alpha").await;
        let (_, before) = agent_model(&router, "alpha").await;

        let (write, admitted, release) =
            parked_write(&router, "zc_second", None, put_model("alpha", "unpaired"));
        admitted
            .await
            .expect("the handler polled the body after admission");

        assert!(authority.pairing().revoke_token("zc_second"));
        release.send(()).unwrap();

        assert_eq!(write.await.unwrap(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            agent_model(&router, "alpha").await.1,
            before,
            "the unpaired write must not reach the configuration"
        );
    }

    // ── Bearer verification limits ──

    fn remote() -> SocketAddr {
        "203.0.113.7:51000".parse().unwrap()
    }

    fn loopback() -> SocketAddr {
        "127.0.0.1:51000".parse().unwrap()
    }

    /// An identity provider whose introspection endpoint answers with
    /// `template`, so a test can make every verification fail the way it
    /// needs: an inactive token, or an outage on the gateway's own leg.
    async fn introspection_idp_answering(template: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
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
            .respond_with(template)
            .mount(&server)
            .await;
        server
    }

    /// How many times the provider was asked to introspect a token.
    async fn introspections(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/introspect")
            .count()
    }

    async fn scoped_read_from(
        router: &Router,
        peer: SocketAddr,
    ) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        send_from(
            router,
            Some(peer),
            &[],
            "GET",
            "/api/quickstart/state",
            Some("opaque-token"),
            Some("oidc.test"),
            None,
        )
        .await
    }

    #[tokio::test]
    async fn ten_failed_bearers_lock_the_client_out() {
        let idp = introspection_idp_answering(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "active": false })),
        )
        .await;
        let (router, _, limits) =
            router_authority_and_limits_for(oidc_config_with_reader_profile(&idp.uri()), false);

        for attempt in 1..=crate::auth_rate_limit::MAX_ATTEMPTS {
            let (status, _, _) = scoped_read_from(&router, remote()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "attempt {attempt}");
        }
        assert_eq!(
            introspections(&idp).await,
            10,
            "each refusal cost one round trip"
        );

        let (status, headers, body) = scoped_read_from(&router, remote()).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["retry_after"], crate::auth_rate_limit::LOCKOUT_SECS);
        assert_eq!(headers["retry-after"], "300");
        assert_eq!(
            body["error"],
            "Too many failed bearer verifications. Try again in 300s."
        );
        assert_eq!(
            introspections(&idp).await,
            10,
            "a lockout costs the provider nothing"
        );
        assert!(limits.attempts.is_locked_out("203.0.113.7"));
    }

    #[tokio::test]
    async fn a_valid_bearer_is_never_booked() {
        let idp = introspection_idp(&["ops"]).await;
        let (router, _, limits) =
            router_authority_and_limits_for(oidc_config_with_reader_profile(&idp.uri()), false);
        for _ in 0..30 {
            let (status, _, _) = scoped_read_from(&router, remote()).await;
            assert_eq!(status, StatusCode::OK);
        }
        assert!(!limits.attempts.is_locked_out("203.0.113.7"));
    }

    #[tokio::test]
    async fn an_idp_outage_is_not_billed_to_the_caller() {
        let idp = introspection_idp_answering(ResponseTemplate::new(503)).await;
        let (router, _, limits) =
            router_authority_and_limits_for(oidc_config_with_reader_profile(&idp.uri()), false);
        for _ in 0..15 {
            let (status, _, _) = scoped_read_from(&router, remote()).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "an unreachable authority fails closed, as before"
            );
        }
        assert!(
            !limits.attempts.is_locked_out("203.0.113.7"),
            "the gateway's own leg to the provider says nothing about the caller"
        );
    }

    #[tokio::test]
    async fn a_verified_but_ungranted_identity_is_not_booked() {
        let idp = introspection_idp(&["unmapped-group"]).await;
        let (router, _, limits) =
            router_authority_and_limits_for(oidc_config_with_reader_profile(&idp.uri()), false);
        for _ in 0..15 {
            let (status, _, _) = scoped_read_from(&router, remote()).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        assert!(!limits.attempts.is_locked_out("203.0.113.7"));
    }

    #[tokio::test]
    async fn the_outbound_cap_refuses_the_seventeenth_verification() {
        let idp = introspection_idp_answering(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "active": false }))
                .set_delay(std::time::Duration::from_secs(2)),
        )
        .await;
        let (router, _, _) =
            router_authority_and_limits_for(oidc_config_with_reader_profile(&idp.uri()), false);

        // Seventeen verifications from seventeen addresses, all in flight
        // while the provider takes its time: the per-client ledger sees one
        // attempt each, so only the cap can refuse.
        let mut in_flight = Vec::new();
        for n in 0..=OUTBOUND_BEARER_CAP {
            let router = router.clone();
            let peer: SocketAddr = format!("198.51.100.{}:4000", n + 1).parse().unwrap();
            in_flight.push(zeroclaw_spawn::spawn!(async move {
                scoped_read_from(&router, peer).await
            }));
        }
        let mut refused = 0;
        let mut verified = 0;
        for task in in_flight {
            let (status, headers, body) = task.await.unwrap();
            match status {
                StatusCode::SERVICE_UNAVAILABLE => {
                    refused += 1;
                    assert_eq!(headers["retry-after"], "5");
                    assert_eq!(body["error"], "bearer verification is busy; retry shortly");
                }
                StatusCode::UNAUTHORIZED => verified += 1,
                other => panic!("unexpected status {other}"),
            }
        }
        assert_eq!(refused, 1);
        assert_eq!(verified, OUTBOUND_BEARER_CAP);
        assert_eq!(introspections(&idp).await, OUTBOUND_BEARER_CAP);

        // The slots are free again once those verifications have ended.
        let (status, _, _) = scoped_read_from(&router, remote()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(introspections(&idp).await, OUTBOUND_BEARER_CAP + 1);
    }

    #[tokio::test]
    async fn the_permit_is_held_only_for_the_verification() {
        let idp = introspection_idp(&["ops"]).await;
        let config = oidc_config_with_reader_profile(&idp.uri());
        let (_, auth, limits) = router_authority_and_limits_for(config.clone(), false);
        let config = crate::api::tests::test_state(config).config;
        // Park each request after verification until explicitly released:
        // a permit held across the handler would leave none for request 17.
        let route_auth = ConfigRouteAuth {
            inbound: auth,
            limits: Arc::clone(&limits),
            config,
        };
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::channel(OUTBOUND_BEARER_CAP + 1);
        let (release_tx, release_rx) = tokio::sync::watch::channel(false);
        let router = Router::new()
            .route(
                "/slow",
                axum::routing::get(move || {
                    let entered = entered_tx.clone();
                    let mut release = release_rx.clone();
                    async move {
                        entered.send(()).await.expect("test awaits handler entry");
                        release
                            .wait_for(|released| *released)
                            .await
                            .expect("test releases handlers");
                        StatusCode::OK
                    }
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                route_auth,
                config_route_auth,
            ));

        let mut parked = Vec::new();
        for n in 0..OUTBOUND_BEARER_CAP {
            let router = router.clone();
            let peer: SocketAddr = format!("198.51.100.{}:4000", n + 1).parse().unwrap();
            parked.push(zeroclaw_spawn::spawn!(async move {
                send_from(
                    &router,
                    Some(peer),
                    &[],
                    "GET",
                    "/slow",
                    Some("opaque-token"),
                    Some("oidc.test"),
                    None,
                )
                .await
                .0
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            for _ in 0..OUTBOUND_BEARER_CAP {
                entered_rx.recv().await.expect("handler signals entry");
            }
        })
        .await
        .expect("all sixteen requests enter their handlers");
        assert_eq!(
            limits.outbound.available_permits(),
            OUTBOUND_BEARER_CAP,
            "every permit was released before its handler ran"
        );
        parked.push(zeroclaw_spawn::spawn!(async move {
            send_from(
                &router,
                Some(remote()),
                &[],
                "GET",
                "/slow",
                Some("opaque-token"),
                Some("oidc.test"),
                None,
            )
            .await
            .0
        }));
        tokio::time::timeout(std::time::Duration::from_secs(10), entered_rx.recv())
            .await
            .expect("the seventeenth request also enters its handler")
            .expect("handler signals entry");
        release_tx.send(true).expect("handlers await release");
        for task in parked {
            assert_eq!(task.await.unwrap(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn loopback_is_exempt_from_the_ledger_not_the_cap() {
        let idp = introspection_idp_answering(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "active": false })),
        )
        .await;
        let (router, _, limits) =
            router_authority_and_limits_for(oidc_config_with_reader_profile(&idp.uri()), false);
        for _ in 0..20 {
            let (status, _, _) = scoped_read_from(&router, loopback()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "never locked out");
        }

        let _held = limits
            .outbound
            .try_acquire_many(OUTBOUND_BEARER_CAP as u32)
            .expect("all permits free");
        let before = introspections(&idp).await;
        let (status, headers, _) = scoped_read_from(&router, loopback()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(headers["retry-after"], "5");
        assert_eq!(
            introspections(&idp).await,
            before,
            "nothing went to the provider"
        );
    }

    #[tokio::test]
    async fn forwarded_headers_key_the_ledger_when_trusted() {
        let idp = introspection_idp_answering(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "active": false })),
        )
        .await;
        let forwarded = [("x-forwarded-for", "198.51.100.9")];

        // Trusted: the forwarded address is the client, and it is booked.
        let (router, _, _) =
            router_authority_and_limits_for(oidc_config_with_reader_profile(&idp.uri()), true);
        for _ in 0..crate::auth_rate_limit::MAX_ATTEMPTS {
            let (status, _, _) = send_from(
                &router,
                Some(loopback()),
                &forwarded,
                "GET",
                "/api/quickstart/state",
                Some("opaque-token"),
                Some("oidc.test"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        let (status, _, _) = send_from(
            &router,
            Some(loopback()),
            &forwarded,
            "GET",
            "/api/quickstart/state",
            Some("opaque-token"),
            Some("oidc.test"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

        // Not trusted: the header is ignored, the peer is loopback, exempt.
        let (router, _, _) =
            router_authority_and_limits_for(oidc_config_with_reader_profile(&idp.uri()), false);
        for _ in 0..20 {
            let (status, _, _) = send_from(
                &router,
                Some(loopback()),
                &forwarded,
                "GET",
                "/api/quickstart/state",
                Some("opaque-token"),
                Some("oidc.test"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn bearer_client_keys_follow_persisted_forwarded_header_trust() {
        for (initial_trust, updated_trust, peer, forwarded) in [
            (true, false, remote(), "203.0.113.8"),
            (false, true, loopback(), "203.0.113.7"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let idp = introspection_idp_answering(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "active": false })),
            )
            .await;
            let config = editor_config(&tmp, &idp.uri(), &[Verb::Read], &[]);
            let config_path = config.config_path.clone();
            let (router, _, limits) = router_authority_and_limits_for(config, initial_trust);
            let headers = [("x-forwarded-for", forwarded)];

            // The initial key is a different forwarded address or loopback.
            // Persisting the opposite policy must make this same router bill
            // 203.0.113.7, without rebuilding its routes or its ledger.
            let (status, _, _) = send_from(
                &router,
                Some(peer),
                &headers,
                "GET",
                "/api/quickstart/state",
                Some("opaque-token"),
                Some("oidc.test"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);

            let (status, body) = send(
                &router,
                "PUT",
                "/api/config/prop",
                OPERATOR.0,
                OPERATOR.1,
                Some(serde_json::json!({
                    "path": "gateway.trust_forwarded_headers",
                    "value": updated_trust,
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let persisted = zeroclaw_config::migration::migrate_to_current(
                &std::fs::read_to_string(config_path).unwrap(),
            )
            .unwrap();
            assert_eq!(persisted.gateway.trust_forwarded_headers, updated_trust);

            for attempt in 0..=crate::auth_rate_limit::MAX_ATTEMPTS {
                let (status, _, _) = send_from(
                    &router,
                    Some(peer),
                    &headers,
                    "GET",
                    "/api/quickstart/state",
                    Some("opaque-token"),
                    Some("oidc.test"),
                    None,
                )
                .await;
                assert_eq!(
                    status,
                    if attempt < crate::auth_rate_limit::MAX_ATTEMPTS {
                        StatusCode::UNAUTHORIZED
                    } else {
                        StatusCode::TOO_MANY_REQUESTS
                    },
                    "trust {initial_trust} -> {updated_trust}, attempt {attempt}"
                );
            }
            assert!(limits.attempts.is_locked_out("203.0.113.7"));
            assert_eq!(
                introspections(&idp).await,
                1 + crate::auth_rate_limit::MAX_ATTEMPTS as usize,
                "the locked-out request must not reach the provider"
            );
        }
    }

    #[tokio::test]
    async fn native_bearer_failures_keep_the_legacy_shape_and_are_not_booked() {
        let (router, _, limits) = router_authority_and_limits_for(paired_config(), false);
        for _ in 0..15 {
            let (status, _, body) = send_from(
                &router,
                Some(remote()),
                &[],
                "GET",
                "/api/config",
                Some("zc_wrong"),
                None,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["error"], LEGACY_DENIAL);
        }
        assert!(!limits.attempts.is_locked_out("203.0.113.7"));
        assert_eq!(limits.outbound.available_permits(), OUTBOUND_BEARER_CAP);
    }
}
