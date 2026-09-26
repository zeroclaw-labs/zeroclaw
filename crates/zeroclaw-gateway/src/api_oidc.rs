//! Cross-surface OIDC enrollment API.
//!
//! Design: `docs/security/oidc-browser-pkce-design-8289.md`. These routes
//! are unauthenticated by necessity (enrollment precedes authentication),
//! rate limited, and grant nothing: they relay what the IdP grants after
//! the user approves. The gateway holds the `[oidc.<alias>]` client
//! credentials so browsers and zerocode need none.
//!
//! - `GET  /api/oidc/providers` lists configured aliases.
//! - `POST /api/oidc/{alias}/device/start` proxies the RFC 8628 start.
//! - `POST /api/oidc/{alias}/device/poll` proxies one token poll. Its
//!   statuses are a contract a polling client reads: 403 is the
//!   authorization server's refusal and ends the flow, while 429 and
//!   503 (both with `Retry-After`) and 502 are "not now" and are
//!   retryable for as long as the device code lives.
//! - `GET  /oidc/login/{alias}` starts Authorization Code + PKCE (302).
//! - `GET  /oidc/callback` finishes it: the one-time page hands the token
//!   to `window.opener` via `postMessage` (gateway origin only) with a
//!   manual copy fallback. No cookies, no session: clients keep
//!   authenticating per request with the route-layer headers.
//!
//! Because nothing here is authenticated, every route is bounded *before*
//! it can cause outbound work, by four independent limits held in
//! `OidcEnrollmentState`: an attempt limiter refuses clients that are
//! already locked out, a per-client sliding-window budget caps relay
//! requests per minute, a process-wide semaphore caps how many IdP round
//! trips can be in flight at once, and the pending-flow store bounds both
//! its own size and how much of it one client can hold. Every response from these routes
//! is `Cache-Control: no-store`: each one carries, or is one step from, a
//! device code or an access token.
//!
//! The attempt limiter is this surface's own, not the gateway-wide
//! `auth_limiter` that `POST /pair` and webhook auth share. Those two
//! count only failures, so billing enrollment traffic to the same counter
//! would let a browser sign-in loop lock a client out of pairing, and a
//! wrong pairing code shorten the enrollment budget. Both limiters are
//! built from the same thresholds and share the loopback exemption.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use tokio::sync::{Semaphore, SemaphorePermit};
use zeroclaw_runtime::security::auth_provider::{DevicePollOutcome, Enrollment, PkceFlow};

use crate::{AppState, SlidingWindowRateLimiter};

const FLOW_TTL: Duration = Duration::from_secs(600);
const FLOW_CAP: usize = 32;

/// Pending flows one remote client may hold at once. The store is shared,
/// so a process-wide cap alone lets one caller park every slot for the
/// whole TTL and refuse browser sign-ins to everyone else, at a request
/// rate low enough that the attempt limiter never locks it out. A browser
/// needs one flow, or a few across tabs and retries, so a quarter of the
/// store bounds one caller while leaving the rest available.
///
/// Loopback is exempt on the same terms as the other per-client limits:
/// those callers are the host's own dashboard and zerocode, they share one
/// key, and a share would throttle the local surface rather than protect
/// it. A deployment behind a reverse proxy wants `trust_forwarded_headers`
/// so its callers are told apart here as well.
const PER_CLIENT_FLOW_CAP: usize = 8;

/// Refusal when the pending-flow store cannot take another flow, either
/// because the shared store is full or because this client already holds
/// its share. Raised twice: once before the IdP round trip that starts a
/// flow, and once on the insert that follows it, so a burst that passes
/// the first check still cannot push the store past its caps.
const FLOW_STORE_FULL: &str = "too many in-flight sign-ins; retry shortly";

/// Relay requests one client may spend per minute across the enrollment
/// surface. RFC 8628 puts the default minimum polling interval at five
/// seconds, so a device-flow client that honours it spends twelve polls a
/// minute; twenty leaves headroom for interval jitter and for the provider
/// listing the same client fetches alongside its polls.
pub(crate) const POLL_BUDGET_PER_MINUTE: u32 = 20;

/// Window the budget above is measured over.
const POLL_BUDGET_WINDOW: Duration = Duration::from_secs(60);

/// Distinct client keys the budget tracks before it starts evicting the
/// least recently seen one. Bounds the map a spray of source addresses (or
/// of forwarded headers, where those are trusted) can grow.
const POLL_BUDGET_MAX_KEYS: usize = 4096;

/// Enrollment requests to an identity provider allowed in flight at once,
/// across all clients. This is the bound that still applies where the
/// per-client budget does not: a loopback caller, or a reverse proxy that
/// collapses every client onto one address because forwarded headers are
/// not trusted. Exceeding it refuses the request instead of queueing it.
pub(crate) const OUTBOUND_RELAY_CAP: usize = 16;

/// `Retry-After` for the two capacity refusals (outbound cap reached,
/// pending-flow store full). Both clear as soon as an in-flight enrollment
/// finishes, so the hint is short.
const CAPACITY_RETRY_AFTER_SECS: u64 = 5;

struct PendingPkce {
    alias: String,
    flow: PkceFlow,
    created: Instant,
    /// The client this flow was started for, under the same key the
    /// limiters bill, so one caller's share can be counted.
    client: String,
}

/// In-flight PKCE flows keyed by `state`, following the pairing-store
/// posture: in-memory, single-use consume-on-arrival, short TTL, capped.
struct OidcFlowStore {
    flows: parking_lot::Mutex<HashMap<String, PendingPkce>>,
    ttl: Duration,
}

impl Default for OidcFlowStore {
    fn default() -> Self {
        Self {
            flows: parking_lot::Mutex::new(HashMap::new()),
            ttl: FLOW_TTL,
        }
    }
}

impl OidcFlowStore {
    /// Whether a flow started now by `client` would have somewhere to land,
    /// counting only entries still inside the TTL. Both the shared cap and
    /// this client's share are checked *before* the IdP round trip so a
    /// refusal costs no outbound request; [`Self::insert`] repeats them for
    /// the flows that start between this answer and their own insert.
    fn has_capacity(&self, client: &str) -> bool {
        let mut flows = self.flows.lock();
        flows.retain(|_, p| p.created.elapsed() < self.ttl);
        Self::has_room(&flows, client)
    }

    fn has_room(flows: &HashMap<String, PendingPkce>, client: &str) -> bool {
        if flows.len() >= FLOW_CAP {
            return false;
        }
        if crate::auth_rate_limit::is_loopback_key(client) {
            return true;
        }
        flows.values().filter(|p| p.client == client).count() < PER_CLIENT_FLOW_CAP
    }

    fn insert(&self, pending: PendingPkce) -> Result<(), &'static str> {
        let mut flows = self.flows.lock();
        flows.retain(|_, p| p.created.elapsed() < self.ttl);
        if !Self::has_room(&flows, &pending.client) {
            return Err(FLOW_STORE_FULL);
        }
        flows.insert(pending.flow.state.clone(), pending);
        Ok(())
    }

    fn consume(&self, state_key: &str) -> Option<PendingPkce> {
        let mut flows = self.flows.lock();
        flows.retain(|_, p| p.created.elapsed() < self.ttl);
        flows.remove(state_key)
    }

    /// Put a consumed flow back when the callback holding it was turned
    /// away for the gateway's own capacity, so the retry its `Retry-After`
    /// invites still has a flow to finish. Nothing about the flow was
    /// spent: no code reached the IdP.
    ///
    /// Neither cap is re-checked. This entry held a slot moments earlier
    /// and the path that returns it here has no await between the consume
    /// and this call, so the only way to land above [`FLOW_CAP`] or a
    /// client's share is a login that took the freed slot on another worker
    /// thread in that window:
    /// bounded, transient, and still swept by the TTL pass above. Refusing
    /// the restore instead would drop a live sign-in to keep a soft bound
    /// exact.
    ///
    /// `created` rides back untouched, so a retry inherits what is left of
    /// the original deadline rather than a fresh TTL; a flow that expired
    /// while the callback was in the handler is dropped, not revived.
    fn restore(&self, pending: PendingPkce) {
        let mut flows = self.flows.lock();
        flows.retain(|_, p| p.created.elapsed() < self.ttl);
        if pending.created.elapsed() >= self.ttl {
            return;
        }
        flows.insert(pending.flow.state.clone(), pending);
    }
}

