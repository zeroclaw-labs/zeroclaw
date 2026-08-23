//! Instance-scoped outbound network policy shared by every plugin transport.
//!
//! Transport adapters submit an [`EgressRequest`], then dial only the pinned
//! addresses returned by [`AuthorizedEgress`]. Policy is resolved at each
//! request, while live-connection accounting is shared across every transport
//! and store belonging to the same logical plugin instance.
//!
//! Linkers expose only the imports selected by an admitted instance's effective
//! grants. This service repeats that grant check at the operation boundary, then
//! applies the common destination, address-class, and capacity policy. The
//! duplicate check is intentional defense in depth: an adapter cannot
//! accidentally turn a linked-but-ungranted import into network access.
//!
//! # What this module does not own
//!
//! Address classification, the egress pattern grammar, NAT64 translation, and
//! the post-resolution SSRF verdict all live in `zeroclaw_infra::net_guard`,
//! which is also what the built-in tools use. Nothing here re-implements them:
//! a plugin and a built-in tool must not be able to disagree about whether a
//! destination is reachable.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use zeroclaw_infra::net_guard::{
    Nat64Prefix, NetworkGuardError, PrivateNetworkAccess, ResolvedDestination, egress_host_matches,
    normalize_egress_patterns, normalize_host, parse_nat64_prefixes,
};

use crate::PluginPermission;
use crate::instance::{PluginInstanceId, PluginInstanceScope};

/// Protocol family and confidentiality mode requested by a plugin adapter.
///
/// The distinction the host cares about is which effective grant a transport
/// needs and whether it can still become encrypted; the wire protocol itself is
/// the adapter's business.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EgressTransport {
    /// HTTP; `encrypted = true` represents HTTPS.
    Http { encrypted: bool },
    /// WebSocket; `encrypted = true` represents WSS.
    WebSocket { encrypted: bool },
    /// Plain raw TCP.
    Tcp,
    /// TLS from the first byte on a raw connection.
    Tls,
    /// Plain protocol negotiation followed by a mandatory in-place TLS upgrade.
    StartTls,
}

impl EgressTransport {
    fn required_permission(self) -> PluginPermission {
        match self {
            Self::Http { .. } => PluginPermission::HttpClient,
            Self::WebSocket { .. } => PluginPermission::WebSocketClient,
            Self::Tcp | Self::Tls | Self::StartTls => PluginPermission::SocketClient,
        }
    }
}

impl fmt::Display for EgressTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Http { encrypted: false } => "http",
            Self::Http { encrypted: true } => "https",
            Self::WebSocket { encrypted: false } => "websocket",
            Self::WebSocket { encrypted: true } => "secure_websocket",
            Self::Tcp => "tcp",
            Self::Tls => "tls",
            Self::StartTls => "starttls",
        })
    }
}

/// One materialized view of canonical operator egress policy.
///
/// Construct this inside an [`EgressPolicyResolver`] call. Long-lived stores
/// retain the resolver, not this view, so an operator's edit applies to the
/// next dial rather than to the next restart.
///
/// The two host lists are exactly the operator's
/// `plugins.entries[].egress_hosts` and `egress_allow_private`. There is no
/// second config surface: a destination is reachable because the operator
/// granted it, and no manifest, permission, or default adds to that.
#[derive(Clone, Debug)]
pub struct EgressPolicy {
    hosts: Vec<String>,
    allow_private: Vec<String>,
    nat64_prefixes: Vec<Nat64Prefix>,
    max_connections_per_instance: usize,
}

