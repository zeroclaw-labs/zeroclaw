//! ZeroClaw's `wasi:http` outbound handler for plugin stores.
//!
//! A plugin granted `http_client` gets the `wasi:http` linker, but the linker is
//! not the authority. This module replaces wasmtime's default [`WasiHttpHooks`]
//! with hooks that submit every guest-issued request to the host-owned egress
//! boundary in [`crate::egress`], and then perform the send itself.
//!
//! Three properties carry the security weight, and all three are decided by the
//! shared boundary rather than here:
//!
//! 1. **Deny by default.** A store built without an [`EgressHostService`] denies
//!    every request. The linker still carries `wasi:http` — store construction
//!    is unchanged — but nothing gets out. "Granted `http_client`" and "may
//!    reach the network" are deliberately different states, which is what shuts
//!    the self-grant path: a component that writes `http_client` into its own
//!    unsigned manifest still reaches nothing.
//!
//! 2. **Policy is read per request, never snapshotted.** The service holds a
//!    resolver closure, so an operator's edit to the canonical config takes
//!    effect on the next request without re-instantiating the guest, which is
//!    ADR-012's use-time resolution mode.
//!
//! 3. **The connect is pinned.** [`EgressHostService::authorize`] performs the
//!    one resolution and hands back the exact addresses that passed validation.
//!    This adapter dials those and never resolves the name again, so a DNS
//!    answer cannot change address classes between the check and the connect.
//!    TLS still takes SNI and certificate verification from the *hostname*, so
//!    pinning the connect does not weaken TLS identity.
//!
//! What is deliberately *not* here: the allowlist match, the address-class
//! verdict, NAT64 classification, and the connection budget. Re-deciding any of
//! them in a transport adapter is how a plugin and a built-in tool come to
//! disagree about what is reachable.
//!
//! Redirects are not followed. A guest that wants to chase one issues a second
//! request, and that request is authorized on its own from scratch.

use std::net::SocketAddr;

use hyper::header::HOST;
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};
use wasmtime_wasi_http::p2::{
    WasiHttpHooks,
    bindings::http::types::ErrorCode,
    body::HyperOutgoingBody,
    types::{HostFutureIncomingResponse, IncomingResponse, OutgoingRequestConfig},
};
use zeroclaw_infra::net_guard::NetworkGuardError;

use crate::egress::{
    AuthorizedEgress, EgressError, EgressHostService, EgressRequest, EgressTransport,
};
use crate::instance::{PluginInstanceId, PluginInstanceScope};

/// The masked text a denied guest sees.
///
/// It names the policy and nothing else: no host, no address, no matched or
/// unmatched pattern. A guest must not be able to use denial messages to map the
/// host's internal network.
pub const DENIED_MESSAGE: &str = "zeroclaw plugin egress policy: destination not permitted";

fn denied() -> ErrorCode {
    ErrorCode::InternalError(Some(DENIED_MESSAGE.to_string()))
}

/// A destination that could not be resolved. Distinct from [`denied`] on
/// purpose: a name that does not resolve is not a policy decision, and reporting
/// it as one would tell a guest that every unreachable host is blocked. Mirrors
/// what the default send path reports for the same condition.
fn dns_failure() -> ErrorCode {
    ErrorCode::DnsError(
        wasmtime_wasi_http::p2::bindings::http::types::DnsErrorPayload {
            rcode: Some("address not available".to_string()),
            info_code: Some(0),
        },
    )
}

/// Emit the structured denial event that attributes the attempt to the exact
/// instance. The destination host and the boundary's reason are recorded
/// host-side — the operator needs both to seed a grant — while only the guest's
/// error is masked.
fn record_denial(id: &PluginInstanceId, host: &str, reason: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "plugin": id.package(),
                "capability": format!("{:?}", id.capability()),
                "binding": id.binding(),
                "host": host,
                "reason": reason,
                "error_key": "plugin_egress_denied",
            })),
        "Denied plugin outbound request by egress policy"
    );
}

