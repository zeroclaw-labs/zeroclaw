// Host-side WIT implementation for all three component-model plugin worlds
// (`tool-plugin`, `memory-plugin`, `channel-plugin`).
//
// [`PluginHost`] is the `Store<T>` data type for all three worlds.
// It carries the `WasiCtx` built from the plugin's `fine_grained_permissions`,
// and the `ResourceTable` required by WasiView.

use std::net::IpAddr;
use std::sync::Arc;

use serde_json::json;
use wasmtime::component::{HasSelf, ResourceTable};
use wasmtime_wasi::sockets::SocketAddrUse;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::{
    self, HttpResult, WasiHttpCtxView, WasiHttpHooks, WasiHttpView, body::HyperOutgoingBody,
    types::HostFutureIncomingResponse, types::OutgoingRequestConfig,
};
use zeroclaw_log::{Action, Event, EventOutcome, record};

use super::bindings;

// ── PluginHost ────────────────────────────────────────────────────────────────

/// Store-data type for all three component plugin worlds.
pub struct PluginHost {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    http_hooks: PluginHttpHooks,
    table: ResourceTable,
}

impl Default for PluginHost {
    /// Constructs a fully-sandboxed host: no filesystem preopens, all network
    /// disabled. Used for metadata-probe stores where no I/O is needed.
    fn default() -> Self {
        Self {
            wasi: WasiCtxBuilder::new().build(),
            http: WasiHttpCtx::new(),
            http_hooks: PluginHttpHooks::default(),
            table: ResourceTable::new(),
        }
    }
}

#[derive(Default)]
struct PluginHttpHooks {
    allowed_http_rules: Vec<HttpHostRule>,
}

impl PluginHttpHooks {
    fn new(allowed_http_rules: Vec<HttpHostRule>) -> Self {
        Self { allowed_http_rules }
    }
}

impl WasiHttpHooks for PluginHttpHooks {
    fn send_request(
        &mut self,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        let Some(authority) = request.uri().authority() else {
            return Err(ErrorCode::HttpRequestUriInvalid.into());
        };
        let Some(host) = normalize_authority_host(authority.as_str()) else {
            return Err(ErrorCode::HttpRequestUriInvalid.into());
        };

        if !self
            .allowed_http_rules
            .iter()
            .any(|rule| rule.matches_host(&host))
        {
            record!(
                WARN,
                Event::new(module_path!(), Action::Reject)
                    .with_outcome(EventOutcome::Failure)
                    .with_attrs(json!({ "host": host, "authority": authority.as_str() })),
                "outbound HTTP request denied by fine-grained permission allow-list"
            );
            return Err(ErrorCode::HttpRequestDenied.into());
        }

        Ok(p2::default_send_request(request, config))
    }
}

