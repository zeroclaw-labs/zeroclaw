//! Host-owned egress policy for plugin instances (ADR-013).
//!
//! A plugin granted `http_client` gets the `wasi:http` linker, but the linker
//! is not the authority. This module is: it replaces wasmtime's default
//! `WasiHttpHooks` with hooks that evaluate a host-owned policy on every
//! guest-issued request, and then perform the send itself.
//!
//! Three properties carry the security weight:
//!
//! 1. **Deny by default.** A store built without a policy denies every request.
//!    The linker still carries `wasi:http` — store construction is unchanged —
//!    but nothing gets out. "Granted `http_client`" and "may reach the network"
//!    are deliberately different states, which is what closes the #9395
//!    self-grant path: a component that writes `http_client` into its own
//!    unsigned manifest still reaches nothing.
//!
//! 2. **Policy is read per request, never snapshotted.** [`EgressPolicy`] holds
//!    a resolver closure, not a value. An operator's edit to the canonical
//!    config takes effect on the next request without re-instantiating the
//!    guest, which is ADR-012's use-time resolution mode.
//!
//! 3. **The connect is pinned.** [`send`] resolves the destination itself,
//!    validates every resolved address through the shared guard, and then
//!    connects to an address it already validated. A DNS answer therefore
//!    cannot change address classes between the check and the connect, and TLS
//!    still uses hostname-based SNI so pinning does not weaken certificate
//!    verification.
//!
//! Redirects are deliberately not followed here. A guest that wants to chase
//! one issues a second request, and that request passes the full policy on its
//! own.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use crate::instance::PluginInstanceId;

use hyper::header::HOST;
use wasmtime_wasi_http::p2::{
    WasiHttpHooks,
    bindings::http::types::ErrorCode,
    body::HyperOutgoingBody,
    types::{HostFutureIncomingResponse, IncomingResponse, OutgoingRequestConfig},
};

/// What the policy knows about one instance at the moment a request is made.
///
/// Both lists are expected to already be canonical
/// [`zeroclaw_infra::net_guard::normalize_egress_pattern`] output; the config
/// and manifest surfaces validate before a value ever reaches here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EgressDecisionInput {
    /// The operator's granted allowlist for this instance. Empty means no reach.
    pub hosts: Vec<String>,
    /// Per-destination carveout letting a granted host resolve to a private,
    /// loopback, or link-local address. Mirrors the tool layer's
    /// `allowed_private_hosts`. It never widens `hosts`, and it never re-opens
    /// cloud metadata addresses.
    pub allow_private: Vec<String>,
}

impl EgressDecisionInput {
    /// A policy input that grants nothing.
    #[must_use]
    pub fn deny_all() -> Self {
        Self::default()
    }
}

/// Resolves the effective policy for an instance at the moment of a request.
pub type EgressResolver = Arc<dyn Fn(&PluginInstanceId) -> EgressDecisionInput + Send + Sync>;

/// A per-store handle to the host's egress authority.
///
/// This deliberately holds a *resolver*, not a resolved allowlist: the
/// long-lived enforcement service must never snapshot policy into instance
/// state (ADR-013), so the value is recomputed on every request.
#[derive(Clone)]
pub struct EgressPolicy {
    resolver: EgressResolver,
}

impl std::fmt::Debug for EgressPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressPolicy").finish_non_exhaustive()
    }
}

impl EgressPolicy {
    /// Build a policy that consults `resolver` on every request.
    #[must_use]
    pub fn from_resolver(resolver: EgressResolver) -> Self {
        Self { resolver }
    }

    /// A policy that always returns the same decision input.
    ///
    /// Intended for tests and one-shot processes that genuinely have no live
    /// config to re-read. Long-lived services must use [`Self::from_resolver`].
    #[must_use]
    pub fn fixed(input: EgressDecisionInput) -> Self {
        Self::from_resolver(Arc::new(move |_| input.clone()))
    }