/// Split a request authority into the one endpoint that must be authorized,
/// dialed, and named on the wire.
///
/// `Authority` is looser than it looks, and its accessors disagree in two ways
/// that would otherwise let a single request describe two different endpoints:
///
/// * `port_u16()` answers `None` both for an absent port and for a port that is
///   present but not a `u16`. `example.com:99999` passes the URI character
///   validation an authority actually gets, so a guest can reach the second
///   case; treating it as the first authorizes and dials the scheme default
///   while the peer is told `example.com:99999`.
/// * `host()` drops userinfo, while `as_str()` — the value that fills the
///   `Host` header — keeps it. `user@example.com` would be authorized and
///   dialed as `example.com` and announced as `user@example.com`.
///
/// Both are refused as malformed URIs, and only a genuinely absent port takes
/// the scheme default. Refusing userinfo outright is the tighter of the two
/// sound contracts and gives up nothing real: userinfo is deprecated in
/// `http`/`https` URIs (RFC 3986 §3.2.1), a `Host` header may not carry it
/// (RFC 9110 §7.2), and wasmtime's own default hooks already fail these
/// requests — they hand the whole authority, userinfo and all, to
/// `TcpStream::connect`, which cannot resolve it. This makes that accidental
/// failure explicit rather than loosening it.
fn authority_endpoint(
    authority: &hyper::http::uri::Authority,
    use_tls: bool,
) -> Result<(String, u16), ErrorCode> {
    let text = authority.as_str();
    if text.contains('@') {
        return Err(ErrorCode::HttpRequestUriInvalid);
    }
    // Whatever `host()` did not claim is the port, separator included.
    let host = authority.host();
    // A trailing root dot (`api.example.`) is kept by `as_str()` — the value
    // that fills the `Host` header — but stripped by request-host
    // normalization, which drives both policy matching and resolution. The two
    // ends would then name different endpoints (and the absolute dotted form
    // also suppresses DNS search-list expansion that the stripped form
    // re-enables), so refuse it outright rather than reconcile the divergence.
    // Config entries reject the same trailing dot in `normalize_egress_pattern`
    // ("not in canonical form"), so neither side can carry one.
    if host.ends_with('.') {
        return Err(ErrorCode::HttpRequestUriInvalid);
    }
    let Some(rest) = text.strip_prefix(host) else {
        return Err(ErrorCode::HttpRequestUriInvalid);
    };
    let port = match rest.strip_prefix(':') {
        Some(digits) => digits
            .parse::<u16>()
            .map_err(|_| ErrorCode::HttpRequestUriInvalid)?,
        None if rest.is_empty() => {
            if use_tls {
                443
            } else {
                80
            }
        }
        None => return Err(ErrorCode::HttpRequestUriInvalid),
    };
    Ok((host.to_string(), port))
}

/// ZeroClaw's replacement for wasmtime's default `wasi:http` hooks.
///
/// One per plugin store. `egress: None` is the deny-by-default state.
pub(crate) struct PluginEgressHooks {
    scope: PluginInstanceScope,
    egress: Option<EgressHostService>,
}

impl PluginEgressHooks {
    pub(crate) fn new(scope: PluginInstanceScope, egress: Option<EgressHostService>) -> Self {
        Self { scope, egress }
    }
}

impl WasiHttpHooks for PluginEgressHooks {
    fn send_request(
        &mut self,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> wasmtime_wasi_http::p2::HttpResult<HostFutureIncomingResponse> {
        let Some(authority) = request.uri().authority().cloned() else {
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(
                ErrorCode::HttpRequestUriInvalid,
            ))));
        };
        // One endpoint for the grant check, the dial, and the `Host` header.
        // An authority that cannot name exactly one is a malformed URI, and is
        // reported as such rather than as a policy denial: nothing about the
        // host's network is disclosed by telling a guest its own URI is bad.
        let (host, port) = match authority_endpoint(&authority, config.use_tls) {
            Ok(endpoint) => endpoint,
            Err(error) => return Ok(HostFutureIncomingResponse::ready(Ok(Err(error)))),
        };

        // Deny by default, before anything is spawned and before any name is
        // looked up: a store that links `wasi:http` without a host-owned egress
        // service reaches nothing.
        let Some(service) = self.egress.clone() else {
            record_denial(
                self.scope.id(),
                &host,
                "no egress policy granted for this instance",
            );
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(denied()))));
        };

        // `encrypted` is the confidentiality mode, not a second permission axis:
        // the operator's grant covers a host, and plain HTTP to a granted host
        // is permitted. The transport is what selects the effective grant the
        // boundary re-checks.
        let egress_request = match EgressRequest::new(
            self.scope.clone(),
            EgressTransport::Http {
                encrypted: config.use_tls,
            },
            &host,
            port,
        ) {
            Ok(request) => request,
            // A malformed destination never becomes a request, so it never
            // reaches DNS. The guest still sees only the masked denial.
            Err(error) => {
                record_denial(self.scope.id(), &host, &error.to_string());
                return Ok(HostFutureIncomingResponse::ready(Ok(Err(denied()))));
            }
        };

        let id = self.scope.id().clone();
        let handle = wasmtime_wasi::runtime::spawn(async move {
            Ok(send(request, config, egress_request, service, id).await)
        });
        Ok(HostFutureIncomingResponse::pending(handle))
    }
}