impl PluginHost {
    /// Build a host from a plugin's `fine_grained_permissions` list.
    ///
    /// - `Dir` entries call `WasiCtxBuilder::preopened_dir`.
    /// - `Http` + `Tcp` entries add rules to the TCP allow-list.
    /// - `Udp` entries add rules to the UDP allow-list.
    ///
    /// TCP bind (`TcpBind`) is unconditionally denied; outbound-only TCP is
    /// allowed when matching rules are present. If no TCP/HTTP rules are
    /// declared TCP is fully disabled; same for UDP.
    ///
    /// Address rules:
    /// - IPv4/IPv6 literals are matched exactly at connect time.
    /// - Exact domain names are resolved via async DNS at construction and
    ///   their IPs are matched at connect time.
    /// - Wildcard domain names (e.g. `*.example.com`) are resolved at connect
    ///   time using a reverse-DNS lookup; the resulting hostname is matched
    ///   against the pattern. If reverse DNS fails, the connection is denied.
    pub async fn with_permissions(perms: &[crate::FineGrainedPermission]) -> anyhow::Result<Self> {
        let mut builder = WasiCtxBuilder::new();

        let mut http_rules: Vec<HttpHostRule> = Vec::new();
        let mut tcp_rules: Vec<AddrRule> = Vec::new();
        let mut udp_rules: Vec<AddrRule> = Vec::new();
        let mut has_tcp = false;
        let mut has_udp = false;
        let mut has_domain_lookup = false;

        for perm in perms {
            match perm {
                crate::FineGrainedPermission::Dir(dir) => {
                    let dir_perms = match (dir.dir_read, dir.dir_write) {
                        (true, true) => DirPerms::all(),
                        (true, false) => DirPerms::READ,
                        (false, true) => DirPerms::MUTATE,
                        (false, false) => DirPerms::empty(),
                    };
                    let file_perms = match (dir.file_read, dir.file_write) {
                        (true, true) => FilePerms::all(),
                        (true, false) => FilePerms::READ,
                        (false, true) => FilePerms::WRITE,
                        (false, false) => FilePerms::empty(),
                    };
                    builder
                        .preopened_dir(&dir.host_path, &dir.guest_path, dir_perms, file_perms)
                        .map_err(|e| anyhow::Error::msg(format!("{e}")))?;
                }
                crate::FineGrainedPermission::Http(addr) => {
                    http_rules.push(HttpHostRule::parse(addr)?);
                    has_tcp = true;
                    if !addr.is_wildcard() {
                        has_domain_lookup =
                            has_domain_lookup || addr.as_str().parse::<IpAddr>().is_err();
                    }
                    tcp_rules.push(AddrRule::parse(addr).await?);
                }
                crate::FineGrainedPermission::Tcp(addr) => {
                    has_tcp = true;
                    if !addr.is_wildcard() {
                        has_domain_lookup =
                            has_domain_lookup || addr.as_str().parse::<IpAddr>().is_err();
                    }
                    tcp_rules.push(AddrRule::parse(addr).await?);
                }
                crate::FineGrainedPermission::Udp(addr) => {
                    has_udp = true;
                    if !addr.is_wildcard() {
                        has_domain_lookup =
                            has_domain_lookup || addr.as_str().parse::<IpAddr>().is_err();
                    }
                    udp_rules.push(AddrRule::parse(addr).await?);
                }
            }
        }

        builder.allow_tcp(has_tcp);
        builder.allow_udp(has_udp);
        // Enable ip-name-lookup if any domain-based (non-IP) permissions are
        // present so the plugin can resolve the names it needs.
        if has_domain_lookup {
            builder.allow_ip_name_lookup(true);
        }

        if has_tcp || has_udp {
            let tcp_rules = Arc::new(tcp_rules);
            let udp_rules = Arc::new(udp_rules);
            builder.socket_addr_check(move |socket_addr, use_kind| {
                let tcp = Arc::clone(&tcp_rules);
                let udp = Arc::clone(&udp_rules);
                let ip = socket_addr.ip();
                Box::pin(async move {
                    match use_kind {
                        // Never allow inbound server sockets.
                        SocketAddrUse::TcpBind => false,
                        SocketAddrUse::TcpConnect => addr_matches(&tcp, ip).await,
                        SocketAddrUse::UdpBind
                        | SocketAddrUse::UdpConnect
                        | SocketAddrUse::UdpOutgoingDatagram => addr_matches(&udp, ip).await,
                    }
                })
            });
        }

        Ok(Self {
            wasi: builder.build(),
            http: WasiHttpCtx::new(),
            http_hooks: PluginHttpHooks::new(http_rules),
            table: ResourceTable::new(),
        })
    }
}

/// A pre-parsed allow-list entry for outbound HTTP request hosts.
enum HttpHostRule {
    Ip(IpAddr),
    ExactDomain(String),
    WildcardPattern(String),
}