impl EgressPolicy {
    /// Build and validate one resolved policy view.
    ///
    /// `hosts` and `allow_private` use the strict egress grammar
    /// (`zeroclaw_infra::net_guard::normalize_egress_pattern`): exact hosts or
    /// `*.suffix` patterns, with no allow-all form. An empty `hosts` list is
    /// the default and means no reach at all.
    ///
    /// `nat64_prefixes` is the deployment's `security.nat64_prefixes`, parsed
    /// here so a malformed list fails the policy closed rather than silently
    /// disabling network-specific classification. This mirrors how the built-in
    /// tools parse the same list at construction.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError`] for an invalid host pattern, an invalid NAT64
    /// prefix, or a zero connection ceiling.
    pub fn new(
        hosts: &[String],
        allow_private: &[String],
        nat64_prefixes: &[String],
        max_connections_per_instance: usize,
    ) -> Result<Self, EgressError> {
        if max_connections_per_instance == 0 {
            return Err(EgressError::InvalidConnectionLimit);
        }
        let hosts = normalize_egress_patterns(hosts, "plugins.entries.egress_hosts")
            .map_err(|error| EgressError::InvalidHostPattern(error.to_string()))?;
        let allow_private =
            normalize_egress_patterns(allow_private, "plugins.entries.egress_allow_private")
                .map_err(|error| EgressError::InvalidHostPattern(error.to_string()))?;
        let nat64_prefixes = parse_nat64_prefixes(nat64_prefixes, "security.nat64_prefixes")
            .map_err(|error| EgressError::InvalidNat64Prefix(error.to_string()))?;
        Ok(Self {
            hosts,
            allow_private,
            nat64_prefixes,
            max_connections_per_instance,
        })
    }

    /// A policy that grants nothing. The state an unconfigured instance is in.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::InvalidConnectionLimit`] for a zero ceiling.
    pub fn deny_all(max_connections_per_instance: usize) -> Result<Self, EgressError> {
        Self::new(&[], &[], &[], max_connections_per_instance)
    }

    fn grants(&self, host: &str) -> bool {
        egress_host_matches(host, &self.hosts)
    }

    fn private_access(&self, host: &str) -> PrivateNetworkAccess {
        if egress_host_matches(host, &self.allow_private) {
            PrivateNetworkAccess::Allow
        } else {
            PrivateNetworkAccess::Deny
        }
    }
}

type ResolveEgress =
    dyn Fn(&PluginInstanceScope) -> Result<EgressPolicy, EgressError> + Send + Sync;

/// Live point-of-use resolver for canonical operator egress policy.
///
/// Deliberately a closure rather than a resolved value: a long-lived store must
/// never snapshot policy, so the lists are re-read on every request.
#[derive(Clone)]
pub struct EgressPolicyResolver {
    resolve: Arc<ResolveEgress>,
}

impl fmt::Debug for EgressPolicyResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressPolicyResolver")
            .finish_non_exhaustive()
    }
}

impl EgressPolicyResolver {
    /// Wrap a live canonical-config lookup.
    #[must_use]
    pub fn new(
        resolve: impl Fn(&PluginInstanceScope) -> Result<EgressPolicy, EgressError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            resolve: Arc::new(resolve),
        }
    }

    fn resolve(&self, scope: &PluginInstanceScope) -> Result<EgressPolicy, EgressError> {
        (self.resolve)(scope)
    }
}

/// Canonical per-operation request presented to the shared egress boundary.
#[derive(Clone, Debug)]
pub struct EgressRequest {
    scope: PluginInstanceScope,
    transport: EgressTransport,
    host: String,
    port: u16,
}

impl EgressRequest {
    /// Create a scoped outbound request.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError`] for a malformed host or a zero port.
    pub fn new(
        scope: PluginInstanceScope,
        transport: EgressTransport,
        host: &str,
        port: u16,
    ) -> Result<Self, EgressError> {
        let host = normalize_host(host)?;
        if port == 0 {
            return Err(EgressError::Network(NetworkGuardError::InvalidPort));
        }
        Ok(Self {
            scope,
            transport,
            host,
            port,
        })
    }

    /// Host-issued logical instance identity.
    #[must_use]
    pub fn instance_id(&self) -> &PluginInstanceId {
        self.scope.id()
    }

    /// Requested transport family and confidentiality mode.
    #[must_use]
    pub fn transport(&self) -> EgressTransport {
        self.transport
    }

    /// Canonical destination host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Destination port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Default)]
struct ConnectionCounts {
    by_instance: Mutex<HashMap<PluginInstanceId, usize>>,
}

impl ConnectionCounts {
    fn acquire(
        self: &Arc<Self>,
        instance: &PluginInstanceId,
        limit: usize,
    ) -> Result<ConnectionLease, EgressError> {
        let mut counts = self
            .by_instance
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let count = counts.entry(instance.clone()).or_default();
        if *count >= limit {
            return Err(EgressError::ConnectionLimitReached {
                instance: instance_label(instance),
                limit,
            });
        }
        *count += 1;
        Ok(ConnectionLease {
            counts: Arc::clone(self),
            instance: instance.clone(),
        })
    }