/// The host-owned pinned send path.
///
/// This mirrors the mechanics of `wasmtime_wasi_http::p2::default_send_request_handler`
/// — same `Host` header fill-in, same first-byte and between-bytes timeouts,
/// same origin-form URI rewrite before `send_request`, same hyper http1
/// handshake and worker task — with two deliberate differences.
///
/// The first is the point of the module: its single `TcpStream::connect(authority)`
/// (which resolves and connects in one unobservable step) becomes **authorize,
/// then connect to an address the boundary already validated**.
///
/// The second is the connect budget. The default handler applies
/// `connect_timeout` per stage and leaves the TLS negotiation with no bound at
/// all. Here one deadline covers authorization, the dial, TLS, and the HTTP
/// handshake, because a host that leases a scarce connection slot to a guest
/// cannot let the peer decide how long to hold it.
async fn send(
    mut request: hyper::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
    egress_request: EgressRequest,
    service: EgressHostService,
    id: PluginInstanceId,
) -> Result<IncomingResponse, ErrorCode> {
    use http_body_util::BodyExt;

    // The authority is safe to name on the wire because [`authority_endpoint`]
    // already refused every form whose `as_str()` describes something other
    // than the endpoint this request was authorized for and will be dialed at.
    // A `Host` header the guest set itself is left alone, exactly as the
    // default send path leaves it: the grant, the pin, and TLS identity all
    // come from the URI regardless of what the guest claims here.
    if !request.headers().contains_key(HOST)
        && let Some(authority) = request.uri().authority()
        && let Ok(value) = hyper::header::HeaderValue::from_str(authority.as_str())
    {
        request.headers_mut().insert(HOST, value);
    }

    // ── one deadline for the whole connect ───────────────────────
    // Authorization, the pinned dial, the TLS negotiation, and the HTTP
    // handshake are stages of a single connect, so they are charged against a
    // single budget, fixed here before any of them starts.
    //
    // Two failures follow from staging the budget per phase instead. A request
    // that fails late spends the operator's `connect_timeout` once per stage,
    // so the total is a multiple of what was configured. And any stage left
    // unwrapped is unbounded: a peer that accepts TCP and then says nothing
    // holds the guest's request open, and with it this instance's connection
    // slot, for as long as it cares to.
    //
    // Expiry at any stage fails closed as `ConnectionTimeout`. The slot comes
    // back on its own: the authorized token owns the lease and is a local of
    // this future until the connection worker takes it, so returning early —
    // or the guest dropping the request, which aborts this future — drops the
    // token and releases the lease.
    //
    // `connect_timeout` is the guest's number — `wasi:http`'s
    // `request-options.set-connect-timeout` reaches this field unclamped — so
    // the deadline is computed with `checked_add` rather than `+`. A duration
    // the monotonic clock cannot represent is not a budget the host can
    // enforce, and it must not become a panic in a host function on a guest's
    // say-so. A nonsense timeout gets the timeout error it asked for, closed
    // and immediate.
    let Some(deadline) = Instant::now().checked_add(config.connect_timeout) else {
        return Err(ErrorCode::ConnectionTimeout);
    };

    // ── authorize ────────────────────────────────────────────────
    // The shared boundary checks the effective grant and the operator's
    // allowlist *before* it resolves anything, then performs the one resolution
    // and pins what it validated. The deadline bounds how long the *guest* waits
    // here and how long this future can hold a connection lease — not the
    // resolver's own work: `tokio::net::lookup_host` runs the platform
    // `getaddrinfo` on a `spawn_blocking` thread that cannot be cancelled, so on
    // expiry the guest is released with `ConnectionTimeout` while that blocking
    // lookup keeps running to its own OS-level completion, bounded by the OS
    // resolver timeout rather than by this deadline. No egress lease leaks: the
    // lease is acquired only after resolution succeeds, so the cost of a wedged
    // resolver is a blocking thread, not a held slot.
    //
    // The requested host is scoped to this block on purpose: past it the only
    // host in hand is the pin's, so there is nothing left to resolve a second
    // time.
    let authorized = {
        let host = egress_request.host().to_string();
        match timeout_at(deadline, service.authorize(egress_request)).await {
            Ok(Ok(authorized)) => authorized,
            Ok(Err(error)) => {
                record_denial(&id, &host, &error.to_string());
                return Err(match error {
                    EgressError::DnsFailed { .. }
                    | EgressError::Network(NetworkGuardError::NoAddresses { .. }) => dns_failure(),
                    _ => denied(),
                });
            }
            Err(_) => return Err(ErrorCode::ConnectionTimeout),
        }
    };

    // ── connect (pinned) ─────────────────────────────────────────
    // Canonical host from the pin, used for SNI and certificate verification —
    // never for a second resolution.
    let server_name = authorized.destination().host().to_string();
    let tcp_stream = dial_pinned(authorized.destination().addresses(), deadline).await?;

    let (mut sender, worker) = if config.use_tls {
        use rustls::pki_types::ServerName;
        use wasmtime_wasi_http::io::TokioIo;

        // Trust is the bundled webpki root program and nothing else: this path
        // deliberately does not read the operating system's trust store, so an
        // enterprise or private CA installed on the host does not silently
        // become trusted for plugin egress. Native-root support is a separate,
        // tracked decision rather than an omission.
        let root_cert_store = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.into(),
        };
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(tls_config));
        let domain = ServerName::try_from(server_name).map_err(|_| ErrorCode::TlsProtocolError)?;
        // The stage that most needs the deadline: the TCP connect has already
        // succeeded, so a peer that never sends a `ServerHello` is indistinguishable
        // from a slow one and would otherwise stall here without limit.
        let stream = timeout_at(deadline, connector.connect(domain, tcp_stream))
            .await
            .map_err(|_| ErrorCode::ConnectionTimeout)?
            .map_err(|_| ErrorCode::TlsProtocolError)?;
        handshake(TokioIo::new(stream), deadline, authorized).await?
    } else {
        use wasmtime_wasi_http::io::TokioIo;
        handshake(TokioIo::new(tcp_stream), deadline, authorized).await?
    };

    // hyper's `SendRequest` does not strip scheme/authority, and an origin
    // server must receive origin-form; same rewrite the default path does.
    *request.uri_mut() = hyper::Uri::builder()
        .path_and_query(
            request
                .uri()
                .path_and_query()
                .map_or("/", |p| p.as_str())
                .to_string(),
        )
        .build()
        .map_err(|_| ErrorCode::HttpRequestUriInvalid)?;

    let resp = timeout(config.first_byte_timeout, sender.send_request(request))
        .await
        .map_err(|_| ErrorCode::ConnectionReadTimeout)?
        .map_err(|_| ErrorCode::HttpProtocolError)?
        .map(|body| {
            body.map_err(|_| ErrorCode::HttpProtocolError)
                .boxed_unsync()
        });

    Ok(IncomingResponse {
        resp,
        worker: Some(worker),
        between_bytes_timeout: config.between_bytes_timeout,
    })
}