impl HttpHostRule {
    fn parse(addr: &crate::AddressString) -> anyhow::Result<Self> {
        let s = addr.as_str();
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Ok(Self::Ip(ip));
        }
        if addr.is_wildcard() {
            return Ok(Self::WildcardPattern(s.to_lowercase()));
        }
        Ok(Self::ExactDomain(s.to_lowercase()))
    }

    fn matches_host(&self, host: &str) -> bool {
        match self {
            Self::Ip(allowed) => host.parse::<IpAddr>().is_ok_and(|ip| ip == *allowed),
            Self::ExactDomain(allowed) => host.eq_ignore_ascii_case(allowed),
            Self::WildcardPattern(pattern) => wildcard_matches(host, pattern),
        }
    }
}

// ── AddrRule ──────────────────────────────────────────────────────────────────

/// A pre-parsed rule for the socket address check.
enum AddrRule {
    /// An explicit IP address literal.
    Ip(IpAddr),
    /// An exact domain, pre-resolved to one or more IPs at construction.
    ResolvedDomain(Arc<[IpAddr]>),
    /// A wildcard domain pattern (e.g. `*.example.com`).  Enforced via
    /// reverse-DNS lookup at connect time.
    WildcardPattern(String),
}

impl AddrRule {
    async fn parse(addr: &crate::AddressString) -> anyhow::Result<Self> {
        let s = addr.as_str();
        // IP literal
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Ok(Self::Ip(ip));
        }
        // Wildcard domain — cannot pre-resolve
        if addr.is_wildcard() {
            record!(
                WARN,
                Event::new(module_path!(), Action::Note).with_attrs(json!({ "address": s })),
                "wildcard domain permission: enforcement uses reverse-DNS at connect time; connections are denied if reverse lookup fails"
            );
            return Ok(Self::WildcardPattern(s.to_string()));
        }
        // Exact domain — resolve async
        use tokio::net::lookup_host;
        let ips: Arc<[IpAddr]> = lookup_host(format!("{s}:0"))
            .await
            .map_err(|e| anyhow::Error::msg(format!("failed to resolve '{s}': {e}")))?
            .map(|sa| sa.ip())
            .collect();
        if ips.is_empty() {
            anyhow::bail!("domain '{s}' resolved to no addresses");
        }
        Ok(Self::ResolvedDomain(ips))
    }
}

