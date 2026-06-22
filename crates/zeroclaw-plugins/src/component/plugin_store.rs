// Host-side `Store<T>` data type shared by all three component-model plugin
// worlds (`tool-plugin`, `memory-plugin`, `channel-plugin`).
//
// [`PluginStore`] is the `Store<T>` data type for all three worlds.
// It carries the `WasiCtx` built from the plugin's `fine_grained_permissions`,
// and the `ResourceTable` required by WasiView.

use std::net::IpAddr;
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use serde_json::json;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::sockets::SocketAddrUse;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::{
    HttpResult, WasiHttpCtxView, WasiHttpHooks, WasiHttpView,
    body::HyperOutgoingBody,
    types::{HostFutureIncomingResponse, IncomingResponse, OutgoingRequestConfig},
};
use zeroclaw_log::{Action, Event, EventOutcome, record};

use crate::PluginNetworkConfig;
use crate::error::PluginError;

// ── PluginStore ────────────────────────────────────────────────────────────────

/// Store-data type for all three component plugin worlds.
pub(crate) struct PluginStore {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    http_hooks: PluginHttpHooks,
    table: ResourceTable,
    /// Per-instance proxy/secrets config, exposed to guests read-only via the
    /// `plugin-config` WIT interface (see `v0::plugin_config`).
    pub(crate) network_config: PluginNetworkConfig,
    /// Opaque gateway resume-state blob (see `gateway.wit`'s `save-session`/
    /// `saved-session`). Lives on the store, not the per-connection resource,
    /// so it survives a `close` followed by a fresh `connect` — channel
    /// plugins are the warm-store world, so this persists for the channel
    /// instance's whole lifetime, same as `DiscordGatewaySession` does today.
    pub(crate) gateway_resume_state: Option<String>,
}

impl Default for PluginStore {
    /// Constructs a fully-sandboxed host: no filesystem preopens, all network
    /// disabled. Used for metadata-probe stores where no I/O is needed.
    fn default() -> Self {
        Self {
            wasi: WasiCtxBuilder::new().build(),
            http: WasiHttpCtx::new(),
            http_hooks: PluginHttpHooks::default(),
            table: ResourceTable::new(),
            network_config: PluginNetworkConfig::default(),
            gateway_resume_state: None,
        }
    }
}

/// Proxy-aware, allow-listed `wasi:http` dispatch — shared by all three
/// plugin worlds (tool, memory, channel).
///
/// Unlike the upstream default (`wasmtime_wasi_http::p2::default_send_request`,
/// which opens a raw direct connection), `send_request` here always executes
/// through a `reqwest::Client` built via
/// `zeroclaw_config::schema::build_channel_proxy_client`, so a guest plugin's
/// outbound HTTP automatically honours the same per-plugin `proxy_url`
/// override and global `ProxyConfig` that native channels already get —
/// closing the gap where a plugin given raw `wasi:http` could silently
/// bypass an operator-configured egress proxy.
struct PluginHttpHooks {
    allowed_http_rules: Vec<HttpHostRule>,
    proxy_client: reqwest::Client,
}

impl Default for PluginHttpHooks {
    fn default() -> Self {
        Self {
            allowed_http_rules: Vec::new(),
            proxy_client: reqwest::Client::new(),
        }
    }
}

impl PluginHttpHooks {
    fn new(allowed_http_rules: Vec<HttpHostRule>, proxy_client: reqwest::Client) -> Self {
        Self {
            allowed_http_rules,
            proxy_client,
        }
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

        let client = self.proxy_client.clone();
        let between_bytes_timeout = config.between_bytes_timeout;
        let handle = wasmtime_wasi::runtime::spawn(async move {
            Ok(send_via_proxy_client(client, request, between_bytes_timeout).await)
        });
        Ok(HostFutureIncomingResponse::pending(handle))
    }
}

/// Execute one HTTP request through `client` (already proxy/timeout
/// configured for this plugin instance) and adapt the result into the
/// `IncomingResponse` shape `wasmtime-wasi-http` expects.
///
/// Deliberately buffers the full request and response bodies in memory
/// rather than streaming — simpler to get correct, and adequate for the
/// JSON/small-attachment payloads plugin tool/channel calls are expected to
/// carry. Streaming can be added later if a plugin needs large transfers.
///
/// Retries on 429/5xx, same magnitudes (`MAX_RETRIES`/base/max delay) and
/// shared `zeroclaw_infra::retry` primitives native channels already use
/// (see `zeroclaw-channels/src/webhook.rs`) — this hook was the one
/// outbound caller that hadn't been wired up to the centralized retry
/// logic since it was promoted out of per-channel duplication.
const MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY_MS: u64 = 500;
const RETRY_MAX_DELAY_MS: u64 = 30_000;

