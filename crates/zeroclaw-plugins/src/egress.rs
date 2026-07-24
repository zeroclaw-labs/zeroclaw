//! Instance-scoped outbound network policy shared by every plugin transport.
//!
//! Transport adapters submit an [`EgressRequest`], then dial only the pinned
//! addresses returned by [`AuthorizedEgress`]. Policy is resolved at each
//! request, while live-connection accounting is shared across every transport
//! and store belonging to the same logical plugin instance.
//!
//! Linkers expose only the imports selected by an admitted instance's effective
//! grants. This service repeats that grant check at the operation boundary, then
//! applies the common destination, confidentiality, TLS-profile, and capacity
//! policy. The duplicate check is intentional defense in depth: an adapter
//! cannot accidentally turn a linked-but-ungranted import into network access.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use zeroclaw_api::plugin_egress::{OutboundHostPattern, is_valid_tls_profile_name};
use zeroclaw_api::plugin_key::SecretPropertyRef;
use zeroclaw_infra::net_guard::{
    NetworkGuardError, PrivateNetworkAccess, ResolvedDestination, normalize_host,
};

use crate::PluginPermission;
use crate::instance::{PluginInstanceId, PluginInstanceScope};

/// Protocol family and confidentiality mode requested by a plugin adapter.
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

    fn permanently_plaintext(self) -> bool {
        matches!(
            self,
            Self::Http { encrypted: false } | Self::WebSocket { encrypted: false } | Self::Tcp
        )
    }

    fn uses_tls(self) -> bool {
        matches!(
            self,
            Self::Http { encrypted: true }
                | Self::WebSocket { encrypted: true }
                | Self::Tls
                | Self::StartTls
        )
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

/// Validated operator-facing name of a TLS profile.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TlsProfileName(String);

impl TlsProfileName {
    /// Parse a lowercase profile slug.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::InvalidTlsProfileName`] for an invalid slug.
    pub fn new(name: impl Into<String>) -> Result<Self, EgressError> {
        let name = name.into();
        if !is_valid_tls_profile_name(&name) {
            return Err(EgressError::InvalidTlsProfileName(name));
        }
        Ok(Self(name))
    }

    /// Canonical profile slug.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Secret references for one TLS client certificate and its private key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsClientIdentity {
    certificate: SecretPropertyRef,
    private_key: SecretPropertyRef,
}

impl TlsClientIdentity {
    /// Pair a certificate-chain property with its private-key property.
    #[must_use]
    pub fn new(certificate: SecretPropertyRef, private_key: SecretPropertyRef) -> Self {
        Self {
            certificate,
            private_key,
        }
    }

    /// PEM certificate-chain secret reference.
    #[must_use]
    pub fn certificate(&self) -> &SecretPropertyRef {
        &self.certificate
    }

    /// PEM private-key secret reference.
    #[must_use]
    pub fn private_key(&self) -> &SecretPropertyRef {
        &self.private_key
    }
}

/// Named TLS trust and optional mTLS identity policy.
///
/// The profile contains references only; it never owns resolved PEM bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsProfile {
    name: TlsProfileName,
    hosts: Vec<OutboundHostPattern>,
    system_roots: bool,
    custom_ca: Option<SecretPropertyRef>,
    client_identity: Option<TlsClientIdentity>,
}