/// Check `ip` against `rules` — used inside the async `socket_addr_check`.
async fn addr_matches(rules: &[AddrRule], ip: IpAddr) -> bool {
    for rule in rules {
        match rule {
            AddrRule::Ip(allowed) => {
                if *allowed == ip {
                    return true;
                }
            }
            AddrRule::ResolvedDomain(ips) => {
                if ips.contains(&ip) {
                    return true;
                }
            }
            AddrRule::WildcardPattern(pattern) => {
                // Reverse-DNS lookup: run blocking call off the async thread.
                let ip_owned = ip;
                let pattern_owned = pattern.clone();
                let hostname =
                    tokio::task::spawn_blocking(move || dns_lookup::lookup_addr(&ip_owned).ok())
                        .await
                        .ok()
                        .flatten();
                if let Some(ref h) = hostname
                    && wildcard_matches(h, &pattern_owned)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Returns `true` if `hostname` matches `pattern`.
///
/// `pattern` may contain `*` in labels at level 3+. Examples:
/// - `*.example.com` matches `foo.example.com` but not `bar.foo.example.com`.
/// - `id-*.docs.example.com` matches `id-123.docs.example.com`.
fn wildcard_matches(hostname: &str, pattern: &str) -> bool {
    let h_parts: Vec<&str> = hostname.trim_end_matches('.').split('.').collect();
    let p_parts: Vec<&str> = pattern.split('.').collect();
    if h_parts.len() != p_parts.len() {
        return false;
    }
    h_parts.iter().zip(p_parts.iter()).all(|(h, p)| {
        if p.contains('*') {
            // Convert glob to a simple prefix/suffix/exact match.
            label_matches_glob(h, p)
        } else {
            h.eq_ignore_ascii_case(p)
        }
    })
}

/// Match a single DNS label against a glob pattern that may contain `*`.
fn label_matches_glob(label: &str, glob: &str) -> bool {
    // Split on '*'; all non-star fragments must appear in order.
    let mut remaining = label;
    let mut parts = glob.split('*').peekable();
    let mut first = true;
    while let Some(part) = parts.next() {
        if first {
            first = false;
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else if parts.peek().is_none() {
            // Last segment: must be a suffix.
            if !remaining.ends_with(part) {
                return false;
            }
            remaining = &remaining[..remaining.len() - part.len()];
        } else {
            // Middle segment: find the next occurrence.
            if let Some(pos) = remaining.find(part) {
                remaining = &remaining[pos + part.len()..];
            } else {
                return false;
            }
        }
    }
    true
}

impl WasiView for PluginHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for PluginHost {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.http_hooks,
        }
    }
}

fn normalize_authority_host(authority: &str) -> Option<String> {
    let trimmed = authority.trim();
    if trimmed.is_empty() || trimmed.contains('@') {
        return None;
    }

    let host = if trimmed.starts_with('[') {
        let end = trimmed.find(']')?;
        if end + 1 < trimmed.len() && !trimmed[end + 1..].starts_with(':') {
            return None;
        }
        &trimmed[1..end]
    } else {
        trimmed.split(':').next().unwrap_or(trimmed)
    };

    let normalized = host.trim_end_matches('.');
    if normalized.is_empty() {
        return None;
    }

    Some(normalized.to_lowercase())
}

// ── types::Host (empty marker trait) ─────────────────────────────────────────

impl bindings::tool::zeroclaw::plugin::types::Host for PluginHost {}
impl bindings::memory::zeroclaw::plugin::types::Host for PluginHost {}
impl bindings::channel::zeroclaw::plugin::types::Host for PluginHost {}

// ── Linker wiring helpers ─────────────────────────────────────────────────────

/// Wire all host interfaces for the `tool-plugin` world into `linker`.
pub fn add_to_linker_tool(
    linker: &mut wasmtime::component::Linker<PluginHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::tool::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::tool::ToolPlugin::add_to_linker::<PluginHost, HasSelf<PluginHost>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

/// Wire all host interfaces for the `memory-plugin` world into `linker`.
pub fn add_to_linker_memory(
    linker: &mut wasmtime::component::Linker<PluginHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::memory::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::memory::MemoryPlugin::add_to_linker::<PluginHost, HasSelf<PluginHost>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

/// Wire all host interfaces for the `channel-plugin` world into `linker`.
pub fn add_to_linker_channel(
    linker: &mut wasmtime::component::Linker<PluginHost>,
) -> anyhow::Result<()> {
    // Use feature flags to allow developers to link in wit bindings that aren't stabilized yet.
    let mut options = crate::component::v0::bindings::channel::LinkOptions::default();
    #[cfg(feature = "plugins-wit-v0")]
    {
        options.plugins_wit_v0(true);
    }
    bindings::channel::ChannelPlugin::add_to_linker::<PluginHost, HasSelf<PluginHost>>(
        linker,
        &options,
        |x| x,
    )
    .map_err(crate::error::PluginError::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_authority_host_handles_common_shapes() {
        assert_eq!(
            normalize_authority_host("example.com:443").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            normalize_authority_host("[::1]:8080").as_deref(),
            Some("::1")
        );
        assert_eq!(
            normalize_authority_host("example.com.").as_deref(),
            Some("example.com")
        );
        assert!(normalize_authority_host("bad@host").is_none());
    }

    #[test]
    fn http_host_rules_match_exact_and_wildcard_hosts() {
        let exact =
            HttpHostRule::parse(&crate::AddressString::new("Example.COM").unwrap()).unwrap();
        let wildcard =
            HttpHostRule::parse(&crate::AddressString::new("*.example.com").unwrap()).unwrap();

        assert!(exact.matches_host("example.com"));
        assert!(wildcard.matches_host("api.example.com"));
        assert!(!wildcard.matches_host("example.com"));
    }
}