/// Everything the enrollment routes need beyond [`AppState`]: the pending
/// browser flows plus the three limits that bound what an unauthenticated
/// caller can make this gateway do.
pub(crate) struct OidcEnrollmentState {
    flows: OidcFlowStore,
    /// This surface's own attempt counter, deliberately separate from
    /// `AppState::auth_limiter`: a lockout earned here must not reach
    /// pairing or webhook auth, nor theirs reach enrollment.
    attempts: crate::auth_rate_limit::AuthRateLimiter,
    poll_budget: SlidingWindowRateLimiter,
    pub(crate) outbound: Semaphore,
}

impl Default for OidcEnrollmentState {
    fn default() -> Self {
        Self {
            flows: OidcFlowStore::default(),
            attempts: crate::auth_rate_limit::AuthRateLimiter::new(),
            poll_budget: SlidingWindowRateLimiter::new(
                POLL_BUDGET_PER_MINUTE,
                POLL_BUDGET_WINDOW,
                POLL_BUDGET_MAX_KEYS,
            ),
            outbound: Semaphore::new(OUTBOUND_RELAY_CAP),
        }
    }
}

pub fn routes() -> Router<AppState> {
    routes_with(Arc::new(OidcEnrollmentState::default()))
}

/// Same routes over a caller-supplied state, so a test can hold the budget
/// and semaphore handles the routes are enforcing.
pub(crate) fn routes_with(enrollment_state: Arc<OidcEnrollmentState>) -> Router<AppState> {
    Router::new()
        .route("/api/oidc/providers", get(handle_providers))
        .route("/api/oidc/{alias}/device/start", post(handle_device_start))
        .route("/api/oidc/{alias}/device/poll", post(handle_device_poll))
        .route("/oidc/login/{alias}", get(handle_pkce_login))
        .route("/oidc/callback", get(handle_pkce_callback))
        .layer(axum::middleware::from_fn(no_store))
        .layer(Extension(enrollment_state))
}

/// Keep enrollment responses out of caches, back/forward restores and
/// proxies: the callback page embeds an access token, the device start
/// returns a device code, and a granted poll returns the token itself.
/// Attached inside [`routes_with`] so the production router gets it by
/// merging these routes, next to (not instead of) the gateway-wide
/// security headers, which set `no-referrer` and COOP but no cache policy.
async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

/// The client this request is billed to: the forwarded address only where
/// the deployment trusts its proxy, the peer address otherwise. Same
/// derivation as every other rate-limited gateway surface.
fn client_key(state: &AppState, peer: SocketAddr, headers: &HeaderMap) -> String {
    crate::client_key_from_request(Some(peer), headers, state.trust_forwarded_headers)
}

fn set_retry_after(response: &mut Response, secs: u64) {
    if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
}

fn too_many_requests(message: &str, retry_after_secs: u64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "error": message,
            "retry_after": retry_after_secs,
        })),
    )
        .into_response();
    set_retry_after(&mut response, retry_after_secs);
    response
}

/// Brute-force gate: refuse a client this surface's limiter has locked
/// out. `record` distinguishes the flow-starting requests (counted on
/// arrival, because each one commits the gateway to an IdP round trip)
/// from the routes counted by outcome: a poll is counted once the IdP's
/// answer shows the caller is polling too fast or feeding the relay
/// invalid device codes, and a callback once it turns out not to have
/// finished a sign-in.
fn auth_gate(
    enrollment_state: &OidcEnrollmentState,
    key: &str,
    record: bool,
) -> Result<(), Box<Response>> {
    if let Err(e) = enrollment_state.attempts.check_rate_limit(key) {
        return Err(Box::new(too_many_requests(
            &format!(
                "Too many enrollment attempts. Try again in {}s.",
                e.retry_after_secs
            ),
            e.retry_after_secs,
        )));
    }
    if record {
        enrollment_state.attempts.record_attempt(key);
    }
    Ok(())
}

/// Per-client request budget for the relay routes, consumed before any
/// outbound work. Loopback is exempt on exactly the terms the auth limiter
/// exempts it on, so a local dashboard and a local zerocode keep working
/// while a remote caller stays bounded.
fn budget_gate(enrollment_state: &OidcEnrollmentState, key: &str) -> Result<(), Box<Response>> {
    if crate::auth_rate_limit::is_loopback_key(key) {
        return Ok(());
    }
    enrollment_state
        .poll_budget
        .allow_or_retry_after(key)
        .map_err(|retry_after_secs| {
            Box::new(too_many_requests(
                &format!("Too many enrollment requests. Try again in {retry_after_secs}s."),
                retry_after_secs,
            ))
        })
}

/// Reserve one of the outbound slots. The permit is held for the whole IdP
/// round trip, so the cap counts requests in flight rather than started.
fn outbound_permit(enrollment_state: &OidcEnrollmentState) -> Option<SemaphorePermit<'_>> {
    enrollment_state.outbound.try_acquire().ok()
}

/// Refusal when the outbound cap is reached: nothing is sent to the IdP.
fn relay_busy() -> Response {
    let mut response = error_json(
        StatusCode::SERVICE_UNAVAILABLE,
        "enrollment relay is busy; retry shortly",
    );
    set_retry_after(&mut response, CAPACITY_RETRY_AFTER_SECS);
    response
}

/// The same refusal for the browser callback, which answers in HTML.
fn relay_busy_page() -> Response {
    let mut response = failure_page(StatusCode::SERVICE_UNAVAILABLE);
    set_retry_after(&mut response, CAPACITY_RETRY_AFTER_SECS);
    response
}

fn error_json(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// Build the enrollment client for a configured alias.
fn enrollment_for(state: &AppState, alias: &str) -> Result<Enrollment, Box<Response>> {
    let entry = {
        let config = state.config.read();
        config.oidc.get(alias).cloned()
    };
    let Some(entry) = entry else {
        return Err(Box::new(error_json(
            StatusCode::NOT_FOUND,
            "unknown oidc provider alias",
        )));
    };
    Enrollment::new(alias, entry).map_err(|_| {
        Box::new(error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "enrollment client construction failed",
        ))
    })
}

async fn handle_providers(
    State(state): State<AppState>,
    Extension(relay): Extension<Arc<OidcEnrollmentState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let key = client_key(&state, peer, &headers);
    if let Err(denied) = auth_gate(&relay, &key, false) {
        return *denied;
    }
    if let Err(denied) = budget_gate(&relay, &key) {
        return *denied;
    }
    let mut aliases: Vec<String> = state.config.read().oidc.keys().cloned().collect();
    aliases.sort();
    let providers: Vec<serde_json::Value> = aliases
        .iter()
        .map(|alias| {
            serde_json::json!({
                "alias": alias,
                "provider": format!("oidc.{alias}"),
            })
        })
        .collect();
    Json(serde_json::json!({ "providers": providers })).into_response()
}

async fn handle_device_start(
    State(state): State<AppState>,
    Extension(relay): Extension<Arc<OidcEnrollmentState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(alias): Path<String>,
    headers: HeaderMap,
) -> Response {
    let key = client_key(&state, peer, &headers);
    if let Err(denied) = auth_gate(&relay, &key, true) {
        return *denied;
    }
    let enrollment = match enrollment_for(&state, &alias) {
        Ok(enrollment) => enrollment,
        Err(response) => return *response,
    };
    let Some(_permit) = outbound_permit(&relay) else {
        return relay_busy();
    };
    match enrollment.device_grant_start().await {
        Ok(start) => Json(start).into_response(),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"alias": alias, "error": format!("{e}")})),
                "oidc device enrollment start failed"
            );
            error_json(StatusCode::BAD_GATEWAY, &format!("{e}"))
        }
    }
}

/// No `Debug`: the one field is a device code, one successful poll away
/// from an access token. Anything that needs to name this body writes a
/// redacting impl rather than deriving one.
#[derive(Deserialize)]
struct DevicePollBody {
    device_code: String,
}