    /// The effective policy for `id`, right now.
    #[must_use]
    pub fn resolve(&self, id: &PluginInstanceId) -> EgressDecisionInput {
        (self.resolver)(id)
    }
}

/// The masked text a denied guest sees.
///
/// It names the policy and nothing else: no host, no address, no matched or
/// unmatched pattern. A guest must not be able to use denial messages to map
/// the host's internal network.
pub const DENIED_MESSAGE: &str = "zeroclaw plugin egress policy: destination not permitted";

fn denied() -> ErrorCode {
    ErrorCode::InternalError(Some(DENIED_MESSAGE.to_string()))
}

/// A destination that could not be resolved. Distinct from [`denied`] on
/// purpose: a name that does not resolve is not a policy decision, and
/// reporting it as one would tell a guest that every unreachable host is
/// blocked. Mirrors what the default send path reports for the same condition.
fn dns_failure() -> ErrorCode {
    ErrorCode::DnsError(
        wasmtime_wasi_http::p2::bindings::http::types::DnsErrorPayload {
            rcode: Some("address not available".to_string()),
            info_code: Some(0),
        },
    )
}

/// ZeroClaw's replacement for wasmtime's default `wasi:http` hooks.
///
/// One per plugin store. `policy: None` is the deny-by-default state.
pub(crate) struct PluginEgressHooks {
    id: PluginInstanceId,
    policy: Option<EgressPolicy>,
}

impl PluginEgressHooks {
    pub(crate) fn new(id: PluginInstanceId, policy: Option<EgressPolicy>) -> Self {
        Self { id, policy }
    }

    /// Emit the structured denial event that attributes the attempt to the
    /// exact instance. The destination host is recorded host-side (the
    /// operator needs it to seed a grant); only the guest's error is masked.
    fn record_denial(&self, host: &str, reason: &str) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "plugin": self.id.package(),
                    "capability": format!("{:?}", self.id.capability()),
                    "binding": self.id.binding(),
                    "host": host,
                    "reason": reason,
                    "error_key": "plugin_egress_denied",
                })),
            "Denied plugin outbound request by egress policy"
        );
    }
}

/// The policy verdict for one request, computed before any I/O happens.
struct Approval {
    /// Whether this destination carries the private-address carveout.
    allow_private: bool,
}

impl PluginEgressHooks {
    /// Evaluate the allowlist for `host`. `Err` is a denial that has already
    /// been logged.
    fn authorize(&self, host: &str) -> Result<Approval, ErrorCode> {
        let Some(policy) = self.policy.as_ref() else {
            // Deny-by-default: a store built with `wasi:http` linked but no
            // granted policy reaches nothing.
            self.record_denial(host, "no egress policy granted for this instance");
            return Err(denied());
        };

        let decision = policy.resolve(self.id());
        if !zeroclaw_infra::net_guard::egress_host_matches(host, &decision.hosts) {
            self.record_denial(
                host,
                "destination is not in the instance's egress allowlist",
            );
            return Err(denied());
        }

        Ok(Approval {
            allow_private: zeroclaw_infra::net_guard::egress_host_matches(
                host,
                &decision.allow_private,
            ),
        })
    }

    fn id(&self) -> &PluginInstanceId {
        &self.id
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
        let host = authority.host().to_string();

        let approval = match self.authorize(&host) {
            Ok(approval) => approval,
            Err(code) => return Ok(HostFutureIncomingResponse::ready(Ok(Err(code)))),
        };

        let port = authority
            .port_u16()
            .unwrap_or(if config.use_tls { 443 } else { 80 });
        let id = self.id.clone();

        let handle = wasmtime_wasi::runtime::spawn(async move {
            Ok(send(request, config, host, port, approval, id).await)
        });
        Ok(HostFutureIncomingResponse::pending(handle))
    }
}