    fn release(&self, instance: &PluginInstanceId) {
        let mut counts = self
            .by_instance
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let remove = counts.get_mut(instance).is_some_and(|count| {
            *count = count.saturating_sub(1);
            *count == 0
        });
        if remove {
            counts.remove(instance);
        }
    }
}

fn instance_label(instance: &PluginInstanceId) -> String {
    format!(
        "{}:{:?}:{}",
        instance.package(),
        instance.capability(),
        instance.binding()
    )
}

// Dropping the authorized token returns capacity to the shared budget.
struct ConnectionLease {
    counts: Arc<ConnectionCounts>,
    instance: PluginInstanceId,
}

impl fmt::Debug for ConnectionLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionLease")
            .field("instance", &self.instance)
            .finish_non_exhaustive()
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        self.counts.release(&self.instance);
    }
}

/// A policy-approved request with pinned addresses and a held connection slot.
///
/// A trusted host adapter may open exactly one connection from this token and
/// must retain it for that connection's lifetime. The type deliberately does
/// not expose a convenience dial method that could open several connections
/// against one budget lease.
#[derive(Debug)]
pub struct AuthorizedEgress {
    request: EgressRequest,
    destination: ResolvedDestination,
    _lease: ConnectionLease,
}

impl AuthorizedEgress {
    /// Original canonical request.
    #[must_use]
    pub fn request(&self) -> &EgressRequest {
        &self.request
    }

    /// Exact validated destination. Adapters must not resolve its host again;
    /// use [`ResolvedDestination::host`] for SNI and certificate verification
    /// and [`ResolvedDestination::addresses`] for the connect.
    #[must_use]
    pub fn destination(&self) -> &ResolvedDestination {
        &self.destination
    }
}

/// Shared service injected into plugin stores and cloned across transports.
#[derive(Clone)]
pub struct EgressHostService {
    resolver: EgressPolicyResolver,
    counts: Arc<ConnectionCounts>,
}

impl fmt::Debug for EgressHostService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressHostService").finish_non_exhaustive()
    }
}

impl EgressHostService {
    /// Construct one service around a live canonical policy resolver.
    #[must_use]
    pub fn new(resolver: EgressPolicyResolver) -> Self {
        Self {
            resolver,
            counts: Arc::new(ConnectionCounts::default()),
        }
    }

    /// Resolve DNS, apply current policy, pin the checked addresses, and reserve
    /// one shared connection slot for the request's logical instance.
    ///
    /// This is the single resolution: the addresses in the returned
    /// [`AuthorizedEgress`] are the only ones that may be dialed.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError`] when the grant check, DNS, address validation,
    /// or connection-budget acquisition fails.
    pub async fn authorize(&self, request: EgressRequest) -> Result<AuthorizedEgress, EgressError> {
        let policy = self.resolve_policy(&request)?;
        let addresses = tokio::net::lookup_host((request.host(), request.port()))
            .await
            .map_err(|error| EgressError::DnsFailed {
                host: request.host().to_string(),
                port: request.port(),
                reason: error.to_string(),
            })?
            .collect::<Vec<_>>();
        self.authorize_with_policy(request, addresses, &policy)
    }

    /// Apply current policy to an address set supplied by a resolver.
    ///
    /// This is the adapter seam for custom resolvers and deterministic tests.
    /// The returned [`ResolvedDestination`] is the only address set that may be
    /// dialed; resolving the hostname again defeats the security contract.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError`] for any denied or malformed request.
    pub fn authorize_addresses(
        &self,
        request: EgressRequest,
        addresses: impl IntoIterator<Item = SocketAddr>,
    ) -> Result<AuthorizedEgress, EgressError> {
        let policy = self.resolve_policy(&request)?;
        self.authorize_with_policy(request, addresses, &policy)
    }