async fn send_via_proxy_client(
    client: reqwest::Client,
    request: hyper::Request<HyperOutgoingBody>,
    between_bytes_timeout: std::time::Duration,
) -> Result<IncomingResponse, ErrorCode> {
    let (parts, body) = request.into_parts();

    let url = parts.uri.to_string().parse::<reqwest::Url>().map_err(|e| {
        ErrorCode::InternalError(Some(format!("invalid outgoing request url: {e}")))
    })?;

    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ErrorCode::InternalError(Some(format!("failed reading request body: {e}"))))?
        .to_bytes();

    let (status, headers, body_bytes) = 'attempts: {
        let mut last_err = None;
        for attempt in 0..=MAX_RETRIES {
            let mut req_builder = client
                .request(parts.method.clone(), url.clone())
                .headers(parts.headers.clone());
            if !body_bytes.is_empty() {
                req_builder = req_builder.body(body_bytes.to_vec());
            }

            let resp = match req_builder.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    last_err = Some(e.to_string());
                    continue;
                }
            };

            let status = resp.status();
            if attempt < MAX_RETRIES && zeroclaw_infra::retry::is_retryable_status(status.as_u16())
            {
                let delay = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(zeroclaw_infra::retry::parse_retry_after_ms)
                    .map(std::time::Duration::from_millis)
                    .unwrap_or_else(|| {
                        zeroclaw_infra::retry::compute_backoff(
                            attempt,
                            RETRY_BASE_DELAY_MS,
                            RETRY_MAX_DELAY_MS,
                        )
                    });
                tokio::time::sleep(delay).await;
                continue;
            }

            let headers = resp.headers().clone();
            let body_bytes = resp.bytes().await.map_err(|e| {
                ErrorCode::InternalError(Some(format!("failed reading response body: {e}")))
            })?;
            break 'attempts (status, headers, body_bytes);
        }
        return Err(ErrorCode::InternalError(Some(format!(
            "proxy-aware outbound request failed after {} attempts: {}",
            MAX_RETRIES + 1,
            last_err.unwrap_or_else(|| "exhausted retries on retryable status".to_string())
        ))));
    };

    let response_body = Full::new(body_bytes)
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed_unsync();

    let mut builder = hyper::Response::builder().status(status);
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    let resp = builder.body(response_body).map_err(|e| {
        ErrorCode::InternalError(Some(format!("failed building proxied response: {e}")))
    })?;

    Ok(IncomingResponse {
        resp,
        worker: None,
        between_bytes_timeout,
    })
}

impl PluginStore {
    /// Build a host from a plugin's `fine_grained_permissions` list.
    ///
    /// - `Dir` entries call `WasiCtxBuilder::preopened_dir`.
    /// - `Http` entries only add rules to the `wasi:http` allow-list
    ///   (enforced by `PluginHttpHooks::send_request`); they grant no raw
    ///   socket access, since `wasi:http` outbound requests never touch the
    ///   `wasi:sockets` layer here.
    /// - `Tcp` entries add rules to the raw outbound-TCP allow-list.
    /// - `Udp` entries add rules to the raw outbound-UDP allow-list.
    ///
    /// TCP bind (`TcpBind`) is unconditionally denied; outbound-only TCP is
    /// allowed when matching `Tcp` rules are present — `Http` rules alone do
    /// not enable raw TCP, so an HTTP-only grant cannot be used to open a
    /// direct socket to the same host. If no TCP rules are declared, raw TCP
    /// is fully disabled; same for UDP.
    ///
    /// Address rules:
    /// - IPv4/IPv6 literals are matched exactly at connect time.
    /// - Exact domain names are resolved via async DNS at construction and
    ///   their IPs are matched at connect time.
    /// - Wildcard domain names (e.g. `*.example.com`) are resolved at connect
    ///   time using a reverse-DNS lookup; the resulting hostname is matched
    ///   against the pattern. If reverse DNS fails, the connection is denied.
    pub(crate) async fn with_permissions(
        perms: &[crate::FineGrainedPermission],
        network_config: &PluginNetworkConfig,
    ) -> Result<Self, PluginError> {
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
                        .map_err(PluginError::from)?;
                }
                crate::FineGrainedPermission::Http(addr) => {
                    // Deliberately does not touch `tcp_rules`/`has_tcp`: wasi:http
                    // outbound requests are fully intercepted by
                    // `PluginHttpHooks::send_request` and never reach the
                    // `wasi:sockets` layer, so an `Http` grant must not also
                    // unlock raw TCP connect to the same host.
                    http_rules.push(HttpHostRule::parse(addr)?);
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

        let proxy_client = zeroclaw_config::schema::build_channel_proxy_client(
            &network_config.service_key,
            network_config.proxy_url.as_deref(),
        );

        Ok(Self {
            wasi: builder.build(),
            http: WasiHttpCtx::new(),
            http_hooks: PluginHttpHooks::new(http_rules, proxy_client),
            table: ResourceTable::new(),
            network_config: network_config.clone(),
            gateway_resume_state: None,
        })
    }