/// The host-owned pinned send path.
///
/// This mirrors the mechanics of `wasmtime_wasi_http::p2::default_send_request_handler`
/// — same `Host` header fill-in, same connect/first-byte timeouts, same
/// origin-form URI rewrite before `send_request`, same hyper http1 handshake and
/// worker task — but replaces its single `TcpStream::connect(authority)` (which
/// resolves and connects in one unobservable step) with an explicit
/// **resolve → validate → connect-to-validated-address** split.
async fn send(
    mut request: hyper::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
    host: String,
    port: u16,
    approval: Approval,
    id: PluginInstanceId,
) -> Result<IncomingResponse, ErrorCode> {
    use http_body_util::BodyExt;
    use tokio::net::TcpStream;
    use tokio::time::timeout;

    if !request.headers().contains_key(HOST)
        && let Some(authority) = request.uri().authority()
        && let Ok(value) = hyper::header::HeaderValue::from_str(authority.as_str())
    {
        request.headers_mut().insert(HOST, value);
    }

    // ── resolve ──────────────────────────────────────────────────
    // Resolution is ours, so the set of addresses we validate is exactly the
    // set we may connect to.
    let addrs: Vec<SocketAddr> = timeout(
        config.connect_timeout,
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| ErrorCode::ConnectionTimeout)?
    .map_err(|_| dns_failure())?
    .collect();

    // ── validate ─────────────────────────────────────────────────
    let ips: Vec<IpAddr> = addrs.iter().map(SocketAddr::ip).collect();
    let validation = if approval.allow_private {
        // The carveout relaxes the address *class* only. Metadata addresses
        // stay refused: this is the single place that decision is made, and
        // it covers a metadata IP written literally as well as a public name
        // that resolves to one.
        zeroclaw_infra::net_guard::validate_resolved_ips_exclude_metadata(&host, &ips)
    } else {
        zeroclaw_infra::net_guard::validate_resolved_ips_are_public(&host, &ips)
    };
    if let Err(error) = validation {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "plugin": id.package(),
                    "capability": format!("{:?}", id.capability()),
                    "binding": id.binding(),
                    "host": host,
                    "reason": format!("{error}"),
                    "error_key": "plugin_egress_denied",
                })),
            "Denied plugin outbound request by egress policy"
        );
        return Err(denied());
    }

    // ── connect (pinned) ─────────────────────────────────────────
    // Connect by `SocketAddr`, never by name: the address is one we just
    // validated, so no second resolution can substitute a different class.
    let addr = *addrs.first().ok_or(dns_failure())?;
    let tcp_stream = timeout(config.connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| ErrorCode::ConnectionTimeout)?
        .map_err(|_| ErrorCode::ConnectionRefused)?;

    let (mut sender, worker) = if config.use_tls {
        use rustls::pki_types::ServerName;
        use wasmtime_wasi_http::io::TokioIo;

        let root_cert_store = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.into(),
        };
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
        // SNI and certificate verification use the *hostname*, not the pinned
        // address, so pinning the connect does not downgrade TLS identity.
        let domain = ServerName::try_from(host.clone())
            .map_err(|_| ErrorCode::TlsProtocolError)?
            .to_owned();
        let stream = connector
            .connect(domain, tcp_stream)
            .await
            .map_err(|_| ErrorCode::TlsProtocolError)?;
        handshake(TokioIo::new(stream), config.connect_timeout).await?
    } else {
        use wasmtime_wasi_http::io::TokioIo;
        handshake(TokioIo::new(tcp_stream), config.connect_timeout).await?
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

/// Drive one hyper http1 handshake and spawn its connection worker.
async fn handshake<S>(
    stream: S,
    connect_timeout: std::time::Duration,
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
    let (sender, conn) = tokio::time::timeout(
        connect_timeout,
        hyper::client::conn::http1::handshake(stream),
    )
    .await
    .map_err(|_| ErrorCode::ConnectionTimeout)?
    .map_err(|_| ErrorCode::HttpProtocolError)?;

    let worker = wasmtime_wasi::runtime::spawn(async move {
        if let Err(error) = conn.await {
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
    use super::*;
    use crate::{PluginCapability, PluginPermission};

    fn hooks(policy: Option<EgressPolicy>) -> PluginEgressHooks {
        let scope = crate::instance::test_scope(
            PluginCapability::Tool,
            "main",
            [PluginPermission::HttpClient],
        );
        PluginEgressHooks::new(scope.id().clone(), policy)
    }

    fn allowing(hosts: &[&str], private: &[&str]) -> EgressPolicy {
        EgressPolicy::fixed(EgressDecisionInput {
            hosts: hosts.iter().map(|h| (*h).to_string()).collect(),
            allow_private: private.iter().map(|h| (*h).to_string()).collect(),
        })
    }

    #[test]
    fn no_policy_denies_every_destination() {
        let hooks = hooks(None);
        for host in ["example.com", "127.0.0.1", "api.internal"] {
            assert!(
                hooks.authorize(host).is_err(),
                "{host} must be denied without a granted policy"
            );
        }
    }

    #[test]
    fn empty_allowlist_denies_every_destination() {
        let hooks = hooks(Some(EgressPolicy::fixed(EgressDecisionInput::deny_all())));
        assert!(hooks.authorize("example.com").is_err());
    }

    #[test]
    fn allowlist_admits_only_exact_and_explicit_suffix_matches() {
        let hooks = hooks(Some(allowing(&["example.com", "*.example.org"], &[])));
        assert!(hooks.authorize("example.com").is_ok());
        assert!(hooks.authorize("api.example.org").is_ok());
        assert!(
            hooks.authorize("api.example.com").is_err(),
            "a bare domain grant must not imply its subdomains"
        );
        assert!(
            hooks.authorize("example.org").is_err(),
            "a suffix grant must not imply the apex"
        );
        assert!(hooks.authorize("evil.com").is_err());
    }

    #[test]
    fn carveout_is_reported_only_for_the_listed_destination() {
        let hooks = hooks(Some(allowing(&["127.0.0.1", "10.0.0.5"], &["127.0.0.1"])));
        assert!(hooks.authorize("127.0.0.1").unwrap().allow_private);
        assert!(
            !hooks.authorize("10.0.0.5").unwrap().allow_private,
            "the carveout is per destination, not per instance"
        );
    }

    #[test]
    fn carveout_alone_does_not_grant_reach() {
        // A host in `allow_private` but not in `hosts` is still denied: the
        // carveout relaxes an address class, it is not a second allowlist.
        let hooks = hooks(Some(allowing(&["example.com"], &["127.0.0.1"])));
        assert!(hooks.authorize("127.0.0.1").is_err());
    }

    #[test]
    fn policy_is_reread_on_every_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let policy = EgressPolicy::from_resolver(Arc::new(move |_| {
            // Grant reach only from the second request onward, proving the
            // decision is not captured at store-construction time.
            let n = seen.fetch_add(1, Ordering::SeqCst);
            EgressDecisionInput {
                hosts: if n == 0 {
                    Vec::new()
                } else {
                    vec!["example.com".to_string()]
                },
                allow_private: Vec::new(),
            }
        }));
        let hooks = hooks(Some(policy));

        assert!(hooks.authorize("example.com").is_err(), "first read denies");
        assert!(hooks.authorize("example.com").is_ok(), "second read allows");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one resolve per request");
    }

    #[test]
    fn denial_message_names_the_policy_and_leaks_no_host() {
        let ErrorCode::InternalError(Some(message)) = denied() else {
            panic!("denial must carry a message");
        };
        assert!(message.contains("egress policy"), "{message}");
        assert!(!message.contains("127.0.0.1"), "{message}");
    }

    #[test]
    fn resolver_receives_the_instance_identity() {
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let sink = seen.clone();
        let policy = EgressPolicy::from_resolver(Arc::new(move |id| {
            sink.lock()
                .unwrap()
                .push(format!("{}/{}", id.package(), id.binding()));
            EgressDecisionInput::deny_all()
        }));
        let hooks = hooks(Some(policy));
        let _ = hooks.authorize("example.com");
        assert_eq!(seen.lock().unwrap().as_slice(), ["fixture/main"]);
    }
}