async fn handle_device_poll(
    State(state): State<AppState>,
    Extension(relay): Extension<Arc<OidcEnrollmentState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(alias): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DevicePollBody>,
) -> Response {
    let key = client_key(&state, peer, &headers);
    if let Err(denied) = auth_gate(&relay, &key, false) {
        return *denied;
    }
    if let Err(denied) = budget_gate(&relay, &key) {
        return *denied;
    }
    let enrollment = match enrollment_for(&state, &alias) {
        Ok(enrollment) => enrollment,
        Err(response) => return *response,
    };
    let Some(_permit) = outbound_permit(&relay) else {
        return relay_busy();
    };
    match enrollment.device_grant_poll(&body.device_code).await {
        Ok(DevicePollOutcome::Pending) => {
            Json(serde_json::json!({ "status": "pending" })).into_response()
        }
        Ok(DevicePollOutcome::SlowDown) => {
            // The IdP says this caller is polling faster than the grant
            // allows. Count it: a caller that ignores back-off walks into
            // the same lockout a password guesser does.
            relay.attempts.record_attempt(&key);
            Json(serde_json::json!({ "status": "slow_down" })).into_response()
        }
        // Only what the enrolling client needs to authenticate: the access
        // token and its lifetime. A refresh token the IdP may have issued
        // is not relayed; renewal is out of scope here, and a long-lived
        // credential nobody consumes has no business in a browser tab or a
        // terminal.
        Ok(DevicePollOutcome::Token(token)) => Json(serde_json::json!({
            "status": "granted",
            "provider": format!("oidc.{alias}"),
            "token": {
                "access_token": token.access_token,
                "expires_in": token.expires_in,
            },
        }))
        .into_response(),
        Ok(DevicePollOutcome::Denied(reason)) => {
            // The IdP rejected the device code outright: what a caller
            // relaying guesses produces, so it counts as an attempt too.
            relay.attempts.record_attempt(&key);
            // Not 502: the round trip succeeded and the authorization server
            // made a decision, so this is a refusal rather than a relay
            // failure. Sharing a status with the `Err` arm below would leave
            // a polling client unable to tell "the user said no" from "try
            // again", and it can only treat one of the two as retryable.
            error_json(
                StatusCode::FORBIDDEN,
                &format!("device grant failed: {reason}"),
            )
        }
        // A transport or parse failure on the way to the IdP says nothing
        // about the caller, so it is not billed to them. 502 is reserved for
        // exactly this: the relay could not complete the round trip, which a
        // client may retry while the grant is still alive.
        Err(e) => error_json(StatusCode::BAD_GATEWAY, &format!("{e}")),
    }
}

/// The callback URI this deployment's browser flow lands on, derived
/// from the request the user's browser just made. A spoofed Host cannot
/// redeem anything: the IdP only accepts registered redirect URIs, and
/// the exchange later uses the exact URI stored at flow start.
fn callback_uri(state: &AppState, headers: &HeaderMap) -> Result<String, Box<Response>> {
    let forwarded_proto = state
        .trust_forwarded_headers
        .then(|| headers.get("x-forwarded-proto"))
        .flatten()
        .and_then(|v| v.to_str().ok());
    let scheme = match forwarded_proto {
        Some(proto) => proto.to_string(),
        None => {
            // A `[gateway.tls]` block that is present but not enabled is
            // the schema default, and the server it configures listens in
            // plain HTTP. Read `enabled`, exactly as the listener setup
            // and the HSTS decision do: a `https://` redirect URI against
            // an HTTP listener is one the browser can never come back to.
            let tls_on = state
                .config
                .read()
                .gateway
                .tls
                .as_ref()
                .is_some_and(|tls| tls.enabled);
            if tls_on {
                "https".into()
            } else {
                "http".into()
            }
        }
    };
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Box::new(error_json(StatusCode::BAD_REQUEST, "missing Host header")))?;
    Ok(format!(
        "{scheme}://{host}{}/oidc/callback",
        state.path_prefix
    ))
}

async fn handle_pkce_login(
    State(state): State<AppState>,
    Extension(relay): Extension<Arc<OidcEnrollmentState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(alias): Path<String>,
    headers: HeaderMap,
) -> Response {
    let key = client_key(&state, peer, &headers);
    if let Err(denied) = auth_gate(&relay, &key, false) {
        return *denied;
    }
    // The budget is what bounds this route's request rate now that a
    // capacity refusal is free: without it a client parked at its share
    // could ask forever at no cost.
    if let Err(denied) = budget_gate(&relay, &key) {
        return *denied;
    }
    let enrollment = match enrollment_for(&state, &alias) {
        Ok(enrollment) => enrollment,
        Err(response) => return *response,
    };
    let redirect_uri = match callback_uri(&state, &headers) {
        Ok(uri) => uri,
        Err(response) => return *response,
    };
    // Refuse before the discovery round trip, not after it: otherwise every
    // login past a cap still costs the gateway an outbound request to the
    // IdP to produce a flow with nowhere to go. The refusal is the
    // gateway's capacity, not a failed authentication, so it is not billed
    // as an attempt; the login that goes on to commit an outbound round
    // trip is.
    if !relay.flows.has_capacity(&key) {
        return too_many_requests(FLOW_STORE_FULL, CAPACITY_RETRY_AFTER_SECS);
    }
    relay.attempts.record_attempt(&key);
    let flow = {
        let Some(_permit) = outbound_permit(&relay) else {
            return relay_busy();
        };
        match enrollment.pkce_start(&redirect_uri).await {
            Ok(flow) => flow,
            Err(e) => return error_json(StatusCode::BAD_GATEWAY, &format!("{e}")),
        }
    };
    let authorize_url = flow.authorize_url.clone();
    if let Err(full) = relay.flows.insert(PendingPkce {
        alias,
        flow,
        created: Instant::now(),
        client: key,
    }) {
        return too_many_requests(full, CAPACITY_RETRY_AFTER_SECS);
    }
    Redirect::temporary(&authorize_url).into_response()
}

/// No `Debug`: `code` is an authorization code, redeemable for an access
/// token until it is spent. Anything that needs to name this query writes
/// a redacting impl rather than deriving one.
#[derive(Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    /// RFC 9207 issuer identification. Checked against the issuer this
    /// flow was started for before the response is acted on either way.
    #[serde(default)]
    iss: Option<String>,
}

/// A callback that did not finish a sign-in: no live flow for the state,
/// a mix-up, an IdP refusal, a code that would not exchange. Each one is
/// billed to the caller before the fixed page goes out — sprayed `state`
/// values and replayed codes are exactly what this route has to bound —
/// while the callback that completes an enrollment costs nothing. A
/// callback turned away because every outbound relay slot is taken is not
/// routed here either: the refusal is the gateway's capacity limit, not a
/// failure on the caller's side, so it answers with the busy page unbilled
/// and puts the untouched flow back for the retry it asks for.
fn unproductive_callback(
    enrollment_state: &OidcEnrollmentState,
    key: &str,
    status: StatusCode,
) -> Response {
    enrollment_state.attempts.record_attempt(key);
    failure_page(status)
}

/// Fixed failure page: never echoes request content. Details go to the
/// server log only.
fn failure_page(status: StatusCode) -> Response {
    (
        status,
        Html(
            "<!doctype html><meta charset=\"utf-8\"><title>Sign-in not completed</title>\
             <body style=\"font-family:system-ui;margin:3rem\"><h1>Sign-in not completed</h1>\
             <p>This request did not complete an enrollment. Start again from the \
             application, or check the gateway log for details.</p>",
        ),
    )
        .into_response()
}