    fn resolve_policy(&self, request: &EgressRequest) -> Result<EgressPolicy, EgressError> {
        let permission = request.transport.required_permission();
        if !request.scope.grants().allows(permission) {
            return Err(EgressError::PermissionDenied {
                transport: request.transport,
                permission,
            });
        }
        let policy = self.resolver.resolve(&request.scope)?;
        if !policy.grants(&request.host) {
            return Err(EgressError::DestinationNotGranted {
                instance: instance_label(request.instance_id()),
                host: request.host.clone(),
            });
        }
        Ok(policy)
    }

    fn authorize_with_policy(
        &self,
        request: EgressRequest,
        addresses: impl IntoIterator<Item = SocketAddr>,
        policy: &EgressPolicy,
    ) -> Result<AuthorizedEgress, EgressError> {
        let destination = ResolvedDestination::new(
            &request.host,
            request.port,
            addresses,
            policy.private_access(&request.host),
            &policy.nat64_prefixes,
        )?;
        let lease = self
            .counts
            .acquire(request.instance_id(), policy.max_connections_per_instance)?;
        Ok(AuthorizedEgress {
            request,
            destination,
            _lease: lease,
        })
    }
}

/// Per-connection STARTTLS phase. Transitions never permit plaintext fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartTlsPhase {
    /// Protocol-specific plaintext negotiation may occur.
    Negotiating,
    /// TLS handshake is in progress; no plaintext I/O is allowed.
    Handshaking,
    /// TLS handshake succeeded; application I/O is allowed.
    Secured,
    /// TLS handshake failed; the adapter must close the underlying connection.
    Failed,
}

/// Host-owned STARTTLS transition guard for one connection.
///
/// The downgrade this closes is the classic one: a negotiation that fails to
/// upgrade, and an adapter that carries on in the clear because the connection
/// is technically still usable. Once an upgrade begins, plaintext I/O is over
/// whether or not the handshake succeeds.
#[derive(Debug)]
pub struct StartTlsState {
    phase: StartTlsPhase,
}

impl Default for StartTlsState {
    fn default() -> Self {
        Self::new()
    }
}

impl StartTlsState {
    /// Begin in protocol negotiation before any credentials/application data.
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: StartTlsPhase::Negotiating,
        }
    }

    /// Current authoritative connection phase.
    #[must_use]
    pub fn phase(&self) -> StartTlsPhase {
        self.phase
    }

    /// Whether protocol-specific plaintext negotiation I/O is currently legal.
    #[must_use]
    pub fn plaintext_negotiation_allowed(&self) -> bool {
        self.phase == StartTlsPhase::Negotiating
    }

    /// Whether authenticated/application I/O is currently legal.
    #[must_use]
    pub fn application_io_allowed(&self) -> bool {
        self.phase == StartTlsPhase::Secured
    }

    /// Commit to an in-place TLS upgrade and permanently end plaintext I/O.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::InvalidStartTlsTransition`] unless negotiation is
    /// still active.
    pub fn begin_upgrade(&mut self) -> Result<(), EgressError> {
        self.transition(StartTlsPhase::Negotiating, StartTlsPhase::Handshaking)
    }

    /// Record a successful TLS handshake.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::InvalidStartTlsTransition`] unless a handshake is
    /// in progress.
    pub fn complete_upgrade(&mut self) -> Result<(), EgressError> {
        self.transition(StartTlsPhase::Handshaking, StartTlsPhase::Secured)
    }

    /// Record a failed TLS handshake. The failed phase is terminal and must
    /// result in connection closure; plaintext fallback is never permitted.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::InvalidStartTlsTransition`] unless a handshake is
    /// in progress.
    pub fn fail_upgrade(&mut self) -> Result<(), EgressError> {
        self.transition(StartTlsPhase::Handshaking, StartTlsPhase::Failed)
    }

    fn transition(
        &mut self,
        expected: StartTlsPhase,
        next: StartTlsPhase,
    ) -> Result<(), EgressError> {
        if self.phase != expected {
            return Err(EgressError::InvalidStartTlsTransition {
                from: self.phase,
                to: next,
            });
        }
        self.phase = next;
        Ok(())
    }
}