    /// Resource table accessor for host implementations of custom resources
    /// (e.g. `websocket`) that live outside this module.
    pub(crate) fn resource_table_mut(&mut self) -> &mut ResourceTable {
        &mut self.table
    }

    /// Whether `url`'s host passes this instance's `FineGrainedPermission::Http`
    /// allow-list — the same check `send_request` applies, reused so
    /// `websocket.connect` cannot reach a host the operator hasn't permitted.
    pub(crate) fn is_url_host_allowed(&self, url: &str) -> bool {
        let Ok(parsed) = url.parse::<http::Uri>() else {
            return false;
        };
        let Some(authority) = parsed.authority() else {
            return false;
        };
        let Some(host) = normalize_authority_host(authority.as_str()) else {
            return false;
        };
        self.http_hooks
            .allowed_http_rules
            .iter()
            .any(|rule| rule.matches_host(&host))
    }

    /// This instance's proxy/timeout-aware HTTP client — the same one
    /// `send_request` dispatches through. Exposed so `http-helpers`'s
    /// `send-multipart`/`download-to-attachment` reuse it rather than
    /// building a second, unproxied client.
    pub(crate) fn proxy_client(&self) -> reqwest::Client {
        self.http_hooks.proxy_client.clone()
    }
}

/// A pre-parsed allow-list entry for outbound HTTP request hosts.
enum HttpHostRule {
    Ip(IpAddr),
    ExactDomain(String),
    WildcardPattern(String),
}