/// One-time success page (decision 4 of the design note): hands the
/// token to `window.opener` via `postMessage` with this page's own
/// origin as the target, plus a manual copy fallback. The token reaches
/// the page only as a JSON string literal inside the inline script.
fn success_page(provider: &str, access_token: &str, expires_in: Option<u64>) -> Response {
    // `</` cannot appear inside the script element, whatever the IdP
    // returned; JSON string escaping handles the rest.
    let token_json = serde_json::to_string(access_token)
        .unwrap_or_else(|_| "\"\"".into())
        .replace("</", "<\\/");
    let provider_json = serde_json::to_string(provider)
        .unwrap_or_else(|_| "\"\"".into())
        .replace("</", "<\\/");
    let expires_note = expires_in
        .map(|secs| format!("<p>The token expires in {secs} seconds.</p>"))
        .unwrap_or_default();
    Html(format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Signed in</title>\
         <body style=\"font-family:system-ui;margin:3rem\">\
         <h1>Signed in</h1>\
         <p>Enrollment is complete. If the application does not pick the token up \
         automatically, copy it below and close this tab.</p>{expires_note}\
         <p><code id=\"token\" style=\"word-break:break-all\"></code></p>\
         <script>(function () {{\
           var token = {token_json};\
           var provider = {provider_json};\
           document.getElementById(\"token\").textContent = token;\
           try {{\
             if (window.opener) {{\
               window.opener.postMessage(\
                 {{ type: \"zeroclaw-oidc-token\", provider: provider, access_token: token }},\
                 window.location.origin\
               );\
             }}\
           }} catch (e) {{}}\
         }})();</script>",
    ))
    .into_response()
}

