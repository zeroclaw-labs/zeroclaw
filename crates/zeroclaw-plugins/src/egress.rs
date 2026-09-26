//! Instance-scoped outbound network policy shared by every plugin transport.
//!
//! Transport adapters submit an [`EgressRequest`], then dial only the pinned
//! addresses returned by [`AuthorizedEgress`]. Policy is resolved at each
//! request, while live-connection accounting is shared process-wide by every
//! transport, store, service, and tool registry that represents the same
//! logical plugin instance.
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
use std::sync::{Arc, Mutex, OnceLock};

use zeroclaw_infra::net_guard::{
    Nat64Prefix, NetworkGuardError, PrivateNetworkAccess, ResolvedDestination, egress_host_matches,
    egress_pattern_contains, normalize_egress_patterns, normalize_host, parse_nat64_prefixes,
};

#[cfg(feature = "plugins-wasmtime")]
use rustls::pki_types::pem::PemObject;
use zeroclaw_api::plugin_egress::is_valid_tls_profile_name;
use zeroclaw_api::plugin_key::SecretPropertyRef;

use crate::PluginPermission;
use crate::instance::{PluginInstanceId, PluginInstanceScope};

/// Deadline shared by outbound connection establishment and a TLS handshake.
///
/// Host policy, not operator configuration. It sits beside the shared
/// authorization boundary so transport adapters cannot drift onto different
/// connect budgets.
#[cfg(feature = "plugins-wasmtime")]
pub(crate) const EGRESS_CONNECT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

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

    /// Whether this transport can carry TLS, and so can use a TLS profile.
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

/// Validated operator-facing name of a TLS profile.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TlsProfileName(String);

impl TlsProfileName {
    /// Parse a profile slug (see
    /// [`zeroclaw_api::plugin_egress::is_valid_tls_profile_name`]).
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

/// Secret references for one TLS client certificate chain and its key.
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

/// Named TLS trust and optional client-identity profile for one instance.
///
/// A profile selects certificates; it never grants a destination. Its hosts
/// must each be inside the instance's `egress_hosts` grant, which
/// [`EgressPolicy::with_tls_profiles`] enforces, and a request that selects it
/// still passes the ordinary grant check first. The profile holds references
/// into the instance's secret config, never PEM bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsProfile {
    name: TlsProfileName,
    hosts: Vec<String>,
    system_roots: bool,
    custom_ca: Option<SecretPropertyRef>,
    client_identity: Option<TlsClientIdentity>,
}