/// Failure at the shared plugin egress boundary.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// Host/address policy rejection from the shared network guard.
    #[error("network destination rejected: {0}")]
    Network(#[from] NetworkGuardError),
    /// Invalid pattern in the canonical operator allowlist.
    #[error("invalid plugin egress host pattern: {0}")]
    InvalidHostPattern(String),
    /// Invalid `security.nat64_prefixes` entry. Fails the policy closed rather
    /// than quietly narrowing the validation boundary.
    #[error("invalid NAT64 prefix configuration: {0}")]
    InvalidNat64Prefix(String),
    /// A policy returned an unsafe zero connection ceiling.
    #[error("plugin max connections per instance must be greater than zero")]
    InvalidConnectionLimit,
    /// The admitted instance lacks the transport's effective grant.
    #[error("{transport} egress requires the effective {permission:?} permission")]
    PermissionDenied {
        transport: EgressTransport,
        permission: PluginPermission,
    },
    /// The destination is not in this instance's operator-granted allowlist.
    #[error("plugin instance {instance} is not granted egress to {host:?}")]
    DestinationNotGranted { instance: String, host: String },
    /// DNS resolution failed before policy could pin an address set.
    #[error("DNS resolution for {host}:{port} failed: {reason}")]
    DnsFailed {
        host: String,
        port: u16,
        reason: String,
    },
    /// The per-instance cross-transport connection ceiling is full.
    #[error("plugin instance {instance:?} reached its {limit}-connection limit")]
    ConnectionLimitReached { instance: String, limit: usize },
    /// STARTTLS attempted a transition that could enable downgrade/fallback.
    #[error("invalid STARTTLS transition from {from:?} to {to:?}")]
    InvalidStartTlsTransition {
        from: StartTlsPhase,
        to: StartTlsPhase,
    },
    /// Canonical host policy could not be resolved.
    #[error("plugin egress policy unavailable: {0}")]
    PolicyUnavailable(String),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::{PluginCapability, PluginManifest, PluginPermission};

    use super::*;

    fn scope_with_grants(
        binding: &str,
        grants: impl IntoIterator<Item = PluginPermission>,
    ) -> PluginInstanceScope {
        let permissions = vec![
            PluginPermission::HttpClient,
            PluginPermission::WebSocketClient,
            PluginPermission::SocketClient,
        ];
        let manifest = PluginManifest {
            name: "egress-fixture".to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: None,
            capabilities: vec![PluginCapability::Channel],
            permissions,
            config_schema: None,
            signature: None,
            publisher_key: None,
        };
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Channel, binding, grants)
            .unwrap()
    }

    fn scope(binding: &str) -> PluginInstanceScope {
        scope_with_grants(
            binding,
            [
                PluginPermission::HttpClient,
                PluginPermission::WebSocketClient,
                PluginPermission::SocketClient,
            ],
        )
    }

    fn addr(ip: &str, port: u16) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), port)
    }

    fn owned(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| (*entry).to_string()).collect()
    }

    fn policy(hosts: &[&str], allow_private: &[&str], limit: usize) -> EgressPolicy {
        EgressPolicy::new(&owned(hosts), &owned(allow_private), &[], limit).unwrap()
    }

    fn service(policy: EgressPolicy) -> EgressHostService {
        EgressHostService::new(EgressPolicyResolver::new(move |_| Ok(policy.clone())))
    }

    fn request(binding: &str, transport: EgressTransport, host: &str, port: u16) -> EgressRequest {
        EgressRequest::new(scope(binding), transport, host, port).unwrap()
    }

    #[test]
    fn an_ungranted_destination_is_denied_even_when_the_transport_is_granted() {
        let service = service(policy(&["api.example.com"], &[], 2));
        let granted = request(
            "main",
            EgressTransport::Http { encrypted: true },
            "api.example.com",
            443,
        );
        assert!(
            service
                .authorize_addresses(granted, [addr("1.1.1.1", 443)])
                .is_ok()
        );

        let ungranted = request(
            "main",
            EgressTransport::Http { encrypted: true },
            "other.example.com",
            443,
        );
        assert!(matches!(
            service.authorize_addresses(ungranted, [addr("1.1.1.1", 443)]),
            Err(EgressError::DestinationNotGranted { .. })
        ));
    }

    #[test]
    fn an_empty_allowlist_reaches_nothing() {
        let service = service(EgressPolicy::deny_all(8).unwrap());
        let anywhere = request(
            "main",
            EgressTransport::Http { encrypted: true },
            "api.example.com",
            443,
        );
        assert!(matches!(
            service.authorize_addresses(anywhere, [addr("1.1.1.1", 443)]),
            Err(EgressError::DestinationNotGranted { .. })
        ));
    }

    /// The grant is per destination, and the strict grammar's apex/subdomain
    /// asymmetry has to survive the trip through the request path.
    #[test]
    fn a_suffix_grant_does_not_authorize_its_apex() {
        let service = service(policy(&["*.cdn.example.com"], &[], 2));
        let subdomain = request(
            "main",
            EgressTransport::Http { encrypted: true },
            "assets.cdn.example.com",
            443,
        );
        assert!(
            service
                .authorize_addresses(subdomain, [addr("1.1.1.1", 443)])
                .is_ok()
        );

        let apex = request(
            "main",
            EgressTransport::Http { encrypted: true },
            "cdn.example.com",
            443,
        );
        assert!(
            matches!(
                service.authorize_addresses(apex, [addr("1.1.1.1", 443)]),
                Err(EgressError::DestinationNotGranted { .. })
            ),
            "a subdomain wildcard must not authorize its apex"
        );
    }

    #[test]
    fn every_transport_is_rejected_without_its_effective_grant() {
        let service = service(policy(&["secure.example.com"], &[], 8));
        let cases = [
            (
                EgressTransport::Http { encrypted: true },
                PluginPermission::WebSocketClient,
                PluginPermission::HttpClient,
            ),
            (
                EgressTransport::WebSocket { encrypted: true },
                PluginPermission::HttpClient,
                PluginPermission::WebSocketClient,
            ),
            (
                EgressTransport::Tls,
                PluginPermission::HttpClient,
                PluginPermission::SocketClient,
            ),
            (
                EgressTransport::StartTls,
                PluginPermission::HttpClient,
                PluginPermission::SocketClient,
            ),
            (
                EgressTransport::Tcp,
                PluginPermission::HttpClient,
                PluginPermission::SocketClient,
            ),
        ];

        for (transport, wrong_grant, expected) in cases {
            let request = EgressRequest::new(
                scope_with_grants("main", [wrong_grant]),
                transport,
                "secure.example.com",
                443,
            )
            .unwrap();
            assert!(
                matches!(
                    service.authorize_addresses(request, [addr("1.1.1.1", 443)]),
                    Err(EgressError::PermissionDenied { permission, .. }) if permission == expected
                ),
                "{transport} must require {expected:?}"
            );
        }
    }

    /// The private carveout relaxes an address class for a granted host. It
    /// never reaches metadata, and it never covers a host that was not granted.
    #[test]
    fn private_carveout_is_host_scoped_and_metadata_remains_blocked() {
        let service = service(policy(
            &["*.internal.example", "metadata.internal.example"],
            &["*.internal.example", "metadata.internal.example"],
            4,
        ));

        let allowed = request("main", EgressTransport::Tls, "mail.internal.example", 993);
        assert!(
            service
                .authorize_addresses(allowed, [addr("10.0.0.5", 993)])
                .is_ok()
        );

        let metadata = request(
            "main",
            EgressTransport::Tls,
            "metadata.internal.example",
            443,
        );
        let error = service
            .authorize_addresses(metadata, [addr("169.254.169.254", 443)])
            .unwrap_err();
        assert!(
            matches!(
                error,
                EgressError::Network(NetworkGuardError::CloudMetadata { .. })
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_granted_host_without_the_carveout_cannot_resolve_private() {
        let service = service(policy(&["gitea.example.com"], &[], 2));
        let request = request("main", EgressTransport::Tls, "gitea.example.com", 443);
        let error = service
            .authorize_addresses(request, [addr("10.0.0.5", 443)])
            .unwrap_err();
        assert!(
            matches!(
                error,
                EgressError::Network(NetworkGuardError::PrivateNetworkDenied { .. })
            ),
            "unexpected error: {error}"
        );
    }

    /// The single-resolution contract: one mixed answer set is refused outright
    /// rather than letting resolver order pick the trust zone.
    #[test]
    fn a_mixed_public_private_answer_set_is_refused() {
        let service = service(policy(&["rebind.example.com"], &["rebind.example.com"], 2));
        let request = request("main", EgressTransport::Tls, "rebind.example.com", 443);
        let error = service
            .authorize_addresses(request, [addr("1.1.1.1", 443), addr("10.0.0.5", 443)])
            .unwrap_err();
        assert!(
            matches!(
                error,
                EgressError::Network(NetworkGuardError::MixedAddressClasses)
            ),
            "unexpected error: {error}"
        );
    }

    /// The pin. The authorized token must carry the exact addresses that were
    /// validated, for a host that does not resolve at all — so an adapter (or a
    /// regression) that reached for a second resolution could not proceed.
    #[test]
    fn the_authorized_destination_is_the_validated_address_set() {
        let service = service(policy(&["pinned.invalid"], &[], 2));
        let request = request("main", EgressTransport::Tls, "pinned.invalid", 443);
        let authorized = service
            .authorize_addresses(request, [addr("1.1.1.1", 443), addr("8.8.8.8", 443)])
            .unwrap();
        assert_eq!(authorized.destination().host(), "pinned.invalid");
        assert_eq!(authorized.destination().port(), 443);
        assert_eq!(
            authorized.destination().addresses(),
            [addr("1.1.1.1", 443), addr("8.8.8.8", 443)],
            "the token must pin the validated answer set, not a fresh resolution"
        );
    }

    /// A NAT64 translator declared in `security.nat64_prefixes` makes an
    /// apparently-global IPv6 answer reach a private or metadata destination.
    /// The foundation must classify through it exactly as the tool layer does.
    #[test]
    fn configured_nat64_translation_is_classified_on_the_foundation_path() {
        let nat64 = owned(&["2001:67c:2b0:db32:0:1::/96"]);
        let build = |allow_private: &[&str]| {
            EgressPolicy::new(
                &owned(&["translated.example.com"]),
                &owned(allow_private),
                &nat64,
                4,
            )
            .unwrap()
        };

        // -> 10.0.0.5, which is private once the translator is declared.
        let private_via_nat64 = addr("2001:67c:2b0:db32:0:1:a00:5", 443);
        let error = service(build(&[]))
            .authorize_addresses(
                request("main", EgressTransport::Tls, "translated.example.com", 443),
                [private_via_nat64],
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                EgressError::Network(NetworkGuardError::PrivateNetworkDenied { .. })
            ),
            "unexpected error: {error}"
        );

        // -> 169.254.169.254, which the carveout must not re-open.
        let metadata_via_nat64 = addr("2001:67c:2b0:db32:0:1:a9fe:a9fe", 443);
        let error = service(build(&["translated.example.com"]))
            .authorize_addresses(
                request("main", EgressTransport::Tls, "translated.example.com", 443),
                [metadata_via_nat64],
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                EgressError::Network(NetworkGuardError::CloudMetadata { .. })
            ),
            "unexpected error: {error}"
        );
    }

    /// A malformed prefix list must fail the policy closed rather than silently
    /// disabling network-specific classification.
    #[test]
    fn a_malformed_nat64_prefix_list_fails_the_policy_closed() {
        let error = EgressPolicy::new(
            &owned(&["api.example.com"]),
            &[],
            &owned(&["2001:db8::/97"]),
            4,
        )
        .unwrap_err();
        assert!(
            matches!(error, EgressError::InvalidNat64Prefix(_)),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn policy_is_resolved_live_for_every_request() {
        let allow_private = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&allow_private);
        let service = EgressHostService::new(EgressPolicyResolver::new(move |_| {
            let hosts = owned(&["internal.example"]);
            let private = if flag.load(Ordering::SeqCst) {
                owned(&["internal.example"])
            } else {
                Vec::new()
            };
            EgressPolicy::new(&hosts, &private, &[], 2)
        }));
        let request = || request("main", EgressTransport::Tls, "internal.example", 443);
        assert!(
            service
                .authorize_addresses(request(), [addr("10.0.0.2", 443)])
                .is_err()
        );
        allow_private.store(true, Ordering::SeqCst);
        assert!(
            service
                .authorize_addresses(request(), [addr("10.0.0.2", 443)])
                .is_ok()
        );
    }

    #[test]
    fn one_budget_is_shared_by_instance_across_transports_and_store_clones() {
        let service = service(policy(&["mail.example.com", "irc.example.com"], &[], 1));
        let clone = service.clone();
        let first = service
            .authorize_addresses(
                request("shared", EgressTransport::Tls, "mail.example.com", 993),
                [addr("1.1.1.1", 993)],
            )
            .unwrap();
        let second = || request("shared", EgressTransport::Tcp, "irc.example.com", 6667);
        assert!(matches!(
            clone.authorize_addresses(second(), [addr("1.1.1.1", 6667)]),
            Err(EgressError::ConnectionLimitReached { limit: 1, .. })
        ));
        drop(first);
        assert!(
            clone
                .authorize_addresses(second(), [addr("1.1.1.1", 6667)])
                .is_ok()
        );
    }

    #[test]
    fn budgets_are_isolated_by_canonical_instance_id() {
        let service = service(policy(&["api.example.com"], &[], 1));
        let authorize = |binding| {
            service.authorize_addresses(
                request(binding, EgressTransport::Tls, "api.example.com", 443),
                [addr("1.1.1.1", 443)],
            )
        };
        let main = authorize("main").unwrap();
        let backup = authorize("backup").unwrap();
        assert_ne!(main.request().instance_id(), backup.request().instance_id());
    }

    #[test]
    fn a_zero_connection_ceiling_is_refused_at_policy_construction() {
        assert!(matches!(
            EgressPolicy::deny_all(0),
            Err(EgressError::InvalidConnectionLimit)
        ));
    }

    #[test]
    fn a_malformed_request_host_or_port_is_refused_before_policy_runs() {
        for host in ["https://api.example.com", "api.example.com:8443", ""] {
            assert!(
                EgressRequest::new(scope("main"), EgressTransport::Tls, host, 443).is_err(),
                "{host:?} must not become a request host"
            );
        }
        assert!(matches!(
            EgressRequest::new(scope("main"), EgressTransport::Tls, "api.example.com", 0),
            Err(EgressError::Network(NetworkGuardError::InvalidPort))
        ));
    }

    #[test]
    fn starttls_never_falls_back_to_plaintext_after_upgrade_begins() {
        let mut state = StartTlsState::new();
        assert!(state.plaintext_negotiation_allowed());
        assert!(!state.application_io_allowed());
        state.begin_upgrade().unwrap();
        assert!(!state.plaintext_negotiation_allowed());
        state.fail_upgrade().unwrap();
        assert_eq!(state.phase(), StartTlsPhase::Failed);
        assert!(state.begin_upgrade().is_err());
        assert!(!state.plaintext_negotiation_allowed());
        assert!(!state.application_io_allowed());
    }

    #[test]
    fn starttls_allows_application_io_only_after_success() {
        let mut state = StartTlsState::new();
        assert!(!state.application_io_allowed());
        state.begin_upgrade().unwrap();
        assert!(!state.application_io_allowed());
        state.complete_upgrade().unwrap();
        assert_eq!(state.phase(), StartTlsPhase::Secured);
        assert!(state.application_io_allowed());
        assert!(!state.plaintext_negotiation_allowed());
        assert!(state.fail_upgrade().is_err());
    }

    /// The async entry point performs the one resolution itself. `localhost`
    /// resolves without a network round trip, so this stays deterministic.
    #[tokio::test]
    async fn authorize_resolves_once_and_pins_what_it_resolved() {
        let service = service(policy(&["localhost"], &["localhost"], 2));
        let authorized = service
            .authorize(request("main", EgressTransport::Tls, "localhost", 443))
            .await
            .unwrap();
        assert_eq!(authorized.destination().host(), "localhost");
        assert!(
            !authorized.destination().addresses().is_empty(),
            "the pin must carry the addresses the service resolved"
        );
        assert!(
            authorized
                .destination()
                .addresses()
                .iter()
                .all(|address| address.ip().is_loopback() && address.port() == 443),
            "got: {:?}",
            authorized.destination().addresses()
        );
    }

    #[tokio::test]
    async fn authorize_checks_the_grant_before_it_resolves_anything() {
        let service = service(EgressPolicy::deny_all(2).unwrap());
        let error = service
            .authorize(request("main", EgressTransport::Tls, "localhost", 443))
            .await
            .unwrap_err();
        assert!(
            matches!(error, EgressError::DestinationNotGranted { .. }),
            "unexpected error: {error}"
        );
    }
}