async fn handle_pkce_callback(
    State(state): State<AppState>,
    Extension(relay): Extension<Arc<OidcEnrollmentState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let key = client_key(&state, peer, &headers);
    // Counted by outcome, not on arrival: the callback that finishes a
    // browser sign-in is the successful end of a flow this gateway itself
    // started, and charging it would halve how many sign-ins a shared
    // address gets. Every other way out of this handler records below,
    // except a refusal for relay capacity: that limit is the gateway's own,
    // and the caller did nothing wrong by arriving while it was reached.
    if let Err(denied) = auth_gate(&relay, &key, false) {
        return *denied;
    }
    // State gates everything: without a live matching flow there is
    // nothing to fail, let alone finish.
    let Some(pending) = query.state.as_deref().and_then(|s| relay.flows.consume(s)) else {
        return unproductive_callback(&relay, &key, StatusCode::BAD_REQUEST);
    };
    // RFC 9207: a response that names another issuer is a mix-up, and is
    // not acted on either way — neither its code nor its error verdict.
    if let Err(e) = pending.flow.check_callback_issuer(query.iss.as_deref()) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "alias": pending.alias,
                    "error": format!("{e}"),
                })),
            "oidc browser sign-in response names a different issuer"
        );
        return unproductive_callback(&relay, &key, StatusCode::BAD_REQUEST);
    }
    if let Some(error) = query.error {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "alias": pending.alias,
                    "error": error,
                    "error_description": query.error_description,
                })),
            "oidc browser sign-in denied by the identity provider"
        );
        return unproductive_callback(&relay, &key, StatusCode::BAD_REQUEST);
    }
    let Some(code) = query.code else {
        return unproductive_callback(&relay, &key, StatusCode::BAD_REQUEST);
    };
    let enrollment = match enrollment_for(&state, &pending.alias) {
        Ok(enrollment) => enrollment,
        // The alias was removed while the flow was in flight: fail closed.
        Err(_) => return unproductive_callback(&relay, &key, StatusCode::BAD_REQUEST),
    };
    let Some(_permit) = outbound_permit(&relay) else {
        // The busy page carries a short `Retry-After`. That invitation is
        // only honest if the retry can still finish the sign-in: the state
        // is single-use and this attempt used none of it, so hand the flow
        // back. Without this the retry finds nothing pending, is billed as
        // an unproductive callback, and walks the caller toward a lockout
        // for a refusal the gateway itself issued.
        relay.flows.restore(pending);
        return relay_busy_page();
    };
    match enrollment.pkce_exchange(&pending.flow, &code).await {
        Ok(token) => success_page(
            &format!("oidc.{}", pending.alias),
            &token.access_token,
            token.expires_in,
        ),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "alias": pending.alias,
                        "error": format!("{e}"),
                    })),
                "oidc browser sign-in code exchange failed"
            );
            unproductive_callback(&relay, &key, StatusCode::BAD_GATEWAY)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    use wiremock::matchers::{body_string_contains, method as http_method, path as http_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::schema::{Config, GatewayTlsConfig, OidcConfig, OidcValidation};

    use crate::auth_rate_limit::MAX_ATTEMPTS;

    async fn idp() -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(http_method("GET"))
            .and(http_path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "device_authorization_endpoint": format!("{issuer}/device"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        server
    }

    fn config_with_alias(issuer: &str, alias: &str) -> Config {
        let mut config = Config::default();
        config.oidc.insert(
            alias.to_string(),
            OidcConfig {
                issuer: issuer.to_string(),
                audience: "zeroclaw".into(),
                client_id: "gw".into(),
                client_secret: Some("s3cret".into()),
                validation: OidcValidation::Introspection,
                claim_path: "groups".into(),
                profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
                ..OidcConfig::default()
            },
        );
        config
    }

    fn router_for(config: Config) -> Router {
        routes().with_state(crate::api::tests::test_state(config))
    }

    /// The default test peer: loopback, exempt from the per-client budget.
    fn loopback() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 39999))
    }

    /// A remote caller, which every per-client limit applies to.
    fn remote() -> SocketAddr {
        SocketAddr::from(([203, 0, 113, 7], 40000))
    }

    /// A second remote caller, for the limits that have to keep one client
    /// from spending what another one needs.
    fn other_remote() -> SocketAddr {
        SocketAddr::from(([198, 51, 100, 9], 40001))
    }

    /// The key every limiter bills [`remote`] under: the peer IP, no port.
    fn remote_key() -> String {
        remote().ip().to_string()
    }

    /// How many requests the IdP mock has seen in total — the count that
    /// says whether a refusal happened before or after any outbound call.
    async fn total_requests(server: &MockServer) -> usize {
        server.received_requests().await.unwrap_or_default().len()
    }

    fn build_request(
        http_method: &str,
        path: &str,
        host: &str,
        body: Option<serde_json::Value>,
        peer: SocketAddr,
        extra_headers: &[(&str, &str)],
    ) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder()
            .method(http_method)
            .uri(path)
            .header("host", host);
        for (name, value) in extra_headers {
            builder = builder.header(*name, *value);
        }
        let payload = body.map(|json| json.to_string());
        if let Some(payload) = payload.as_deref() {
            builder = builder
                .header("content-type", "application/json")
                .header("content-length", payload.len().to_string());
        }
        let mut request = match payload {
            Some(payload) => builder.body(Body::from(payload)).unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    async fn send(
        router: &Router,
        http_method: &str,
        path: &str,
        host: &str,
        body: Option<serde_json::Value>,
    ) -> axum::response::Response {
        let request = build_request(http_method, path, host, body, loopback(), &[]);
        router.clone().oneshot(request).await.unwrap()
    }

    async fn send_as(
        router: &Router,
        peer: SocketAddr,
        http_method: &str,
        path: &str,
        host: &str,
        body: Option<serde_json::Value>,
        extra_headers: &[(&str, &str)],
    ) -> axum::response::Response {
        let request = build_request(http_method, path, host, body, peer, extra_headers);
        router.clone().oneshot(request).await.unwrap()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        serde_json::from_str(&body_text(response).await).unwrap()
    }

    /// How many requests the IdP saw on one path — the count that says
    /// whether a refusal happened before or after the outbound call.
    async fn requests_to(server: &MockServer, path: &str) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| request.url.path() == path)
            .count()
    }

    fn header_value(response: &axum::response::Response, name: &str) -> String {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    fn poll_body() -> Option<serde_json::Value> {
        Some(serde_json::json!({"device_code": "dev-relayed"}))
    }

    fn assert_no_store(label: &str, response: &axum::response::Response, status: StatusCode) {
        assert_eq!(response.status(), status, "{label}");
        assert_eq!(
            header_value(response, "cache-control"),
            "no-store",
            "{label}"
        );
        assert_eq!(header_value(response, "pragma"), "no-cache", "{label}");
    }

    /// Percent-encode a URL so it survives as one query-parameter value.
    fn query_escape(value: &str) -> String {
        value
            .replace('%', "%25")
            .replace(':', "%3A")
            .replace('/', "%2F")
    }

    #[tokio::test]
    async fn providers_lists_configured_aliases_sorted() {
        let mut config = config_with_alias("https://a.example.com", "zeta");
        config
            .oidc
            .insert("alpha".into(), config.oidc.get("zeta").cloned().unwrap());
        let router = router_for(config);
        let response = send(&router, "GET", "/api/oidc/providers", "gw.local", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(json["providers"][0]["alias"], "alpha");
        assert_eq!(json["providers"][1]["provider"], "oidc.zeta");
    }

    #[tokio::test]
    async fn unknown_alias_is_a_404() {
        let router = router_for(Config::default());
        let response = send(
            &router,
            "POST",
            "/api/oidc/nope/device/start",
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn device_proxy_start_and_poll_round_trip() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-9",
                "user_code": "WXYZ-1234",
                "verification_uri": "https://sso.example.com/activate",
                "expires_in": 600,
                "interval": 5,
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .and(body_string_contains("device_code=still-pending"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .and(body_string_contains("device_code=dev-9"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-device",
                "token_type": "Bearer",
                "refresh_token": "rt-never-relayed",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));

        let response = send(
            &router,
            "POST",
            "/api/oidc/corp/device/start",
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let start: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(start["user_code"], "WXYZ-1234");

        let response = send(
            &router,
            "POST",
            "/api/oidc/corp/device/poll",
            "gw.local",
            Some(serde_json::json!({"device_code": "still-pending"})),
        )
        .await;
        let poll: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(poll["status"], "pending");

        let response = send(
            &router,
            "POST",
            "/api/oidc/corp/device/poll",
            "gw.local",
            Some(serde_json::json!({"device_code": "dev-9"})),
        )
        .await;
        let poll: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(poll["status"], "granted");
        assert_eq!(poll["provider"], "oidc.corp");
        assert_eq!(poll["token"]["access_token"], "at-device");
        assert_eq!(poll["token"]["expires_in"], 3600);
        assert!(
            poll["token"].get("refresh_token").is_none(),
            "the relay hands out only the access token and its lifetime: {poll}"
        );
    }

    #[tokio::test]
    async fn pkce_login_redirects_to_the_idp_with_s256_and_our_callback() {
        let server = idp().await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));
        let response = send(&router, "GET", "/oidc/login/corp", "gw.local:9443", None).await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        assert!(location.starts_with(&format!("{}/authorize", server.uri())));
        assert!(location.contains("code_challenge_method=S256"));
        assert!(
            location.contains("gw.local%3A9443%2Foidc%2Fcallback"),
            "redirect_uri derives from the request Host: {location}"
        );
    }

    /// The `redirect_uri` the login redirect sent the browser off with,
    /// still percent-encoded as the authorize URL carries it.
    async fn login_redirect_uri(
        config: Config,
        trust_forwarded_headers: bool,
        extra_headers: &[(&str, &str)],
    ) -> String {
        let mut state = crate::api::tests::test_state(config);
        state.trust_forwarded_headers = trust_forwarded_headers;
        let router = routes().with_state(state);
        let response = send_as(
            &router,
            loopback(),
            "GET",
            "/oidc/login/corp",
            "gw.local",
            None,
            extra_headers,
        )
        .await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        header_value(&response, "location")
            .split('&')
            .find_map(|pair| pair.strip_prefix("redirect_uri="))
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn the_login_redirect_uri_scheme_follows_the_effective_tls_setting() {
        let server = idp().await;
        let base = config_with_alias(&server.uri(), "corp");
        let tls_block = |enabled: bool| {
            Some(GatewayTlsConfig {
                enabled,
                cert_path: "/etc/zeroclaw/cert.pem".into(),
                key_path: "/etc/zeroclaw/key.pem".into(),
                client_auth: None,
            })
        };
        let http_uri = query_escape("http://gw.local/oidc/callback");
        let https_uri = query_escape("https://gw.local/oidc/callback");

        // No `[gateway.tls]` block at all: a plain HTTP listener.
        assert_eq!(
            login_redirect_uri(base.clone(), false, &[]).await,
            http_uri,
            "no TLS configured"
        );

        // Present but disabled — the schema default for the block, and
        // still a plain HTTP listener. An `https://` redirect URI here is
        // one the browser could never come back to.
        let mut disabled = base.clone();
        disabled.gateway.tls = tls_block(false);
        assert_eq!(
            login_redirect_uri(disabled, false, &[]).await,
            http_uri,
            "[gateway.tls] present with enabled = false"
        );

        // Enabled: the listener really is HTTPS.
        let mut enabled = base.clone();
        enabled.gateway.tls = tls_block(true);
        assert_eq!(
            login_redirect_uri(enabled, false, &[]).await,
            https_uri,
            "[gateway.tls] present with enabled = true"
        );

        // A TLS-terminating proxy the deployment trusts outranks both:
        // the browser's origin is the proxy's, not this listener's.
        let forwarded = [("x-forwarded-proto", "https")];
        assert_eq!(
            login_redirect_uri(base.clone(), true, &forwarded).await,
            https_uri,
            "trusted X-Forwarded-Proto"
        );

        // The same header from a hop the deployment does not trust says
        // nothing, and cannot talk the gateway into a foreign scheme.
        assert_eq!(
            login_redirect_uri(base, false, &forwarded).await,
            http_uri,
            "untrusted X-Forwarded-Proto is ignored"
        );
    }

    #[tokio::test]
    async fn callback_without_a_live_flow_is_refused() {
        let router = router_for(Config::default());
        let response = send(
            &router,
            "GET",
            "/oidc/callback?code=x&state=never-issued",
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response).await.contains("Sign-in not completed"));
    }

    fn state_from_location(location: &str) -> String {
        let query = location.split('?').nth(1).unwrap();
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix("state="))
            .unwrap()
            .to_string()
    }

    /// Start a browser flow and return the `state` the gateway issued.
    async fn start_login(router: &Router) -> String {
        start_login_as(router, loopback()).await
    }

    /// The same, from a chosen peer, for the tests that care which client
    /// the flow is billed to.
    async fn start_login_as(router: &Router, peer: SocketAddr) -> String {
        let response = send_as(
            router,
            peer,
            "GET",
            "/oidc/login/corp",
            "gw.local",
            None,
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        state_from_location(&header_value(&response, "location"))
    }

    #[tokio::test]
    async fn pkce_callback_exchanges_once_and_only_once() {
        let server = idp().await;
        // The exchange must present the redirect URI stored when the flow
        // started, byte for byte: RFC 6749 section 4.1.3 makes the IdP
        // compare it against the one the authorize request carried, and a
        // mock that does not see it answers 404 instead of a token.
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .and(body_string_contains(format!(
                "redirect_uri={}",
                query_escape("http://gw.local/oidc/callback")
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-browser",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));

        let flow_state = start_login(&router).await;

        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = body_text(response).await;
        assert!(page.contains("zeroclaw-oidc-token"), "postMessage handoff");
        assert!(page.contains("\"at-browser\""), "token as a JSON literal");
        assert!(page.contains("oidc.corp"));

        // Single use: the same state cannot be redeemed twice.
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn idp_error_on_callback_consumes_the_flow_and_shows_the_fixed_page() {
        let server = idp().await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));
        let flow_state = start_login(&router).await;
        let response = send(
            &router,
            "GET",
            &format!(
                "/oidc/callback?error=access_denied&error_description=nope&state={flow_state}"
            ),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let page = body_text(response).await;
        assert!(page.contains("Sign-in not completed"));
        assert!(
            !page.contains("access_denied") && !page.contains("nope"),
            "the failure page never echoes request content"
        );
    }

    #[tokio::test]
    async fn device_polls_consume_a_bounded_per_client_budget() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
            })))
            .mount(&server)
            .await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));

        // A remote caller that never started a flow still pays for every
        // poll it makes, because each one costs the gateway an IdP round
        // trip carrying this deployment's client credentials.
        for attempt in 0..POLL_BUDGET_PER_MINUTE {
            let response = send_as(
                &router,
                remote(),
                "POST",
                "/api/oidc/corp/device/poll",
                "gw.local",
                poll_body(),
                &[],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "poll {attempt}");
        }
        let response = send_as(
            &router,
            remote(),
            "POST",
            "/api/oidc/corp/device/poll",
            "gw.local",
            poll_body(),
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_header = header_value(&response, "retry-after");
        let retry_after = body_json(response).await["retry_after"].as_u64().unwrap();
        assert!(
            (1..=POLL_BUDGET_WINDOW.as_secs()).contains(&retry_after),
            "retry_after names when the window frees a slot: {retry_after}"
        );
        assert_eq!(retry_header, retry_after.to_string());

        assert_eq!(
            requests_to(&server, "/token").await,
            POLL_BUDGET_PER_MINUTE as usize,
            "the refused poll never reached the identity provider"
        );
    }

    #[tokio::test]
    async fn repeated_slow_down_walks_the_caller_into_the_lockout() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "slow_down" })),
            )
            .mount(&server)
            .await;
        let state = crate::api::tests::test_state(config_with_alias(&server.uri(), "corp"));
        // The limiter `POST /pair` and webhook auth share, kept to check
        // that this surface's lockout does not spill onto them.
        let shared_auth = Arc::clone(&state.auth_limiter);
        let router = routes().with_state(state);

        for attempt in 0..MAX_ATTEMPTS {
            let response = send_as(
                &router,
                remote(),
                "POST",
                "/api/oidc/corp/device/poll",
                "gw.local",
                poll_body(),
                &[],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "poll {attempt}");
            assert_eq!(body_json(response).await["status"], "slow_down");
        }
        let outbound = requests_to(&server, "/token").await;
        assert_eq!(outbound, MAX_ATTEMPTS as usize);

        let response = send_as(
            &router,
            remote(),
            "POST",
            "/api/oidc/corp/device/poll",
            "gw.local",
            poll_body(),
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            requests_to(&server, "/token").await,
            outbound,
            "the locked-out caller caused no further outbound work"
        );
        assert!(
            !shared_auth.is_locked_out(&remote_key()),
            "an enrollment lockout must not reach /pair or webhook auth for \
             the same client address"
        );
        // What `POST /pair` and webhook auth actually ask on arrival. The
        // shared limiter holds no record of this client, so its next
        // pairing attempt is served rather than refused for polls it never
        // made — and `is_locked_out` alone would not notice, since only a
        // check registers the lockout a recorded attempt earns.
        assert!(
            shared_auth.check_rate_limit(&remote_key()).is_ok(),
            "pairing from the same address is still allowed"
        );
    }

    #[tokio::test]
    async fn relayed_garbage_device_codes_run_into_the_lockout() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "expired_token" })),
            )
            .mount(&server)
            .await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));

        for attempt in 0..MAX_ATTEMPTS {
            let response = send_as(
                &router,
                remote(),
                "POST",
                "/api/oidc/corp/device/poll",
                "gw.local",
                poll_body(),
                &[],
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "poll {attempt}");
        }
        let outbound = requests_to(&server, "/token").await;
        assert_eq!(outbound, MAX_ATTEMPTS as usize);

        let response = send_as(
            &router,
            remote(),
            "POST",
            "/api/oidc/corp/device/poll",
            "gw.local",
            poll_body(),
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(!header_value(&response, "retry-after").is_empty());
        assert_eq!(requests_to(&server, "/token").await, outbound);
    }

    #[tokio::test]
    async fn completed_browser_sign_ins_are_not_billed_as_attempts() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-browser",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let relay = Arc::new(OidcEnrollmentState::default());
        let router = routes_with(Arc::clone(&relay)).with_state(crate::api::tests::test_state(
            config_with_alias(&server.uri(), "corp"),
        ));

        // Five sign-ins in a row from one remote address — a shared office
        // egress, or one user retrying. Each is a login (one attempt) plus
        // the callback that finishes it (none): a completed enrollment is
        // not a failed authentication and must not spend the budget.
        for round in 0..5 {
            let flow_state = start_login_as(&router, remote()).await;
            let response = send_as(
                &router,
                remote(),
                "GET",
                &format!("/oidc/callback?code=auth-{round}&state={flow_state}"),
                "gw.local",
                None,
                &[],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "sign-in {round}");
            assert!(body_text(response).await.contains("\"at-browser\""));
        }
        assert!(!relay.attempts.is_locked_out(&remote_key()));

        // Ten recorded attempts is already the lockout; five is not, so
        // the next login is still served.
        let response = send_as(
            &router,
            remote(),
            "GET",
            "/oidc/login/corp",
            "gw.local",
            None,
            &[],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::TEMPORARY_REDIRECT,
            "only the five logins were recorded, not the five callbacks"
        );
    }

    #[tokio::test]
    async fn callbacks_without_a_live_flow_run_the_caller_into_the_lockout() {
        let router = router_for(Config::default());

        // A caller spraying `state` values at the callback is guessing at
        // flows it did not start: each dead callback is an attempt.
        for attempt in 0..MAX_ATTEMPTS {
            let response = send_as(
                &router,
                remote(),
                "GET",
                "/oidc/callback?code=x&state=never-issued",
                "gw.local",
                None,
                &[],
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "callback {attempt}"
            );
        }
        let response = send_as(
            &router,
            remote(),
            "GET",
            "/oidc/callback?code=x&state=never-issued",
            "gw.local",
            None,
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(!header_value(&response, "retry-after").is_empty());
    }

    #[tokio::test]
    async fn a_relay_failure_reaching_the_idp_is_not_billed_to_the_caller() {
        // An alias pointed at a closed loopback port: every poll fails in
        // transport, before the IdP has said anything about the device
        // code. That is the deployment's problem, not the caller's, so it
        // must not walk a blameless client into a five-minute lockout.
        let router = router_for(config_with_alias("http://127.0.0.1:1", "corp"));

        // One more than the lockout threshold, and well inside the poll
        // budget, so a 429 here could only come from a recorded attempt.
        for attempt in 0..=MAX_ATTEMPTS {
            let response = send_as(
                &router,
                remote(),
                "POST",
                "/api/oidc/corp/device/poll",
                "gw.local",
                poll_body(),
                &[],
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_GATEWAY,
                "poll {attempt} is a relay failure, not an attempt"
            );
        }
    }

    #[tokio::test]
    async fn the_provider_listing_is_budgeted_for_remote_callers_only() {
        let router = router_for(config_with_alias("https://idp.example.com", "corp"));

        for attempt in 0..POLL_BUDGET_PER_MINUTE {
            let response = send_as(
                &router,
                remote(),
                "GET",
                "/api/oidc/providers",
                "gw.local",
                None,
                &[],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "listing {attempt}");
        }
        let response = send_as(
            &router,
            remote(),
            "GET",
            "/api/oidc/providers",
            "gw.local",
            None,
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        // Loopback keeps the exemption the auth limiter already grants it.
        for _ in 0..(POLL_BUDGET_PER_MINUTE * 2) {
            let response = send(&router, "GET", "/api/oidc/providers", "gw.local", None).await;
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn a_trusted_forwarded_address_is_the_budgeted_identity() {
        let mut state = crate::api::tests::test_state(Config::default());
        state.trust_forwarded_headers = true;
        let router = routes().with_state(state);
        let forwarded = [("x-forwarded-for", "198.51.100.4")];

        // The peer is loopback, but the deployment trusts its proxy, so the
        // budget follows the forwarded client, not the proxy.
        for attempt in 0..POLL_BUDGET_PER_MINUTE {
            let response = send_as(
                &router,
                loopback(),
                "GET",
                "/api/oidc/providers",
                "gw.local",
                None,
                &forwarded,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "listing {attempt}");
        }
        let response = send_as(
            &router,
            loopback(),
            "GET",
            "/api/oidc/providers",
            "gw.local",
            None,
            &forwarded,
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        // The same loopback peer without the header is a local client.
        for _ in 0..(POLL_BUDGET_PER_MINUTE + 1) {
            let response = send(&router, "GET", "/api/oidc/providers", "gw.local", None).await;
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn the_outbound_cap_refuses_relaying_before_contacting_the_idp() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "never-relayed",
                "token_type": "Bearer",
            })))
            .mount(&server)
            .await;
        let relay = Arc::new(OidcEnrollmentState::default());
        let router = routes_with(relay.clone()).with_state(crate::api::tests::test_state(
            config_with_alias(&server.uri(), "corp"),
        ));

        // A live flow, started while the relay still had capacity.
        let flow_state = start_login(&router).await;

        // Every outbound slot is taken; nothing may reach the IdP now.
        let _permits = relay
            .outbound
            .try_acquire_many(OUTBOUND_RELAY_CAP as u32)
            .unwrap();

        let response = send(
            &router,
            "POST",
            "/api/oidc/corp/device/poll",
            "gw.local",
            poll_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header_value(&response, "retry-after"), "5");
        assert_eq!(requests_to(&server, "/token").await, 0);

        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(body_text(response).await.contains("Sign-in not completed"));
        assert_eq!(
            requests_to(&server, "/token").await,
            0,
            "no code was exchanged while the relay was saturated"
        );
    }

    #[tokio::test]
    async fn callbacks_the_relay_turns_away_are_not_billed_as_attempts() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "never-relayed",
                "token_type": "Bearer",
            })))
            .mount(&server)
            .await;
        let relay = Arc::new(OidcEnrollmentState::default());
        let router = routes_with(Arc::clone(&relay)).with_state(crate::api::tests::test_state(
            config_with_alias(&server.uri(), "corp"),
        ));

        // Enough live flows for a lockout's worth of callbacks, started from
        // loopback so starting them bills nothing to the remote caller.
        let mut flow_states = Vec::new();
        for _ in 0..MAX_ATTEMPTS {
            flow_states.push(start_login(&router).await);
        }

        // Every outbound slot is taken while the remote caller's browsers
        // come back from the IdP.
        let permits = relay
            .outbound
            .try_acquire_many(OUTBOUND_RELAY_CAP as u32)
            .unwrap();
        for (round, flow_state) in flow_states.iter().enumerate() {
            let response = send_as(
                &router,
                remote(),
                "GET",
                &format!("/oidc/callback?code=auth-{round}&state={flow_state}"),
                "gw.local",
                None,
                &[],
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "callback {round}"
            );
        }
        assert_eq!(requests_to(&server, "/token").await, 0);
        drop(permits);

        // Billing each of those would have put the caller at the lockout
        // threshold, and this login would be refused with 429.
        let response = send_as(
            &router,
            remote(),
            "GET",
            "/oidc/login/corp",
            "gw.local",
            None,
            &[],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::TEMPORARY_REDIRECT,
            "a callback refused for relay capacity is not the caller's failed attempt"
        );
    }

    #[tokio::test]
    async fn every_enrollment_response_forbids_caching() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-9",
                "user_code": "WXYZ-1234",
                "verification_uri": "https://sso.example.com/activate",
                "expires_in": 600,
                "interval": 5,
            })))
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-cacheable-never",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));
        let flow_state = start_login(&router).await;

        assert_no_store(
            "providers",
            &send(&router, "GET", "/api/oidc/providers", "gw.local", None).await,
            StatusCode::OK,
        );
        assert_no_store(
            "unknown alias",
            &send(
                &router,
                "POST",
                "/api/oidc/nope/device/start",
                "gw.local",
                None,
            )
            .await,
            StatusCode::NOT_FOUND,
        );
        assert_no_store(
            "device start",
            &send(
                &router,
                "POST",
                "/api/oidc/corp/device/start",
                "gw.local",
                None,
            )
            .await,
            StatusCode::OK,
        );
        assert_no_store(
            "granted poll",
            &send(
                &router,
                "POST",
                "/api/oidc/corp/device/poll",
                "gw.local",
                poll_body(),
            )
            .await,
            StatusCode::OK,
        );
        assert_no_store(
            "callback success page",
            &send(
                &router,
                "GET",
                &format!("/oidc/callback?code=auth-1&state={flow_state}"),
                "gw.local",
                None,
            )
            .await,
            StatusCode::OK,
        );
        assert_no_store(
            "callback failure page",
            &send(
                &router,
                "GET",
                "/oidc/callback?code=x&state=never-issued",
                "gw.local",
                None,
            )
            .await,
            StatusCode::BAD_REQUEST,
        );
    }

    #[tokio::test]
    async fn the_callback_refuses_a_response_from_another_issuer() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-browser",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));
        let foreign = query_escape("https://different.example");

        // A matching state carrying a foreign `iss` is a mix-up: refused
        // before the code is exchanged.
        let flow_state = start_login(&router).await;
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}&iss={foreign}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response).await.contains("Sign-in not completed"));
        assert_eq!(requests_to(&server, "/token").await, 0);

        // An IdP error from a foreign issuer is not a verdict on this flow
        // either, and the state is spent regardless.
        let flow_state = start_login(&router).await;
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?error=access_denied&state={flow_state}&iss={foreign}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "the refused response consumed the state"
        );
        assert_eq!(requests_to(&server, "/token").await, 0);

        // The issuer this flow was started for is accepted.
        let flow_state = start_login(&router).await;
        let matching = query_escape(&server.uri());
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}&iss={matching}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("\"at-browser\""));
        assert_eq!(requests_to(&server, "/token").await, 1);
    }

    #[tokio::test]
    async fn the_production_assembly_keeps_every_boundary() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-browser",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let mut config = config_with_alias(&server.uri(), "corp");
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec!["zc_paired".into()];
        let mut state = AppState {
            pairing: Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
                config.gateway.require_pairing,
                &config.gateway.paired_tokens,
                zeroclaw_config::pairing::PairingCodePolicy::default(),
            )),
            ..crate::api::tests::test_state(config.clone())
        };
        state.path_prefix = "/gw".to_string();
        let inbound_auth = Arc::new(
            crate::principal_gate::GatewayInboundAuth::from_config(
                &config,
                Arc::clone(&state.pairing),
            )
            .unwrap(),
        );
        // The real assembly: the authenticated config group and the
        // deliberately unauthenticated enrollment group in one router,
        // nested under the deployment's path prefix, under the gateway
        // body cap and the gateway security headers.
        let app = Router::new()
            .nest(
                "/gw",
                Router::new()
                    .merge(crate::config_admin_router(&inbound_auth))
                    .merge(routes())
                    .with_state(state)
                    .layer(tower_http::limit::RequestBodyLimitLayer::new(
                        crate::MAX_BODY_SIZE,
                    )),
            )
            .layer(axum::middleware::from_fn(crate::security_headers::apply));

        // The config group's route layer still gates config, and does not
        // reach across the merge to the enrollment routes, which have to
        // answer before anyone can enroll a credential.
        let response = send(&app, "GET", "/gw/api/config", "gw.local", None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = send(&app, "GET", "/gw/api/oidc/providers", "gw.local", None).await;
        assert_eq!(response.status(), StatusCode::OK);

        // The redirect_uri the browser is sent with carries the prefix the
        // deployment is nested under.
        let response = send(&app, "GET", "/gw/oidc/login/corp", "gw.local", None).await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        assert!(
            location.contains("%2Fgw%2Foidc%2Fcallback"),
            "redirect_uri keeps the path prefix: {location}"
        );
        let flow_state = state_from_location(&location);

        // The token-bearing page carries both the gateway security headers
        // and this surface's cache policy.
        let response = send(
            &app,
            "GET",
            &format!("/gw/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_value(&response, "cross-origin-opener-policy"),
            "same-origin"
        );
        assert_eq!(header_value(&response, "referrer-policy"), "no-referrer");
        assert_eq!(header_value(&response, "x-frame-options"), "DENY");
        assert_eq!(header_value(&response, "cache-control"), "no-store");
        assert_eq!(header_value(&response, "pragma"), "no-cache");

        // The gateway body cap applies to the enrollment routes too.
        let oversized = "d".repeat(70 * 1024);
        let response = send(
            &app,
            "POST",
            "/gw/api/oidc/corp/device/poll",
            "gw.local",
            Some(serde_json::json!({ "device_code": oversized })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // And a remote identity is still budgeted through the whole stack.
        for attempt in 0..POLL_BUDGET_PER_MINUTE {
            let response = send_as(
                &app,
                remote(),
                "POST",
                "/gw/api/oidc/corp/device/poll",
                "gw.local",
                poll_body(),
                &[],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "poll {attempt}");
        }
        let response = send_as(
            &app,
            remote(),
            "POST",
            "/gw/api/oidc/corp/device/poll",
            "gw.local",
            poll_body(),
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn a_pending_flow_past_its_ttl_is_gone_when_the_callback_arrives() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "never-exchanged",
            })))
            .mount(&server)
            .await;
        // A store whose entries are stale the moment they land: the retain
        // pass that guards insert and consume must drop the pending flow
        // rather than let a late callback finish it.
        let relay = Arc::new(OidcEnrollmentState {
            flows: OidcFlowStore {
                flows: parking_lot::Mutex::new(HashMap::new()),
                ttl: Duration::ZERO,
            },
            ..OidcEnrollmentState::default()
        });
        let router = routes_with(relay).with_state(crate::api::tests::test_state(
            config_with_alias(&server.uri(), "corp"),
        ));

        let flow_state = start_login(&router).await;
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response).await.contains("Sign-in not completed"));
        assert_eq!(requests_to(&server, "/token").await, 0);
    }

    #[tokio::test]
    async fn the_pending_flow_store_is_capped_before_the_idp_is_contacted() {
        let server = idp().await;
        let router = router_for(config_with_alias(&server.uri(), "corp"));
        for attempt in 0..FLOW_CAP {
            let response = send(&router, "GET", "/oidc/login/corp", "gw.local", None).await;
            assert_eq!(
                response.status(),
                StatusCode::TEMPORARY_REDIRECT,
                "login {attempt}"
            );
        }
        let outbound = total_requests(&server).await;

        let response = send(&router, "GET", "/oidc/login/corp", "gw.local", None).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(!header_value(&response, "retry-after").is_empty());
        let json = body_json(response).await;
        assert!(
            json["error"]
                .as_str()
                .unwrap_or_default()
                .contains("too many in-flight"),
            "{json}"
        );
        assert_eq!(
            total_requests(&server).await,
            outbound,
            "a full store refuses without spending a discovery round trip \
             on a flow that would have nowhere to land"
        );
    }

    #[tokio::test]
    async fn removing_the_alias_mid_flow_fails_the_callback_closed() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "never-exchanged",
            })))
            .mount(&server)
            .await;
        let state = crate::api::tests::test_state(config_with_alias(&server.uri(), "corp"));
        let config = state.config.clone();
        let router = routes().with_state(state);

        let flow_state = start_login(&router).await;
        config.write().oidc.clear();

        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response).await.contains("Sign-in not completed"));
        assert_eq!(requests_to(&server, "/token").await, 0);
    }

    #[tokio::test]
    async fn a_callback_refused_for_relay_capacity_can_still_be_retried() {
        let server = idp().await;
        Mock::given(http_method("POST"))
            .and(http_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-after-retry",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let relay = Arc::new(OidcEnrollmentState::default());
        let router = routes_with(Arc::clone(&relay)).with_state(crate::api::tests::test_state(
            config_with_alias(&server.uri(), "corp"),
        ));

        let flow_state = start_login(&router).await;

        // The browser comes back while every outbound slot is taken.
        let permits = relay
            .outbound
            .try_acquire_many(OUTBOUND_RELAY_CAP as u32)
            .unwrap();
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header_value(&response, "retry-after"), "5");
        assert_eq!(requests_to(&server, "/token").await, 0);
        drop(permits);

        // The retry that `Retry-After` invited finishes the sign-in: the
        // refusal spent nothing, so it must not have spent the flow.
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = body_text(response).await;
        assert!(page.contains("\"at-after-retry\""), "{page}");
        assert_eq!(requests_to(&server, "/token").await, 1);

        // Single use survives the round trip: the flow is spent now.
        let response = send(
            &router,
            "GET",
            &format!("/oidc/callback?code=auth-1&state={flow_state}"),
            "gw.local",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(requests_to(&server, "/token").await, 1);
    }

    #[tokio::test]
    async fn one_client_cannot_park_the_shared_flow_store() {
        let server = idp().await;
        let relay = Arc::new(OidcEnrollmentState::default());
        let router = routes_with(Arc::clone(&relay)).with_state(crate::api::tests::test_state(
            config_with_alias(&server.uri(), "corp"),
        ));

        // One caller starts sign-ins and never finishes them. Its share of
        // the store is all it gets: the flows live for the whole TTL, and
        // the request rate that parks them is far below the lockout.
        for round in 0..PER_CLIENT_FLOW_CAP {
            let response = send_as(
                &router,
                remote(),
                "GET",
                "/oidc/login/corp",
                "gw.local",
                None,
                &[],
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::TEMPORARY_REDIRECT,
                "login {round}"
            );
        }
        let response = send_as(
            &router,
            remote(),
            "GET",
            "/oidc/login/corp",
            "gw.local",
            None,
            &[],
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(header_value(&response, "retry-after"), "5");
        assert!(body_text(response).await.contains("in-flight sign-ins"));

        // Everyone else still gets to sign in, which is the point.
        let response = send_as(
            &router,
            other_remote(),
            "GET",
            "/oidc/login/corp",
            "gw.local",
            None,
            &[],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::TEMPORARY_REDIRECT,
            "another client must not pay for the first one's parked flows"
        );
    }

    #[tokio::test]
    async fn a_flow_store_refusal_is_not_billed_to_the_caller() {
        let server = idp().await;
        let relay = Arc::new(OidcEnrollmentState::default());
        let router = routes_with(Arc::clone(&relay)).with_state(crate::api::tests::test_state(
            config_with_alias(&server.uri(), "corp"),
        ));

        // Fill this client's share, then keep asking. The refusals are the
        // gateway's capacity, not failed authentications, so they cost the
        // caller nothing: billing them would let a user who retries a busy
        // surface lock themselves out of enrollment entirely.
        for _ in 0..PER_CLIENT_FLOW_CAP {
            assert!(
                !start_login_as(&router, remote()).await.is_empty(),
                "a login within the share is served"
            );
        }
        for round in 0..5 {
            let response = send_as(
                &router,
                remote(),
                "GET",
                "/oidc/login/corp",
                "gw.local",
                None,
                &[],
            )
            .await;
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS, "{round}");
            assert!(
                body_text(response).await.contains("in-flight sign-ins"),
                "refusal {round} must be the capacity one, not a lockout"
            );
        }
        assert!(!relay.attempts.is_locked_out(&remote_key()));
    }

    #[tokio::test]
    async fn a_restored_flow_keeps_the_deadline_it_started_with() {
        let server = idp().await;
        let relay = Arc::new(OidcEnrollmentState::default());
        let router = routes_with(Arc::clone(&relay)).with_state(crate::api::tests::test_state(
            config_with_alias(&server.uri(), "corp"),
        ));

        // A flow that has spent almost all of its TTL, handed back the way
        // a capacity refusal hands one back. The retry inherits what is
        // left of the original deadline: stamping a fresh `created` here
        // would let a caller hold a slot indefinitely by bouncing off the
        // relay cap once per TTL.
        let nearly_spent = start_login(&router).await;
        let pending = relay.flows.consume(&nearly_spent).expect("flow pending");
        let aged = FLOW_TTL - Duration::from_secs(1);
        relay.flows.restore(PendingPkce {
            created: Instant::now() - aged,
            ..pending
        });
        let back = relay.flows.consume(&nearly_spent).expect("still live");
        assert!(
            back.created.elapsed() >= aged,
            "the restore must not restart the TTL"
        );

        // And a flow whose deadline passed while the callback was in the
        // handler is dropped rather than revived.
        let expired = start_login(&router).await;
        let pending = relay.flows.consume(&expired).expect("flow pending");
        relay.flows.restore(PendingPkce {
            created: Instant::now() - FLOW_TTL - Duration::from_secs(1),
            ..pending
        });
        assert!(relay.flows.consume(&expired).is_none());
    }
}