impl TlsProfile {
    /// Create a named TLS profile.
    ///
    /// `hosts` uses the strict egress grammar of `egress_hosts`.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError`] if no destination is bound, a host pattern is
    /// invalid, or neither system roots nor a custom CA supplies trust anchors.
    pub fn new(
        name: TlsProfileName,
        hosts: &[String],
        system_roots: bool,
        custom_ca: Option<SecretPropertyRef>,
        client_identity: Option<TlsClientIdentity>,
    ) -> Result<Self, EgressError> {
        if hosts.is_empty() {
            return Err(EgressError::TlsProfileWithoutHosts(
                name.as_str().to_string(),
            ));
        }
        let hosts = normalize_egress_patterns(
            hosts,
            &format!("plugins.entries.tls_profiles.{}.hosts", name.as_str()),
        )
        .map_err(|error| EgressError::InvalidHostPattern(error.to_string()))?;
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

    /// Profile name a transport selects.
    #[must_use]
    pub fn name(&self) -> &TlsProfileName {
        &self.name
    }

    /// Whether the roots plugin HTTPS trusts are included.
    #[must_use]
    pub fn uses_system_roots(&self) -> bool {
        self.system_roots
    }

    /// Optional instance-secret property containing PEM CA certificates.
    #[must_use]
    pub fn custom_ca(&self) -> Option<&SecretPropertyRef> {
        self.custom_ca.as_ref()
    }

    /// Optional instance-secret properties forming a client identity.
    #[must_use]
    pub fn client_identity(&self) -> Option<&TlsClientIdentity> {
        self.client_identity.as_ref()
    }

    fn allows_host(&self, host: &str) -> bool {
        egress_host_matches(host, &self.hosts)
    }
}

/// Build one rustls client configuration from an authorized TLS profile.
///
/// `system_roots` is the root store plugin HTTPS already trusts, supplied by
/// the caller so every plugin transport shares one trust decision. It is used
/// when `profile` is `None` or the profile keeps system roots. `resolve_secret`
/// is called only for references the profile actually uses, at the operation
/// boundary, so no adapter holds a parallel copy of TLS material.
///
/// # Errors
///
/// Returns [`EgressError`] when a referenced secret is unavailable, PEM is
/// malformed or empty, a CA certificate cannot be added, or a client
/// certificate and key do not form a valid identity.
#[cfg(feature = "plugins-wasmtime")]
pub fn build_tls_client_config(
    profile: Option<&TlsProfile>,
    system_roots: &rustls::RootCertStore,
    mut resolve_secret: impl FnMut(&SecretPropertyRef) -> Result<String, EgressError>,
) -> Result<Arc<rustls::ClientConfig>, EgressError> {
    let profile_name = profile
        .map(|profile| profile.name().as_str())
        .unwrap_or("system-roots")
        .to_string();
    let mut roots = if profile.is_none_or(TlsProfile::uses_system_roots) {
        system_roots.clone()
    } else {
        rustls::RootCertStore::empty()
    };
    if let Some(custom_ca) = profile.and_then(TlsProfile::custom_ca) {
        let pem = resolve_secret(custom_ca)?;
        for certificate in parse_pem_certificates(&pem, &profile_name, "custom CA")? {
            roots
                .add(certificate)
                .map_err(|_| EgressError::InvalidTlsMaterial {
                    profile: profile_name.clone(),
                    part: "custom CA certificate".to_string(),
                })?;
        }
    }

    let builder = rustls::ClientConfig::builder().with_root_certificates(roots);
    let config = if let Some(identity) = profile.and_then(TlsProfile::client_identity) {
        let certificate_pem = resolve_secret(identity.certificate())?;
        let certificates =
            parse_pem_certificates(&certificate_pem, &profile_name, "client certificate")?;
        let private_key_pem = resolve_secret(identity.private_key())?;
        let invalid_key = || EgressError::InvalidTlsMaterial {
            profile: profile_name.clone(),
            part: "client private key".to_string(),
        };
        let private_key =
            rustls::pki_types::PrivateKeyDer::from_pem_slice(private_key_pem.as_bytes())
                .map_err(|_| invalid_key())?;
        builder
            .with_client_auth_cert(certificates, private_key)
            .map_err(|_| EgressError::InvalidTlsMaterial {
                profile: profile_name.clone(),
                part: "client certificate/private-key pair".to_string(),
            })?
    } else {
        builder.with_no_client_auth()
    };
    Ok(Arc::new(config))
}

#[cfg(feature = "plugins-wasmtime")]
fn parse_pem_certificates(
    pem: &str,
    profile: &str,
    part: &str,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, EgressError> {
    let invalid = || EgressError::InvalidTlsMaterial {
        profile: profile.to_string(),
        part: part.to_string(),
    };
    let certificates = rustls::pki_types::CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    if certificates.is_empty() {
        return Err(invalid());
    }
    Ok(certificates)
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
    tls_profiles: HashMap<TlsProfileName, TlsProfile>,
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
    /// Returns [`EgressError`] for an invalid host pattern, a private carveout
    /// that is broader than every host grant, an invalid NAT64 prefix, or a
    /// zero connection ceiling.
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
        if let Some(private) = allow_private.iter().find(|private| {
            !hosts
                .iter()
                .any(|grant| egress_pattern_contains(grant, private))
        }) {
            return Err(EgressError::InvalidHostPattern(format!(
                "plugins.entries.egress_allow_private entry {private:?} is not granted by plugins.entries.egress_hosts; the carveout relaxes an address class for a granted destination, it does not grant one"
            )));
        }
        let nat64_prefixes = parse_nat64_prefixes(nat64_prefixes, "security.nat64_prefixes")
            .map_err(|error| EgressError::InvalidNat64Prefix(error.to_string()))?;
        Ok(Self {
            hosts,
            allow_private,
            tls_profiles: HashMap::new(),
            nat64_prefixes,
            max_connections_per_instance,
        })
    }

    /// Attach the instance's TLS profiles (`plugins.entries[].tls_profiles`).
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::DuplicateTlsProfile`] for a repeated name and
    /// [`EgressError::TlsProfileHostNotGranted`] when a profile names a host the
    /// grant does not contain: a profile selects certificates for a granted
    /// destination, it never grants one.
    pub fn with_tls_profiles(
        mut self,
        profiles: impl IntoIterator<Item = TlsProfile>,
    ) -> Result<Self, EgressError> {
        for profile in profiles {
            if let Some(host) = profile.hosts.iter().find(|host| {
                !self
                    .hosts
                    .iter()
                    .any(|grant| egress_pattern_contains(grant, host))
            }) {
                return Err(EgressError::TlsProfileHostNotGranted {
                    profile: profile.name.as_str().to_string(),
                    host: host.clone(),
                });
            }
            let name = profile.name.clone();
            if self.tls_profiles.insert(name.clone(), profile).is_some() {
                return Err(EgressError::DuplicateTlsProfile(name.as_str().to_string()));
            }
        }
        Ok(self)
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

/// The two operator-authored lists an instance's grant is made of, as the
/// canonical config currently resolves them.
// Gated like its only consumer, the `wasi_http` denial path.
#[cfg(feature = "plugins-wasmtime")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GrantLists {
    pub(crate) hosts: Vec<String>,
    pub(crate) allow_private: Vec<String>,
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
    tls_profile: Option<TlsProfileName>,
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
            tls_profile: None,
        })
    }

    /// Select a named TLS profile for this request. Without one, the request
    /// uses the roots plugin HTTPS trusts and no client certificate.
    ///
    /// # Errors
    ///
    /// Returns [`EgressError::InvalidTlsProfileName`] for a malformed name and
    /// [`EgressError::TlsProfileOnPlaintext`] for a transport that never
    /// carries TLS.
    pub fn with_tls_profile(mut self, name: &str) -> Result<Self, EgressError> {
        let name = TlsProfileName::new(name)?;
        if !self.transport.uses_tls() {
            return Err(EgressError::TlsProfileOnPlaintext(self.transport));
        }
        self.tls_profile = Some(name);
        Ok(self)
    }

    /// Host-issued logical instance identity.
    #[must_use]
    /// The scope this request was made under.
    #[cfg(feature = "plugins-wasmtime")]
    pub(crate) fn scope(&self) -> &PluginInstanceScope {
        &self.scope
    }

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

    /// Selected TLS profile, if any.
    #[must_use]
    pub fn tls_profile(&self) -> Option<&TlsProfileName> {
        self.tls_profile.as_ref()
    }
}

/// Live connection accounting for every plugin instance in this process.
///
/// # Why this state is process-global
///
/// A live connection is *runtime* state: the socket this instance holds open
/// right now either exists or it does not, and there is exactly one truth about
/// that per process. This is deliberately not the same kind of fact as an
/// admission or policy decision, which is derived from canonical config every
/// time it is asked and therefore must never acquire a global counter. Nothing
/// about the operator's policy is cached here — only how many slots are
/// currently held. The ceiling those counts are compared against is still read
/// live from the resolver on every acquire, so lowering it applies to the next
/// connection.
///
/// # Why per-service counting was wrong
///
/// `EgressHostService` used to own its counts, so a clone shared them but an
/// independently built service did not. Production builds a *fresh* service per
/// `all_tools_with_runtime` call, and the same canonical instance is registered
/// by several independent paths — the agent loop, the gateway, the channels
/// orchestrator, and the delegate tool. Each registry therefore handed the same
/// instance a full budget, and N registries multiplied the operator's ceiling by
/// N. Keying on the canonical [`PluginInstanceId`] puts every one of those
/// registries on the same count.
#[derive(Clone, Default)]
struct ConnectionRegistry {
    by_instance: Arc<Mutex<HashMap<PluginInstanceId, Arc<InstanceConnections>>>>,
}

impl ConnectionRegistry {
    /// The one registry every service built by [`EgressHostService::new`] uses.
    fn shared() -> Self {
        static SHARED: OnceLock<ConnectionRegistry> = OnceLock::new();
        SHARED.get_or_init(ConnectionRegistry::default).clone()
    }

    /// This instance's counter, created on first use.
    ///
    /// The map holds a strong reference for the life of the process, and that
    /// is the point rather than an oversight. A `Weak` map would be tidier but
    /// unsound here: no counter outlives the request that acquired against it,
    /// so every entry would drop between connections and quietly reset the
    /// ceiling to zero — a leak of budget rather than of memory. Growth is
    /// bounded by the number of distinct instance identities the host admits,
    /// which is the operator's installed plugin set; a guest cannot mint one.
    fn counter(&self, instance: &PluginInstanceId) -> Arc<InstanceConnections> {
        Arc::clone(self.lock().entry(instance.clone()).or_default())
    }

    fn acquire(
        &self,
        instance: &PluginInstanceId,
        limit: usize,
    ) -> Result<ConnectionLease, EgressError> {
        self.counter(instance).acquire(instance, limit)
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<PluginInstanceId, Arc<InstanceConnections>>> {
        self.by_instance
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    #[cfg(all(test, feature = "plugins-wasmtime"))]
    fn live(&self, instance: &PluginInstanceId) -> usize {
        self.lock()
            .get(instance)
            .map_or(0, |connections| *connections.lock())
    }
}

/// One instance's live connection count, shared by every service that
/// represents that instance.
#[derive(Default)]
struct InstanceConnections {
    live: Mutex<usize>,
}

impl InstanceConnections {
    fn acquire(
        self: &Arc<Self>,
        instance: &PluginInstanceId,
        limit: usize,
    ) -> Result<ConnectionLease, EgressError> {
        let mut live = self.lock();
        if *live >= limit {
            return Err(EgressError::ConnectionLimitReached {
                instance: instance_label(instance),
                limit,
            });
        }
        *live += 1;
        Ok(ConnectionLease {
            connections: Arc::clone(self),
        })
    }

    fn release(&self) {
        let mut live = self.lock();
        *live = live.saturating_sub(1);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, usize> {
        self.live.lock().unwrap_or_else(|error| error.into_inner())
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
    connections: Arc<InstanceConnections>,
}

impl fmt::Debug for ConnectionLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionLease").finish_non_exhaustive()
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        self.connections.release();
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

    /// Exact validated destination. Adapters must not resolve its host again;
    /// use [`ResolvedDestination::host`] for SNI and certificate verification
    /// and [`ResolvedDestination::addresses`] for the connect.
    #[must_use]
    pub fn destination(&self) -> &ResolvedDestination {
        &self.destination
    }

    /// The TLS profile the request selected, resolved from the same policy
    /// view that authorized it; `None` means system roots without a client
    /// certificate.
    #[must_use]
    pub fn tls_profile(&self) -> Option<&TlsProfile> {
        self.tls_profile.as_ref()
    }
}

/// Test-only stand-in for `tokio::net::lookup_host`.
///
/// A deterministic closure from a requested host and port to an address set,
/// used only to model a resolver whose answer changes between calls (DNS
/// rebinding) without depending on real DNS or resolver ordering. Production
/// has no equivalent and never installs one.
#[cfg(test)]
type TestAddressResolver = Arc<dyn Fn(&str, u16) -> Vec<SocketAddr> + Send + Sync>;

/// Shared service injected into plugin stores and cloned across transports.
///
/// Policy is per service, because a service is built around one resolver.
/// Connection accounting is not: it comes from the process-wide
/// connection registry, so two services built independently for the same
/// canonical instance spend one budget rather than one each.
#[derive(Clone)]
pub struct EgressHostService {
    resolver: EgressPolicyResolver,
    connections: ConnectionRegistry,
    /// Test-only DNS override. `None` in every production build (the field
    /// does not exist there at all), so the resolve path stays the shipped
    /// `lookup_host` call.
    #[cfg(test)]
    resolver_override: Option<TestAddressResolver>,
}

impl fmt::Debug for EgressHostService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressHostService").finish_non_exhaustive()
    }
}

impl EgressHostService {
    /// Construct one service around a live canonical policy resolver.
    ///
    /// The connection budget it enforces is the process-wide one for each
    /// instance it sees, so an operator's ceiling holds no matter how many tool
    /// registries end up representing the same instance.
    #[must_use]
    pub fn new(resolver: EgressPolicyResolver) -> Self {
        Self {
            resolver,
            connections: ConnectionRegistry::shared(),
            #[cfg(test)]
            resolver_override: None,
        }
    }

    /// A service whose connection accounting is private to it.
    ///
    /// Test-only. Tests share one process with the real registry, so a test
    /// holding a lease would otherwise be able to exhaust another test's
    /// ceiling. Tests that are *about* the sharing use
    /// [`EgressHostService::new`] with an instance identity unique to that
    /// test, which is the only way to observe the shared registry without
    /// depending on what else is running.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_private_connection_accounting(resolver: EgressPolicyResolver) -> Self {
        Self {
            resolver,
            connections: ConnectionRegistry::default(),
            resolver_override: None,
        }
    }

    /// A service whose DNS resolution is replaced by a deterministic closure.
    ///
    /// Test-only. The override stands in for `tokio::net::lookup_host`, so a
    /// test can pin one answer and then observe that a *different* later answer
    /// is never dialed — the DNS-rebinding / TOCTOU property, made deterministic
    /// and free of any dependence on resolver ordering. Accounting is private to
    /// the caller for the same reason [`Self::with_private_connection_accounting`]
    /// is. Production has no equivalent constructor and never sets the override.
    ///
    /// The gate matches the *caller's*: the only caller is in [`crate::wasi_http`]'s
    /// tests, and that module exists only under `plugins-wasmtime`. A wider gate
    /// makes this dead code on the default feature surface, which the `default
    /// features, all targets` CI row compiles with `-D warnings`.
    #[cfg(all(test, feature = "plugins-wasmtime"))]
    #[must_use]
    pub(crate) fn with_test_resolver(
        resolver: EgressPolicyResolver,
        addresses: impl Fn(&str, u16) -> Vec<SocketAddr> + Send + Sync + 'static,
    ) -> Self {
        Self {
            resolver,
            connections: ConnectionRegistry::default(),
            resolver_override: Some(Arc::new(addresses)),
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
        // Test-only DNS override. Production never installs one, so under
        // `not(test)` this block compiles to nothing and the resolution below is
        // exactly the shipped `lookup_host` path. Under test it lets a case pin
        // the first answer and prove a later, different answer is never dialed.
        #[cfg(test)]
        if let Some(resolver) = &self.resolver_override {
            let addresses = resolver(request.host(), request.port());
            return self.authorize_with_policy(request, addresses, &policy);
        }
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

    /// Live connection count held for one instance.
    ///
    /// Test-only: the budget is observable in production solely through
    /// [`EgressError::ConnectionLimitReached`], and adapters must not be able to
    /// read or reset it. Tests that prove a failed dial returns its slot need to
    /// see the count itself, not just that a later acquire happened to succeed.
    ///
    /// The gate matches the *caller's*, not just `test`: the only caller is in
    /// [`crate::wasi_http`]'s tests, and that module exists only under
    /// `plugins-wasmtime`. A wider gate makes this dead code on the default
    /// feature surface, which is exactly what the `default features, all
    /// targets` CI row compiles with `-D warnings`.
    #[cfg(all(test, feature = "plugins-wasmtime"))]
    pub(crate) fn live_connections(&self, instance: &PluginInstanceId) -> usize {
        self.connections.live(instance)
    }

    /// The instance's current operator grant: the `egress_hosts` and
    /// `egress_allow_private` lists the canonical config resolves to right now.
    ///
    /// A denial remedy uses this so the command it prints carries every entry
    /// the operator already has: `config set` replaces a whole list, so a
    /// remedy built from the denied host alone would silently revoke the rest.
    /// `None` when the policy cannot be resolved; the caller must then avoid
    /// printing a replacement list at all.
    #[cfg(feature = "plugins-wasmtime")]
    pub(crate) fn current_grant(&self, scope: &PluginInstanceScope) -> Option<GrantLists> {
        self.resolver.resolve(scope).ok().map(|policy| GrantLists {
            hosts: policy.hosts,
            allow_private: policy.allow_private,
        })
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
        if let Some(name) = request.tls_profile.as_ref() {
            let profile = policy
                .tls_profiles
                .get(name)
                .ok_or_else(|| EgressError::UnknownTlsProfile(name.as_str().to_string()))?;
            if !profile.allows_host(&request.host) {
                return Err(EgressError::TlsProfileHostDenied {
                    profile: name.as_str().to_string(),
                    host: request.host.clone(),
                });
            }
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
        // The ceiling comes from the policy this request just resolved, never
        // from a value cached alongside the count: an operator who lowers
        // `max_connections_per_instance` binds the next connection.
        let lease = self
            .connections
            .acquire(request.instance_id(), policy.max_connections_per_instance)?;
        let tls_profile = request
            .tls_profile
            .as_ref()
            .and_then(|name| policy.tls_profiles.get(name))
            .cloned();
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
    /// Invalid TLS profile slug.
    #[error("invalid TLS profile name: {0:?}")]
    InvalidTlsProfileName(String),
    /// Incoherent TLS profile definition.
    #[error("invalid TLS profile {profile:?}: {reason}")]
    InvalidTlsProfile { profile: String, reason: String },
    /// Two profiles on one instance share a name.
    #[error("duplicate TLS profile name: {0:?}")]
    DuplicateTlsProfile(String),
    /// A TLS profile binds no destination.
    #[error("TLS profile {0:?} must name at least one host")]
    TlsProfileWithoutHosts(String),
    /// A TLS profile names a host the instance's grant does not contain.
    #[error(
        "TLS profile {profile:?} names {host:?}, which the instance's egress_hosts does not grant"
    )]
    TlsProfileHostNotGranted { profile: String, host: String },
    /// A transport that never carries TLS selected a TLS profile.
    #[error("a TLS profile cannot be selected for plaintext {0} egress")]
    TlsProfileOnPlaintext(EgressTransport),
    /// The selected TLS profile is not configured on this instance.
    #[error("unknown plugin TLS profile: {0:?}")]
    UnknownTlsProfile(String),
    /// The selected TLS profile does not cover this destination.
    #[error("plugin TLS profile {profile:?} is not configured for host {host:?}")]
    TlsProfileHostDenied { profile: String, host: String },
    /// A selected TLS profile's certificate material is missing or invalid.
    #[error("plugin TLS profile {profile:?} has invalid {part}")]
    InvalidTlsMaterial { profile: String, part: String },
    /// An adapter used an authorization issued to a different instance.
    #[error("plugin egress authorization does not belong to this plugin instance")]
    AuthorizationScopeMismatch,
    /// A selected TLS profile names a secret this instance cannot resolve.
    #[error("plugin TLS profile {profile:?} cannot resolve secret property {property:?}")]
    TlsSecretUnavailable { profile: String, property: String },
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use crate::{PluginCapability, PluginManifest, PluginPermission};

    use super::*;

    fn scope_in_package(
        package: &str,
        binding: &str,
        grants: impl IntoIterator<Item = PluginPermission>,
    ) -> PluginInstanceScope {
        let permissions = vec![
            PluginPermission::HttpClient,
            PluginPermission::WebSocketClient,
            PluginPermission::SocketClient,
        ];
        let manifest = PluginManifest {
            name: package.to_string(),
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
            egress: Default::default(),
        };
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Channel, binding, grants)
            .unwrap()
    }

    fn scope_with_grants(
        binding: &str,
        grants: impl IntoIterator<Item = PluginPermission>,
    ) -> PluginInstanceScope {
        scope_in_package("egress-fixture", binding, grants)
    }

    fn all_grants() -> [PluginPermission; 3] {
        [
            PluginPermission::HttpClient,
            PluginPermission::WebSocketClient,
            PluginPermission::SocketClient,
        ]
    }

    fn scope(binding: &str) -> PluginInstanceScope {
        scope_with_grants(binding, all_grants())
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

    /// A service with accounting private to the test that built it.
    ///
    /// Everything below except the sharing regressions is about policy, and
    /// policy is per service. Isolating the counts keeps concurrently running
    /// tests from spending each other's ceilings through the process registry.
    fn service(policy: EgressPolicy) -> EgressHostService {
        EgressHostService::with_private_connection_accounting(EgressPolicyResolver::new(
            move |_| Ok(policy.clone()),
        ))
    }

    /// A service built exactly the way production builds one: bound to the
    /// process-wide connection registry.
    fn shared_service(policy: EgressPolicy) -> EgressHostService {
        EgressHostService::new(EgressPolicyResolver::new(move |_| Ok(policy.clone())))
    }

    fn request(binding: &str, transport: EgressTransport, host: &str, port: u16) -> EgressRequest {
        EgressRequest::new(scope(binding), transport, host, port).unwrap()
    }

    #[cfg(feature = "plugins-wasmtime")]
    fn secret(name: &str) -> SecretPropertyRef {
        SecretPropertyRef::parse(name.to_string()).unwrap()
    }

    fn profile(name: &str, hosts: &[&str]) -> TlsProfile {
        TlsProfile::new(
            TlsProfileName::new(name).unwrap(),
            &owned(hosts),
            true,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn a_tls_profile_cannot_name_a_host_the_grant_does_not_contain() {
        let base = policy(&["imap.example.com", "*.mail.example.com"], &[], 2);
        assert!(
            base.clone()
                .with_tls_profiles([profile(
                    "corp",
                    &["imap.example.com", "*.eu.mail.example.com"]
                )])
                .is_ok()
        );
        assert!(matches!(
            base.clone()
                .with_tls_profiles([profile("corp", &["smtp.example.com"])]),
            Err(EgressError::TlsProfileHostNotGranted { .. })
        ));
        assert!(matches!(
            base.with_tls_profiles([
                profile("same", &["imap.example.com"]),
                profile("same", &["imap.example.com"])
            ]),
            Err(EgressError::DuplicateTlsProfile(_))
        ));
    }

    #[test]
    fn a_tls_profile_is_refused_on_a_plaintext_transport() {
        for plaintext in [
            EgressTransport::Tcp,
            EgressTransport::Http { encrypted: false },
            EgressTransport::WebSocket { encrypted: false },
        ] {
            assert!(matches!(
                request("main", plaintext, "imap.example.com", 143).with_tls_profile("corp"),
                Err(EgressError::TlsProfileOnPlaintext(_))
            ));
        }
        assert!(matches!(
            request("main", EgressTransport::Tls, "imap.example.com", 993).with_tls_profile("Bad"),
            Err(EgressError::InvalidTlsProfileName(_))
        ));
    }

    #[test]
    fn authorization_resolves_the_selected_profile_and_never_widens_the_grant() {
        let service = service(
            policy(&["imap.example.com", "smtp.example.com"], &[], 4)
                .with_tls_profiles([profile("corp", &["imap.example.com"])])
                .unwrap(),
        );
        let with_profile = |host: &str, name: &str| {
            request("main", EgressTransport::Tls, host, 993)
                .with_tls_profile(name)
                .unwrap()
        };

        let authorized = service
            .authorize_addresses(
                with_profile("imap.example.com", "corp"),
                [addr("1.1.1.1", 993)],
            )
            .expect("profile covers a granted host");
        assert_eq!(
            authorized.tls_profile().map(|p| p.name().as_str()),
            Some("corp")
        );
        let plain = service
            .authorize_addresses(
                request("main", EgressTransport::Tls, "imap.example.com", 993),
                [addr("1.1.1.1", 993)],
            )
            .expect("no profile is the default trust");
        assert!(plain.tls_profile().is_none());

        assert!(matches!(
            service.authorize_addresses(
                with_profile("imap.example.com", "absent"),
                [addr("1.1.1.1", 993)]
            ),
            Err(EgressError::UnknownTlsProfile(_))
        ));
        assert!(matches!(
            service.authorize_addresses(
                with_profile("smtp.example.com", "corp"),
                [addr("1.1.1.1", 993)]
            ),
            Err(EgressError::TlsProfileHostDenied { .. })
        ));
        // The grant is checked first: a profile never reaches past it.
        assert!(matches!(
            service.authorize_addresses(
                with_profile("evil.example.net", "corp"),
                [addr("1.1.1.1", 993)]
            ),
            Err(EgressError::DestinationNotGranted { .. })
        ));
    }

    #[cfg(feature = "plugins-wasmtime")]
    #[test]
    fn tls_builder_materializes_custom_ca_and_client_identity_from_secrets() {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params =
            rcgen::CertificateParams::new(vec!["Plugin Test CA".to_string()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_certificate = ca_params.self_signed(&ca_key).unwrap();
        let client_key = rcgen::KeyPair::generate().unwrap();
        let client_certificate = rcgen::CertificateParams::new(vec!["client.example".to_string()])
            .unwrap()
            .signed_by(&client_key, &ca_certificate, &ca_key)
            .unwrap();

        let mtls = TlsProfile::new(
            TlsProfileName::new("private-mtls").unwrap(),
            &owned(&["service.example"]),
            false,
            Some(secret("ca_pem")),
            Some(TlsClientIdentity::new(
                secret("client_cert_pem"),
                secret("client_key_pem"),
            )),
        )
        .unwrap();
        let mut resolved = Vec::new();
        let config =
            build_tls_client_config(Some(&mtls), &rustls::RootCertStore::empty(), |reference| {
                resolved.push(reference.as_str().to_string());
                match reference.as_str() {
                    "ca_pem" => Ok(ca_certificate.pem()),
                    "client_cert_pem" => Ok(client_certificate.pem()),
                    "client_key_pem" => Ok(client_key.serialize_pem()),
                    property => Err(EgressError::TlsSecretUnavailable {
                        profile: "private-mtls".to_string(),
                        property: property.to_string(),
                    }),
                }
            });
        assert!(config.is_ok(), "{config:?}");
        assert!(config.unwrap().client_auth_cert_resolver.has_certs());
        assert_eq!(resolved, ["ca_pem", "client_cert_pem", "client_key_pem"]);
    }

    #[cfg(feature = "plugins-wasmtime")]
    #[test]
    fn tls_builder_rejects_empty_material_and_resolves_nothing_it_does_not_use() {
        let ca_only = TlsProfile::new(
            TlsProfileName::new("private-ca").unwrap(),
            &owned(&["service.example"]),
            false,
            Some(secret("ca_pem")),
            None,
        )
        .unwrap();
        assert!(matches!(
            build_tls_client_config(Some(&ca_only), &rustls::RootCertStore::empty(), |_| Ok(String::new())),
            Err(EgressError::InvalidTlsMaterial { part, .. }) if part == "custom CA"
        ));

        // Without a profile the caller's system roots are used as-is and no
        // secret is ever read.
        let config = build_tls_client_config(None, &rustls::RootCertStore::empty(), |reference| {
            panic!(
                "no secret may be read without a profile, got {}",
                reference.as_str()
            )
        });
        assert!(config.is_ok());
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
    fn a_private_carveout_cannot_widen_an_exact_host_grant() {
        let error = EgressPolicy::new(&owned(&["example.com"]), &owned(&["*.example.com"]), &[], 4)
            .unwrap_err();
        assert!(matches!(error, EgressError::InvalidHostPattern(_)));
        assert!(error.to_string().contains("not granted by"));
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

    /// The sharing regression.
    ///
    /// Production never clones one service around the process. It builds a
    /// fresh [`EgressHostService`] per `all_tools_with_runtime` call, and the
    /// agent loop, the gateway, the channels orchestrator, and the delegate
    /// tool each register the same canonical instance through a registry of
    /// their own. When the count lived in the service, every one of those
    /// registries handed that instance a full budget. These two services share
    /// nothing but the instance identity.
    #[test]
    fn one_budget_is_shared_by_services_built_independently_for_one_instance() {
        // Packages unique to this test. The registry is process-wide, so the
        // instance identity is what keeps the assertions deterministic under a
        // parallel test runner.
        const INSTANCE: &str = "egress-shared-ceiling-fixture";
        const OTHER: &str = "egress-other-ceiling-fixture";

        let granted = policy(&["api.example.com"], &[], 1);
        let first_registry = shared_service(granted.clone());
        let second_registry = shared_service(granted);

        let authorize = |service: &EgressHostService, package: &str| {
            let request = EgressRequest::new(
                scope_in_package(package, "main", all_grants()),
                EgressTransport::Tls,
                "api.example.com",
                443,
            )
            .unwrap();
            service.authorize_addresses(request, [addr("1.1.1.1", 443)])
        };

        let held = authorize(&first_registry, INSTANCE).unwrap();
        assert!(
            matches!(
                authorize(&second_registry, INSTANCE),
                Err(EgressError::ConnectionLimitReached { limit: 1, .. })
            ),
            "a second registry must not grant the same instance a second budget"
        );
        assert!(
            authorize(&second_registry, OTHER).is_ok(),
            "the ceiling is shared per instance, not seized process-wide"
        );

        drop(held);
        assert!(
            authorize(&second_registry, INSTANCE).is_ok(),
            "a slot released in one registry must come back in every registry"
        );
    }

    /// The count is shared; the ceiling it is compared against is not cached
    /// with it. An operator who lowers `max_connections_per_instance` binds the
    /// next connection rather than the next restart.
    #[test]
    fn the_connection_ceiling_is_re_read_from_policy_on_every_acquire() {
        let ceiling = Arc::new(AtomicUsize::new(2));
        let configured = Arc::clone(&ceiling);
        let service = EgressHostService::with_private_connection_accounting(
            EgressPolicyResolver::new(move |_| {
                EgressPolicy::new(
                    &owned(&["api.example.com"]),
                    &[],
                    &[],
                    configured.load(Ordering::SeqCst),
                )
            }),
        );
        let authorize = || {
            service.authorize_addresses(
                request("main", EgressTransport::Tls, "api.example.com", 443),
                [addr("1.1.1.1", 443)],
            )
        };

        let held = authorize().unwrap();
        ceiling.store(1, Ordering::SeqCst);
        assert!(
            matches!(
                authorize(),
                Err(EgressError::ConnectionLimitReached { limit: 1, .. })
            ),
            "a lowered ceiling must bind the next acquire"
        );
        drop(held);
        assert!(authorize().is_ok());
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