impl TlsProfile {
    /// Create a named TLS profile.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError`] if no destination is bound, a host pattern is
    /// invalid, or neither system roots nor a custom CA supplies trust anchors.
    pub fn new(
        name: TlsProfileName,
        hosts: impl IntoIterator<Item = String>,
        system_roots: bool,
        custom_ca: Option<SecretPropertyRef>,
        client_identity: Option<TlsClientIdentity>,
    ) -> Result<Self, EgressError> {
        let hosts = hosts
            .into_iter()
            .map(|pattern| {
                OutboundHostPattern::parse(&pattern).ok_or(EgressError::InvalidHostPattern(pattern))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if hosts.is_empty() {
            return Err(EgressError::TlsProfileWithoutHosts(
                name.as_str().to_string(),
            ));
        }
        if !system_roots && custom_ca.is_none() {
            return Err(EgressError::InvalidTlsProfile {
                profile: name.as_str().to_string(),
                reason: "at least one of system roots or a custom CA is required".to_string(),
            });
        }
        Ok(Self {
            name,
            hosts,
            system_roots,
            custom_ca,
            client_identity,
        })
    }

    /// Profile name selected by an egress request.
    #[must_use]
    pub fn name(&self) -> &TlsProfileName {
        &self.name
    }

    /// Whether platform/system trust roots should be loaded.
    #[must_use]
    pub fn uses_system_roots(&self) -> bool {
        self.system_roots
    }

    /// Optional instance-secret property containing PEM CA certificates.
    #[must_use]
    pub fn custom_ca(&self) -> Option<&SecretPropertyRef> {
        self.custom_ca.as_ref()
    }

    /// Optional instance-secret properties forming an mTLS identity.
    #[must_use]
    pub fn client_identity(&self) -> Option<&TlsClientIdentity> {
        self.client_identity.as_ref()
    }

    fn allows_host(&self, host: &str) -> bool {
        self.hosts
            .iter()
            .any(|pattern| pattern.matches_normalized(host))
    }
}

/// One materialized view of canonical operator egress policy.
///
/// Construct this inside an [`EgressPolicyResolver`] call. Long-lived stores
/// retain the resolver, not this view, so reloads apply to the next dial.
#[derive(Clone, Debug)]
pub struct EgressPolicy {
    private_network_hosts: Vec<OutboundHostPattern>,
    plaintext_hosts: Vec<OutboundHostPattern>,
    tls_profiles: HashMap<TlsProfileName, TlsProfile>,
    max_connections_per_instance: usize,
}

impl EgressPolicy {
    /// Build and validate one resolved policy view.
    ///
    /// Host patterns accept exact hosts, `*.example.com`, or the explicit `*`
    /// wildcard. Public encrypted egress is permitted by default; these lists
    /// authorize only otherwise-denied private-network and permanent-plaintext
    /// exceptions.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError`] for invalid patterns, duplicate TLS profile
    /// names, or a zero connection ceiling.
    pub fn new(
        private_network_hosts: impl IntoIterator<Item = String>,
        plaintext_hosts: impl IntoIterator<Item = String>,
        tls_profiles: impl IntoIterator<Item = TlsProfile>,
        max_connections_per_instance: usize,
    ) -> Result<Self, EgressError> {
        if max_connections_per_instance == 0 {
            return Err(EgressError::InvalidConnectionLimit);
        }
        let private_network_hosts = private_network_hosts
            .into_iter()
            .map(|pattern| {
                OutboundHostPattern::parse(&pattern).ok_or(EgressError::InvalidHostPattern(pattern))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let plaintext_hosts = plaintext_hosts
            .into_iter()
            .map(|pattern| {
                OutboundHostPattern::parse(&pattern).ok_or(EgressError::InvalidHostPattern(pattern))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut profiles = HashMap::new();
        for profile in tls_profiles {
            let name = profile.name().clone();
            if profiles.insert(name.clone(), profile).is_some() {
                return Err(EgressError::DuplicateTlsProfile(name.as_str().to_string()));
            }
        }
        Ok(Self {
            private_network_hosts,
            plaintext_hosts,
            tls_profiles: profiles,
            max_connections_per_instance,
        })
    }

    fn private_network_allowed(&self, host: &str) -> bool {
        self.private_network_hosts
            .iter()
            .any(|pattern| pattern.matches_normalized(host))
    }

    fn plaintext_allowed(&self, host: &str) -> bool {
        self.plaintext_hosts
            .iter()
            .any(|pattern| pattern.matches_normalized(host))
    }
}

type ResolveEgress =
    dyn Fn(&PluginInstanceScope) -> Result<EgressPolicy, EgressError> + Send + Sync;

/// Live point-of-use resolver for canonical operator egress policy.
#[derive(Clone)]
pub struct EgressPolicyResolver {
    resolve: Arc<ResolveEgress>,
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
    tls_profile: Option<TlsProfileName>,
}

impl EgressRequest {
    /// Create a scoped outbound request.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError`] for a malformed host/port/profile or for
    /// selecting a TLS profile on a permanently plaintext transport.
    pub fn new(
        scope: PluginInstanceScope,
        transport: EgressTransport,
        host: &str,
        port: u16,
        tls_profile: Option<&str>,
    ) -> Result<Self, EgressError> {
        let host = normalize_host(host)?;
        if port == 0 {
            return Err(EgressError::Network(NetworkGuardError::InvalidPort));
        }
        let tls_profile = tls_profile.map(TlsProfileName::new).transpose()?;
        if tls_profile.is_some() && !transport.uses_tls() {
            return Err(EgressError::TlsProfileOnPlaintext(transport));
        }
        Ok(Self {
            scope,
            transport,
            host,
            port,
            tls_profile,
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

    /// Optional named TLS profile; `None` means system roots without mTLS.
    #[must_use]
    pub fn tls_profile(&self) -> Option<&TlsProfileName> {
        self.tls_profile.as_ref()
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
                instance: instance.config_entry_key().unwrap_or_else(|_| {
                    format!(
                        "{}:{:?}:{}",
                        instance.package(),
                        instance.capability(),
                        instance.binding()
                    )
                }),
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
    tls_profile: Option<TlsProfile>,
    _lease: ConnectionLease,
}

impl AuthorizedEgress {
    /// Original canonical request.
    #[must_use]
    pub fn request(&self) -> &EgressRequest {
        &self.request
    }

    /// Exact validated destination. Adapters must not resolve its host again.
    #[must_use]
    pub fn destination(&self) -> &ResolvedDestination {
        &self.destination
    }

    /// Selected named profile, or `None` for the implicit system-roots profile.
    #[must_use]
    pub fn tls_profile(&self) -> Option<&TlsProfile> {
        self.tls_profile.as_ref()
    }
}

/// Shared service injected into plugin stores and cloned across transports.
#[derive(Clone)]
pub struct EgressHostService {
    resolver: EgressPolicyResolver,
    counts: Arc<ConnectionCounts>,
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
    /// # Errors
    ///
    /// Returns [`EgressError`] when policy resolution, DNS, address validation,
    /// TLS-profile selection, or connection-budget acquisition fails.
    pub async fn authorize(&self, request: EgressRequest) -> Result<AuthorizedEgress, EgressError> {
        let (policy, tls_profile) = self.resolve_policy(&request)?;
        let addresses = tokio::net::lookup_host((request.host(), request.port()))
            .await
            .map_err(|error| EgressError::DnsFailed {
                host: request.host().to_string(),
                port: request.port(),
                reason: error.to_string(),
            })?
            .collect::<Vec<_>>();
        self.authorize_with_policy(request, addresses, policy, tls_profile)
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
        let (policy, tls_profile) = self.resolve_policy(&request)?;
        self.authorize_with_policy(request, addresses, policy, tls_profile)
    }

    fn resolve_policy(
        &self,
        request: &EgressRequest,
    ) -> Result<(EgressPolicy, Option<TlsProfile>), EgressError> {
        let permission = request.transport.required_permission();
        if !request.scope.grants().allows(permission) {
            return Err(EgressError::PermissionDenied {
                transport: request.transport,
                permission,
            });
        }
        let policy = self.resolver.resolve(&request.scope)?;
        if request.transport.permanently_plaintext() && !policy.plaintext_allowed(&request.host) {
            return Err(EgressError::PlaintextDenied {
                transport: request.transport,
                host: request.host.clone(),
            });
        }
        let tls_profile = if let Some(name) = request.tls_profile.as_ref() {
            let profile = policy
                .tls_profiles
                .get(name)
                .cloned()
                .ok_or_else(|| EgressError::UnknownTlsProfile(name.as_str().to_string()))?;
            if !profile.allows_host(&request.host) {
                return Err(EgressError::TlsProfileHostDenied {
                    profile: name.as_str().to_string(),
                    host: request.host.clone(),
                });
            }
            Some(profile)
        } else {
            None
        };
        Ok((policy, tls_profile))
    }

    fn authorize_with_policy(
        &self,
        request: EgressRequest,
        addresses: impl IntoIterator<Item = SocketAddr>,
        policy: EgressPolicy,
        tls_profile: Option<TlsProfile>,
    ) -> Result<AuthorizedEgress, EgressError> {
        let private_access = if policy.private_network_allowed(&request.host) {
            PrivateNetworkAccess::Allow
        } else {
            PrivateNetworkAccess::Deny
        };
        let destination =
            ResolvedDestination::new(&request.host, request.port, addresses, private_access)?;
        let lease = self
            .counts
            .acquire(request.instance_id(), policy.max_connections_per_instance)?;
        Ok(AuthorizedEgress {
            request,
            destination,
            tls_profile,
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
    /// Host/address policy rejection.
    #[error("network destination rejected: {0}")]
    Network(#[from] NetworkGuardError),
    /// Invalid exception pattern in canonical config.
    #[error("invalid plugin egress host pattern: {0:?}")]
    InvalidHostPattern(String),
    /// Invalid top-level secret property reference.
    #[error("invalid TLS secret property reference: {0:?}")]
    InvalidSecretReference(String),
    /// Invalid TLS profile slug.
    #[error("invalid TLS profile name: {0:?}")]
    InvalidTlsProfileName(String),
    /// Incoherent TLS profile definition.
    #[error("invalid TLS profile {profile:?}: {reason}")]
    InvalidTlsProfile { profile: String, reason: String },
    /// Duplicate name in the canonical TLS profile table.
    #[error("duplicate TLS profile name: {0:?}")]
    DuplicateTlsProfile(String),
    /// A TLS profile has no destination binding.
    #[error("TLS profile {0:?} must authorize at least one host pattern")]
    TlsProfileWithoutHosts(String),
    /// A policy returned an unsafe zero connection ceiling.
    #[error("plugin max connections per instance must be greater than zero")]
    InvalidConnectionLimit,
    /// The admitted instance lacks the transport's effective grant.
    #[error("{transport} egress requires the effective {permission:?} permission")]
    PermissionDenied {
        transport: EgressTransport,
        permission: PluginPermission,
    },
    /// A permanently plaintext transport lacks an exact operator exception.
    #[error("plaintext {transport} egress to {host:?} is not authorized")]
    PlaintextDenied {
        transport: EgressTransport,
        host: String,
    },
    /// A plaintext transport attempted to select TLS metadata.
    #[error("TLS profile cannot be selected for plaintext {0} egress")]
    TlsProfileOnPlaintext(EgressTransport),
    /// Requested named TLS profile does not exist.
    #[error("unknown plugin TLS profile: {0:?}")]
    UnknownTlsProfile(String),
    /// Requested named TLS profile is not authorized for this destination.
    #[error("plugin TLS profile {profile:?} is not authorized for host {host:?}")]
    TlsProfileHostDenied { profile: String, host: String },
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
            wasm_sha256: None,
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

    fn service(policy: EgressPolicy) -> EgressHostService {
        EgressHostService::new(EgressPolicyResolver::new(move |_| Ok(policy.clone())))
    }

    fn policy(private_hosts: &[&str], plaintext_hosts: &[&str], limit: usize) -> EgressPolicy {
        EgressPolicy::new(
            private_hosts.iter().map(|host| (*host).to_string()),
            plaintext_hosts.iter().map(|host| (*host).to_string()),
            [],
            limit,
        )
        .unwrap()
    }

    #[test]
    fn encrypted_public_egress_is_default_and_plaintext_is_exception_only() {
        let service = service(policy(&[], &[], 2));
        let secure = EgressRequest::new(
            scope("main"),
            EgressTransport::Http { encrypted: true },
            "api.example.com",
            443,
            None,
        )
        .unwrap();
        assert!(
            service
                .authorize_addresses(secure, [addr("1.1.1.1", 443)])
                .is_ok()
        );

        let plaintext = EgressRequest::new(
            scope("main"),
            EgressTransport::Tcp,
            "irc.example.com",
            6667,
            None,
        )
        .unwrap();
        assert!(matches!(
            service.authorize_addresses(plaintext, [addr("1.1.1.1", 6667)]),
            Err(EgressError::PlaintextDenied { .. })
        ));
    }

    #[test]
    fn every_transport_is_rejected_without_its_effective_grant() {
        let service = service(policy(&[], &["plain.example.com"], 8));
        let cases = [
            (
                EgressTransport::Http { encrypted: true },
                "secure.example.com",
                443,
                PluginPermission::WebSocketClient,
                PluginPermission::HttpClient,
            ),
            (
                EgressTransport::WebSocket { encrypted: true },
                "secure.example.com",
                443,
                PluginPermission::HttpClient,
                PluginPermission::WebSocketClient,
            ),
            (
                EgressTransport::Tls,
                "secure.example.com",
                443,
                PluginPermission::HttpClient,
                PluginPermission::SocketClient,
            ),
        ];

        for (transport, host, port, wrong_grant, expected) in cases {
            let request = EgressRequest::new(
                scope_with_grants("main", [wrong_grant]),
                transport,
                host,
                port,
                None,
            )
            .unwrap();
            assert!(matches!(
                service.authorize_addresses(request, [addr("1.1.1.1", port)]),
                Err(EgressError::PermissionDenied { permission, .. }) if permission == expected
            ));
        }
    }

    #[test]
    fn private_exception_is_host_scoped_and_metadata_remains_blocked() {
        let service = service(policy(&["*.internal.example"], &[], 2));
        let allowed = EgressRequest::new(
            scope("main"),
            EgressTransport::Tls,
            "mail.internal.example",
            993,
            None,
        )
        .unwrap();
        assert!(
            service
                .authorize_addresses(allowed, [addr("10.0.0.5", 993)])
                .is_ok()
        );

        let metadata = EgressRequest::new(
            scope("main"),
            EgressTransport::Tls,
            "metadata.internal.example",
            443,
            None,
        )
        .unwrap();
        assert!(matches!(
            service.authorize_addresses(metadata, [addr("169.254.169.254", 443)]),
            Err(EgressError::Network(NetworkGuardError::CloudMetadata(_)))
        ));

        let wildcard_apex = EgressRequest::new(
            scope("main"),
            EgressTransport::Tls,
            "internal.example",
            443,
            None,
        )
        .unwrap();
        assert!(
            service
                .authorize_addresses(wildcard_apex, [addr("10.0.0.6", 443)])
                .is_err(),
            "a subdomain wildcard must not authorize its apex"
        );
    }

    #[test]
    fn policy_is_resolved_live_for_every_request() {
        let allow_private = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&allow_private);
        let service = EgressHostService::new(EgressPolicyResolver::new(move |_| {
            let hosts = flag
                .load(Ordering::SeqCst)
                .then(|| "internal.example".to_string())
                .into_iter();
            EgressPolicy::new(hosts, [], [], 2)
        }));
        let request = || {
            EgressRequest::new(
                scope("main"),
                EgressTransport::Tls,
                "internal.example",
                443,
                None,
            )
            .unwrap()
        };
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
        let service = service(policy(&[], &["irc.example.com"], 1));
        let clone = service.clone();
        let first = EgressRequest::new(
            scope("shared"),
            EgressTransport::Tls,
            "mail.example.com",
            993,
            None,
        )
        .unwrap();
        let first = service
            .authorize_addresses(first, [addr("1.1.1.1", 993)])
            .unwrap();
        let second = EgressRequest::new(
            scope("shared"),
            EgressTransport::Tcp,
            "irc.example.com",
            6667,
            None,
        )
        .unwrap();
        assert!(matches!(
            clone.authorize_addresses(second.clone(), [addr("1.1.1.1", 6667)]),
            Err(EgressError::ConnectionLimitReached { limit: 1, .. })
        ));
        drop(first);
        assert!(
            clone
                .authorize_addresses(second, [addr("1.1.1.1", 6667)])
                .is_ok()
        );
    }

    #[test]
    fn budgets_are_isolated_by_canonical_instance_id() {
        let service = service(policy(&[], &[], 1));
        let authorize = |binding| {
            let request = EgressRequest::new(
                scope(binding),
                EgressTransport::Tls,
                "api.example.com",
                443,
                None,
            )
            .unwrap();
            service.authorize_addresses(request, [addr("1.1.1.1", 443)])
        };
        let main = authorize("main").unwrap();
        let backup = authorize("backup").unwrap();
        assert_ne!(main.request().instance_id(), backup.request().instance_id());
    }

    #[test]
    fn tls_profiles_hold_only_same_instance_secret_property_references() {
        let profile = TlsProfile::new(
            TlsProfileName::new("corporate-mtls").unwrap(),
            ["api.example.com".to_string()],
            true,
            Some(SecretPropertyRef::parse("corporate_ca_pem").unwrap()),
            Some(TlsClientIdentity::new(
                SecretPropertyRef::parse("client_cert_pem").unwrap(),
                SecretPropertyRef::parse("client_key_pem").unwrap(),
            )),
        )
        .unwrap();
        let policy = EgressPolicy::new([], [], [profile], 2).unwrap();
        let service = service(policy);
        let request = EgressRequest::new(
            scope("main"),
            EgressTransport::Tls,
            "api.example.com",
            443,
            Some("corporate-mtls"),
        )
        .unwrap();
        let authorized = service
            .authorize_addresses(request, [addr("1.1.1.1", 443)])
            .unwrap();
        let selected = authorized.tls_profile().unwrap();
        assert_eq!(selected.custom_ca().unwrap().as_str(), "corporate_ca_pem");
        assert_eq!(
            selected.client_identity().unwrap().private_key().as_str(),
            "client_key_pem"
        );

        let wrong_host = EgressRequest::new(
            scope("main"),
            EgressTransport::Tls,
            "attacker.example",
            443,
            Some("corporate-mtls"),
        )
        .unwrap();
        assert!(matches!(
            service.authorize_addresses(wrong_host, [addr("1.1.1.1", 443)]),
            Err(EgressError::TlsProfileHostDenied { .. })
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
        assert!(!state.application_io_allowed());
    }

    #[test]
    fn starttls_allows_application_io_only_after_success() {
        let mut state = StartTlsState::new();
        state.begin_upgrade().unwrap();
        state.complete_upgrade().unwrap();
        assert_eq!(state.phase(), StartTlsPhase::Secured);
        assert!(state.application_io_allowed());
        assert!(!state.plaintext_negotiation_allowed());
        assert!(state.fail_upgrade().is_err());
    }
}