impl HttpHostRule {
    fn parse(addr: &crate::AddressString) -> Result<Self, PluginError> {
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
    async fn parse(addr: &crate::AddressString) -> Result<Self, PluginError> {
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
            .await?
            .map(|sa| sa.ip())
            .collect();
        if ips.is_empty() {
            return Err(PluginError::ResolveFailed(s.to_string()));
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

impl WasiView for PluginStore {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for PluginStore {
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

    /// Spin up a minimal one-shot HTTP/1.1 server on `127.0.0.1` that replies
    /// `200 OK` with a fixed body to the first request it receives. Returns
    /// the bound port and a `JoinHandle` the caller should await.
    async fn spawn_fixed_response_server(body: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            // Just drain whatever the client sent; we don't need to parse it.
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        (port, handle)
    }

    fn empty_outgoing_body() -> HyperOutgoingBody {
        http_body_util::Empty::<bytes::Bytes>::new()
            .map_err(|never: std::convert::Infallible| match never {})
            .boxed_unsync()
    }

    fn test_outgoing_request_config() -> OutgoingRequestConfig {
        OutgoingRequestConfig {
            use_tls: false,
            connect_timeout: std::time::Duration::from_secs(5),
            first_byte_timeout: std::time::Duration::from_secs(5),
            between_bytes_timeout: std::time::Duration::from_secs(5),
        }
    }

    async fn resolve_future_response(
        future: HostFutureIncomingResponse,
    ) -> Result<IncomingResponse, ErrorCode> {
        use wasmtime_wasi::p2::Pollable;
        let mut future = future;
        Pollable::ready(&mut future).await;
        future.unwrap_ready().expect("host future must not trap")
    }

    #[tokio::test]
    async fn send_request_allow_list_denies_unlisted_host() {
        let mut hooks = PluginHttpHooks::new(vec![], reqwest::Client::new());
        let request = hyper::Request::builder()
            .method("GET")
            .uri("http://example.com/")
            .body(empty_outgoing_body())
            .unwrap();

        let err = hooks
            .send_request(request, test_outgoing_request_config())
            .expect_err("host not in allow-list must be denied before any network call");
        assert!(matches!(
            err.downcast().unwrap(),
            ErrorCode::HttpRequestDenied
        ));
    }

    #[tokio::test]
    async fn send_request_allowed_host_reaches_real_server() {
        let (port, server) = spawn_fixed_response_server("hello-from-test-server").await;
        let allowed =
            vec![HttpHostRule::parse(&crate::AddressString::new("127.0.0.1").unwrap()).unwrap()];
        let mut hooks = PluginHttpHooks::new(allowed, reqwest::Client::new());

        let request = hyper::Request::builder()
            .method("GET")
            .uri(format!("http://127.0.0.1:{port}/"))
            .body(empty_outgoing_body())
            .unwrap();

        let future = hooks
            .send_request(request, test_outgoing_request_config())
            .expect("allow-listed host must be permitted");
        let incoming = resolve_future_response(future)
            .await
            .expect("request to a real local server must succeed");
        assert_eq!(incoming.resp.status(), 200);

        let body = incoming
            .resp
            .into_body()
            .collect()
            .await
            .expect("reading response body must succeed")
            .to_bytes();
        assert_eq!(&body[..], b"hello-from-test-server");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn send_request_routes_through_configured_proxy_url() {
        // The allow-listed host (the real server) is never reached directly:
        // pointing `proxy_url` at an address with nothing listening must make
        // the request fail, proving dispatch actually goes through the
        // resolved proxy client rather than connecting straight to the host.
        let (port, server) = spawn_fixed_response_server("should-not-be-reached").await;
        let allowed =
            vec![HttpHostRule::parse(&crate::AddressString::new("127.0.0.1").unwrap()).unwrap()];
        // Port 1 is reserved and nothing will be listening on it; using it as
        // the proxy address is the unrouted endpoint that forces a failure.
        let proxy_client = zeroclaw_config::schema::build_channel_proxy_client(
            "test.plugin",
            Some("http://127.0.0.1:1"),
        );
        let mut hooks = PluginHttpHooks::new(allowed, proxy_client);

        let request = hyper::Request::builder()
            .method("GET")
            .uri(format!("http://127.0.0.1:{port}/"))
            .body(empty_outgoing_body())
            .unwrap();

        let future = hooks
            .send_request(request, test_outgoing_request_config())
            .expect("allow-listed host must be permitted");
        let result = resolve_future_response(future).await;
        assert!(
            result.is_err(),
            "request must fail when routed through an unreachable proxy instead of \
             connecting directly to the allow-listed host"
        );

        drop(server);
    }

    /// Spin up a one-shot HTTP/1.1 server that replies to a sequence of
    /// connections in order, one `(status, body)` pair per connection, then
    /// shuts down. Every response carries `Retry-After: 0` so retry tests
    /// don't pay real backoff delay.
    async fn spawn_sequenced_response_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = zeroclaw_spawn::spawn!(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status} {status}\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (port, handle)
    }

    #[tokio::test]
    async fn send_request_retries_on_429_then_succeeds() {
        let (port, server) =
            spawn_sequenced_response_server(vec![(429, "rate-limited"), (200, "ok-now")]).await;
        let allowed =
            vec![HttpHostRule::parse(&crate::AddressString::new("127.0.0.1").unwrap()).unwrap()];
        let mut hooks = PluginHttpHooks::new(allowed, reqwest::Client::new());

        let request = hyper::Request::builder()
            .method("GET")
            .uri(format!("http://127.0.0.1:{port}/"))
            .body(empty_outgoing_body())
            .unwrap();

        let future = hooks
            .send_request(request, test_outgoing_request_config())
            .expect("allow-listed host must be permitted");
        let incoming = resolve_future_response(future)
            .await
            .expect("request must succeed after retrying past the 429");
        assert_eq!(incoming.resp.status(), 200);

        let body = incoming
            .resp
            .into_body()
            .collect()
            .await
            .expect("reading response body must succeed")
            .to_bytes();
        assert_eq!(&body[..], b"ok-now");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn send_request_surfaces_final_status_after_exhausting_retries() {
        let (port, server) =
            spawn_sequenced_response_server(vec![(503, "1"), (503, "2"), (503, "3"), (503, "4")])
                .await;
        let allowed =
            vec![HttpHostRule::parse(&crate::AddressString::new("127.0.0.1").unwrap()).unwrap()];
        let mut hooks = PluginHttpHooks::new(allowed, reqwest::Client::new());

        let request = hyper::Request::builder()
            .method("GET")
            .uri(format!("http://127.0.0.1:{port}/"))
            .body(empty_outgoing_body())
            .unwrap();

        let future = hooks
            .send_request(request, test_outgoing_request_config())
            .expect("allow-listed host must be permitted");
        let incoming = resolve_future_response(future)
            .await
            .expect("the final attempt's response must still be surfaced, not an error");
        assert_eq!(incoming.resp.status(), 503);

        let body = incoming
            .resp
            .into_body()
            .collect()
            .await
            .expect("reading response body must succeed")
            .to_bytes();
        assert_eq!(
            &body[..],
            b"4",
            "must surface the 4th (last) attempt's response"
        );
        server.await.unwrap();
    }
}