/// Dial the pinned addresses in order and return the first connection that
/// comes up, under the shared connect deadline.
///
/// Connect by `SocketAddr`, never by name: every candidate comes out of the one
/// [`crate::egress::AuthorizedEgress`] the boundary validated, so moving to the
/// next address after a refusal is failover within an already-checked set, not a
/// second resolution that could substitute a different address class.
///
/// The deadline belongs to the connect as a whole rather than to each attempt.
/// A destination whose first address is a blackhole therefore spends the budget
/// the later addresses would have used instead of extending it, which is what
/// keeps a multi-address answer from multiplying the operator's timeout.
async fn dial_pinned(addresses: &[SocketAddr], deadline: Instant) -> Result<TcpStream, ErrorCode> {
    let mut attempted = false;
    for address in addresses {
        attempted = true;
        match timeout_at(deadline, TcpStream::connect(*address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            // Refused or unreachable: try the next validated address.
            Ok(Err(_)) => {}
            Err(_) => return Err(ErrorCode::ConnectionTimeout),
        }
    }
    if attempted {
        Err(ErrorCode::ConnectionRefused)
    } else {
        // `ResolvedDestination` is never empty, so an empty candidate set is an
        // unreachable state rather than a policy decision.
        Err(dns_failure())
    }
}

/// Drive one hyper http1 handshake and spawn its connection worker.
///
/// `authorized` travels into the worker rather than being dropped here: the
/// token holds this instance's connection slot, and the slot has to last as long
/// as the connection it paid for, not as long as [`send`].
async fn handshake<S>(
    stream: S,
    deadline: Instant,
    authorized: AuthorizedEgress,
) -> Result<
    (
        hyper::client::conn::http1::SendRequest<HyperOutgoingBody>,
        wasmtime_wasi::runtime::AbortOnDropJoinHandle<()>,
    ),
    ErrorCode,
>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (sender, conn) = timeout_at(deadline, hyper::client::conn::http1::handshake(stream))
        .await
        .map_err(|_| ErrorCode::ConnectionTimeout)?
        .map_err(|_| ErrorCode::HttpProtocolError)?;

    let worker = wasmtime_wasi::runtime::spawn(async move {
        let outcome = conn.await;
        drop(authorized);
        if let Err(error) = outcome {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{error}") })),
                "plugin egress connection ended with an error"
            );
        }
    });

    Ok((sender, worker))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use http_body_util::BodyExt;

    use super::*;
    use crate::egress::{EgressPolicy, EgressPolicyResolver};
    use crate::{PluginCapability, PluginPermission};

    fn hooks(egress: Option<EgressHostService>) -> PluginEgressHooks {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        PluginEgressHooks::new(scope, egress)
    }

    fn request(uri: &str) -> hyper::Request<HyperOutgoingBody> {
        let body = http_body_util::Empty::<hyper::body::Bytes>::new()
            .map_err(|_| unreachable!("an empty body cannot fail"))
            .boxed_unsync();
        hyper::Request::builder()
            .uri(uri)
            .body(body)
            .expect("valid fixture request")
    }

    fn config() -> OutgoingRequestConfig {
        OutgoingRequestConfig {
            use_tls: false,
            connect_timeout: Duration::from_secs(1),
            first_byte_timeout: Duration::from_secs(1),
            between_bytes_timeout: Duration::from_secs(1),
        }
    }

    fn denial(response: HostFutureIncomingResponse) -> ErrorCode {
        match response {
            HostFutureIncomingResponse::Ready(Ok(Err(code))) => code,
            other => panic!("expected a synchronous denial, got: {other:?}"),
        }
    }

    /// Deny-by-default is answered synchronously, before a task is spawned and
    /// before the host looks anything up. The e2e proves no packet leaves; this
    /// proves the refusal does not even reach the async send path.
    #[test]
    fn a_store_without_an_egress_service_denies_without_spawning() {
        let mut hooks = hooks(None);
        for uri in [
            "http://example.com/",
            "http://127.0.0.1:9/",
            "http://api.internal/",
        ] {
            let response = hooks
                .send_request(request(uri), config())
                .expect("a denial is a guest-visible error, never a trap");
            assert!(
                matches!(denial(response), ErrorCode::InternalError(Some(message)) if message == DENIED_MESSAGE),
                "{uri} must be denied without a granted policy"
            );
        }
    }

    /// A destination the shared boundary cannot even accept as a request host is
    /// refused on the same masked path, not reported as a distinct condition.
    #[test]
    fn a_malformed_request_host_is_denied_without_resolving() {
        let service =
            EgressHostService::with_private_connection_accounting(EgressPolicyResolver::new(
                |_| EgressPolicy::new(&["example.com".to_string()], &[], &[], 4),
            ));
        let mut hooks = hooks(Some(service));
        let response = hooks
            .send_request(request("http://exa_mple.com/"), config())
            .expect("a denial is a guest-visible error, never a trap");
        assert!(matches!(
            denial(response),
            ErrorCode::InternalError(Some(message)) if message == DENIED_MESSAGE
        ));
    }

    fn authority(text: &str) -> hyper::http::uri::Authority {
        text.parse().expect("a valid fixture authority")
    }

    /// The endpoint contract: one authority names exactly one host and port, or
    /// it is not a usable authority.
    ///
    /// The port cases are the reason this helper exists. `port_u16()` cannot
    /// tell "no port" from "a port that is not a `u16`", and the URI validation
    /// an authority actually receives lets the second through, so the old
    /// `unwrap_or(default)` authorized and dialed the scheme default for an
    /// authority the peer would be told was something else.
    #[test]
    fn an_authority_names_one_endpoint_or_none() {
        assert_eq!(
            authority_endpoint(&authority("example.com:8443"), false).unwrap(),
            ("example.com".to_string(), 8443),
            "an explicit in-range port is honoured over the scheme default"
        );
        assert_eq!(
            authority_endpoint(&authority("example.com"), false).unwrap(),
            ("example.com".to_string(), 80)
        );
        assert_eq!(
            authority_endpoint(&authority("example.com"), true).unwrap(),
            ("example.com".to_string(), 443),
            "only an absent port takes the scheme default"
        );
        assert_eq!(
            authority_endpoint(&authority("[::1]:8443"), false).unwrap(),
            ("[::1]".to_string(), 8443),
            "an IPv6 literal keeps its brackets and loses only the port"
        );
        assert_eq!(
            authority_endpoint(&authority("[::1]"), true).unwrap(),
            ("[::1]".to_string(), 443),
            "the colons inside an IPv6 literal are not an explicit port"
        );

        for rejected in [
            // Out of `u16` range, and accepted by authority validation.
            "example.com:99999",
            "example.com:65536",
            // Present but not numeric, and present but empty.
            "example.com:http",
            "example.com:",
            // Userinfo: `host()` drops it, `as_str()` keeps it, so the
            // authorized endpoint and the wire `Host` value would disagree.
            "user@example.com",
            "user:pass@example.com:8443",
            // Trailing root dot: `as_str()`/`host()` keep it for the wire `Host`
            // header, while request-host normalization strips it for policy and
            // resolution, so the two ends would name different endpoints.
            "api.example.",
            "api.example.:8443",
        ] {
            assert!(
                matches!(
                    authority_endpoint(&authority(rejected), false),
                    Err(ErrorCode::HttpRequestUriInvalid)
                ),
                "{rejected:?} names no single endpoint and must be refused"
            );
        }

        // The same host without the trailing dot is a single well-formed
        // endpoint, so the rejection turns on the dot alone.
        assert_eq!(
            authority_endpoint(&authority("api.example"), false).unwrap(),
            ("api.example".to_string(), 80)
        );
    }

    /// Through the hook the guest actually calls: a trailing-dot authority is
    /// refused as a malformed URI rather than authorized under its dot-stripped
    /// form while the wire `Host` header still carries the dot.
    #[test]
    fn a_trailing_dot_authority_is_refused_as_malformed() {
        let service =
            EgressHostService::with_private_connection_accounting(EgressPolicyResolver::new(
                // The dot-stripped host is granted, so a regression that let the
                // dot through would reach authorization rather than fail.
                |_| EgressPolicy::new(&["api.example".to_string()], &[], &[], 4),
            ));
        let mut hooks = hooks(Some(service));
        let response = hooks
            .send_request(request("http://api.example./"), config())
            .expect("a malformed URI is a guest-visible error, never a trap");
        assert!(
            matches!(denial(response), ErrorCode::HttpRequestUriInvalid),
            "a trailing-dot authority must be refused as a malformed URI"
        );
    }

    /// The same contract through the hook the guest actually calls: an
    /// out-of-range port is refused outright rather than quietly redirected to
    /// the scheme default, and the refusal is a URI error rather than a policy
    /// denial — the guest's own URI is the only thing it discloses.
    #[test]
    fn an_out_of_range_port_is_refused_instead_of_defaulted() {
        let service =
            EgressHostService::with_private_connection_accounting(EgressPolicyResolver::new(
                // The scheme default this would have fallen back to is granted,
                // so a regression here reaches the network rather than failing.
                |_| EgressPolicy::new(&["example.com".to_string()], &[], &[], 4),
            ));
        let mut hooks = hooks(Some(service));
        for uri in [
            "http://example.com:99999/",
            "http://user@example.com/",
            "http://user:pass@example.com:8443/",
        ] {
            let response = hooks
                .send_request(request(uri), config())
                .expect("a malformed URI is a guest-visible error, never a trap");
            assert!(
                matches!(denial(response), ErrorCode::HttpRequestUriInvalid),
                "{uri} must be refused as a malformed URI"
            );
        }
    }

    /// A loopback grant with the private carveout and a one-connection ceiling.
    /// The tight ceiling is the point: a slot that leaks on a failed dial makes
    /// the next authorization fail, so the budget is observable.
    ///
    /// Accounting is private to the caller. Connection counts are otherwise
    /// process-wide, and a one-slot ceiling shared with whatever else the test
    /// binary is running concurrently would not be observable at all.
    fn loopback_service() -> EgressHostService {
        EgressHostService::with_private_connection_accounting(EgressPolicyResolver::new(|_| {
            EgressPolicy::new(
                &["127.0.0.1".to_string()],
                &["127.0.0.1".to_string()],
                &[],
                1,
            )
        }))
    }

    /// The connect budget arrives from the guest: `wasi:http`'s
    /// `request-options.set-connect-timeout` reaches `connect_timeout`
    /// unclamped, and `WasiHttpHooks::send_request` accepts whatever
    /// `OutgoingRequestConfig` it is handed. A duration the monotonic clock
    /// cannot represent must therefore fail closed here rather than panic
    /// inside a host function.
    ///
    /// The guest's own ceiling is `Duration::from_nanos(u64::MAX)` — about 585
    /// years, which today's targets *can* represent — so this is a defense of
    /// the hook boundary and of platforms whose clock epoch leaves less room,
    /// not a reproduction of a wasm module that panics the host on demand.
    #[tokio::test]
    async fn an_unrepresentable_connect_budget_fails_closed_instead_of_panicking() {
        let mut hooks = hooks(Some(loopback_service()));
        let config = OutgoingRequestConfig {
            use_tls: false,
            connect_timeout: Duration::MAX,
            first_byte_timeout: Duration::from_secs(1),
            between_bytes_timeout: Duration::from_secs(1),
        };

        let response = hooks
            .send_request(request("http://127.0.0.1:1/"), config)
            .expect("a nonsense timeout is a guest-visible error, never a trap");
        let HostFutureIncomingResponse::Pending(handle) = response else {
            panic!("a granted destination is dialed asynchronously");
        };
        let outcome = handle.await.expect("the send task must not trap");
        assert!(
            matches!(outcome, Err(ErrorCode::ConnectionTimeout)),
            "an unrepresentable connect budget must fail closed, got: {outcome:?}"
        );
    }

    /// A peer that completes the TCP handshake and then sends nothing.
    ///
    /// Deliberately raw `std::net` on its own thread: what this proves is what
    /// an open, silent socket does to the connect path, and a server framework
    /// would only add machinery that might answer.
    fn stalled_peer() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback peer");
        let port = listener.local_addr().expect("loopback port").port();
        std::thread::spawn(move || {
            let Ok((_stream, _)) = listener.accept() else {
                return;
            };
            // Hold the accepted connection open and silent: a client waiting on
            // a `ServerHello` here waits for as long as the host lets it.
            std::thread::sleep(Duration::from_secs(60));
        });
        port
    }

    /// The whole connect path shares one deadline.
    ///
    /// A peer that accepts TCP and then stalls the TLS negotiation must not be
    /// able to hang the guest's request, and must not keep the instance's
    /// connection slot while it does. The TLS stage carried no timeout of its
    /// own, so this request had no bound at all before the shared deadline.
    #[tokio::test]
    async fn a_stalled_tls_peer_times_out_within_one_connect_budget() {
        let port = stalled_peer();
        let service = loopback_service();
        let mut hooks = hooks(Some(service.clone()));
        let instance = hooks.scope.id().clone();
        let budget = Duration::from_millis(250);
        let config = OutgoingRequestConfig {
            use_tls: true,
            connect_timeout: budget,
            first_byte_timeout: Duration::from_secs(1),
            between_bytes_timeout: Duration::from_secs(1),
        };

        let started = std::time::Instant::now();
        let response = hooks
            .send_request(request(&format!("https://127.0.0.1:{port}/")), config)
            .expect("a stalled peer is a guest-visible error, never a trap");
        let HostFutureIncomingResponse::Pending(handle) = response else {
            panic!("an authorized destination is dialed asynchronously");
        };
        let outcome = handle.await.expect("the send task must not trap");
        let elapsed = started.elapsed();

        assert!(
            matches!(outcome, Err(ErrorCode::ConnectionTimeout)),
            "a stalled TLS negotiation must fail closed as a connect timeout, got: {outcome:?}"
        );
        // Generous enough that a loaded runner cannot flake it, and far tighter
        // than an unbounded stage, which never returns at all.
        assert!(
            elapsed < Duration::from_secs(5),
            "the connect path must not outlive its budget; took {elapsed:?}"
        );
        // Budget composition: authorization, the pinned dial, and the TLS
        // negotiation are charged against one `connect_timeout`, so a failure
        // that crosses several stages costs about one budget, not one each.
        assert!(
            elapsed < 4 * budget,
            "staged budgets would compound; took {elapsed:?} against a {budget:?} budget"
        );
        assert_eq!(
            service.live_connections(&instance),
            0,
            "a timed-out connect must return its slot to the shared budget"
        );
        // The returned slot must also be usable: the ceiling here is one.
        let next = EgressRequest::new(
            hooks.scope.clone(),
            EgressTransport::Http { encrypted: true },
            "127.0.0.1",
            port,
        )
        .expect("a loopback destination is a valid request");
        assert!(
            service
                .authorize_addresses(next, [SocketAddr::from(([127, 0, 0, 1], port))])
                .is_ok(),
            "the instance must be able to dial again after a timed-out connect"
        );
    }

    /// A bound port nobody is listening on any more. Loopback refuses these
    /// immediately, which is what makes the failover case deterministic rather
    /// than dependent on a network that might blackhole instead.
    fn refusing_address() -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback peer");
        let address = listener.local_addr().expect("loopback address");
        drop(listener);
        address
    }

    /// The pin can carry several addresses, and one of them being dead is not a
    /// reason to fail the request: the old unpinned path got that failover from
    /// `TcpStream::connect(authority)` for free.
    ///
    /// Every candidate here comes from one validated destination, so this is
    /// failover inside an already-checked set — the security property is that no
    /// address outside the pin is ever dialed, not that only the first is.
    #[tokio::test]
    async fn the_pinned_dial_fails_over_to_the_next_validated_address() {
        let refusing = refusing_address();
        // Bound and never accepted from: the kernel completes the handshake out
        // of the backlog, which is all a dial needs to succeed.
        let live = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback peer");
        let live_address = live.local_addr().expect("loopback address");
        let deadline = Instant::now() + Duration::from_secs(5);

        let stream = dial_pinned(&[refusing, live_address], deadline)
            .await
            .expect("a dead first address must not fail the whole connect");
        assert_eq!(
            stream.peer_addr().expect("connected peer"),
            live_address,
            "failover must land on the next validated address"
        );

        assert!(
            matches!(
                dial_pinned(&[refusing], deadline).await,
                Err(ErrorCode::ConnectionRefused)
            ),
            "an exhausted candidate list is a refused connect"
        );
        assert!(
            matches!(
                dial_pinned(&[], deadline).await,
                Err(ErrorCode::DnsError(_))
            ),
            "an empty pin is reported as an unresolvable destination"
        );
    }

    /// The dial is bounded by the shared deadline, not by a fresh timeout per
    /// address: a pin full of stalled addresses cannot buy more time than one.
    #[tokio::test]
    async fn an_expired_deadline_stops_the_dial_before_it_tries_an_address() {
        let live = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback peer");
        let live_address = live.local_addr().expect("loopback address");
        let expired = Instant::now() - Duration::from_millis(1);

        assert!(
            matches!(
                dial_pinned(&[live_address], expired).await,
                Err(ErrorCode::ConnectionTimeout)
            ),
            "a spent budget must not fund another attempt"
        );
    }

    /// A live loopback listener that speaks just enough HTTP/1.1 to let a hook
    /// send complete, counting every connection it accepts.
    ///
    /// Deliberately raw `std::net` on its own thread, like the e2e server: this
    /// proves which endpoint the host's client actually dialed, so the listener
    /// adds no framework machinery that might mask the answer.
    fn counting_http_listener() -> (SocketAddr, Arc<AtomicUsize>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback peer");
        let address = listener.local_addr().expect("loopback address");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                // Read just past the request head; the fixture request has no body.
                let mut buf = [0_u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                let _ = stream.flush();
            }
        });
        (address, hits)
    }

    /// The pin holds across a changing resolver: the hook dials only the address
    /// set validated during authorization, never a later resolution.
    ///
    /// This is the crown-jewel property of the pinned send path, exercised at
    /// the real `send_request` boundary rather than at `dial_pinned` in
    /// isolation. `authorize` performs the one resolution and pins listener A;
    /// the resolver is then armed to answer with listener B on any *second*
    /// call. A regression that re-resolved before connecting — the DNS-rebinding
    /// / TOCTOU window pinning closes — would consult that second answer and land
    /// on B.
    ///
    /// Both listeners are loopback, so the address-class verdict treats them
    /// identically: the class check cannot be what keeps the connection off B.
    /// The *only* thing that can is the pin, so B asserts it is never dialed.
    /// The request port matches A's port so the pinned answer passes the
    /// resolved-address port check, and the host is a public name so nothing but
    /// the pin selects which loopback endpoint is reached.
    #[tokio::test]
    async fn the_hook_dials_only_the_pinned_answer_not_a_later_resolution() {
        let (pinned, pinned_hits) = counting_http_listener();
        let (rebind, rebind_hits) = counting_http_listener();

        // A resolver whose answer changes between calls: the first resolution —
        // the one `authorize` pins — is A; any later resolution is B. The switch
        // is a deterministic counter, so nothing depends on DNS or address order.
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = move |_host: &str, _port: u16| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![pinned]
            } else {
                vec![rebind]
            }
        };
        let service = EgressHostService::with_test_resolver(
            EgressPolicyResolver::new(|_| {
                // The public name is granted, and the private carveout is what
                // lets its (loopback) resolved address pass the class check —
                // the same carveout every loopback destination here needs.
                EgressPolicy::new(
                    &["rebind.example.com".to_string()],
                    &["rebind.example.com".to_string()],
                    &[],
                    4,
                )
            }),
            resolver,
        );
        let mut hooks = hooks(Some(service));

        let response = hooks
            .send_request(
                request(&format!("http://rebind.example.com:{}/", pinned.port())),
                config(),
            )
            .expect("an authorized destination is dialed asynchronously");
        let HostFutureIncomingResponse::Pending(handle) = response else {
            panic!("an authorized destination is dialed asynchronously");
        };
        let outcome = handle.await.expect("the send task must not trap");

        assert!(
            outcome.is_ok(),
            "the pinned destination must be reachable; got: {outcome:?}"
        );
        assert_eq!(
            pinned_hits.load(Ordering::SeqCst),
            1,
            "the connection must land on the pinned answer"
        );
        assert_eq!(
            rebind_hits.load(Ordering::SeqCst),
            0,
            "a re-resolved answer must never be dialed: the pin is the only \
             address set the hook may use"
        );
    }

    #[test]
    fn denial_message_names_the_policy_and_leaks_no_host() {
        let ErrorCode::InternalError(Some(message)) = denied() else {
            panic!("denial must carry a message");
        };
        assert!(message.contains("egress policy"), "{message}");
        assert!(!message.contains("127.0.0.1"), "{message}");
    }
}
