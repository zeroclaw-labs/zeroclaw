//! URL/network policy for MCP OAuth.
//!
//! One explicit policy seam for every URL the OAuth flow may touch: the
//! configured MCP URL, protected-resource metadata, authorization-server
//! metadata, registration, token, and server-advertised SSE message
//! endpoints. Defaults are closed:
//!
//! - `https` everywhere; plain `http` only for the exact loopback hosts
//!   (127.0.0.1, localhost, [::1]) so local mock servers and stdio-style dev
//!   setups still work without opening the internet to cleartext;
//! - no userinfo, no fragment, host required;
//! - DNS answers are validated and pinned before connect, so a first lookup
//!   cannot advertise a public host and the connection then rebind to cloud
//!   metadata; validation runs through the crate-wide `net_guard` (the same
//!   classifier every other network-reaching tool uses), including decoding
//!   of NAT64-translated answers under the well-known `64:ff9b::/96` and the
//!   operator-declared `security.nat64_prefixes`;
//! - private/loopback/CGNAT/ULA addresses are trusted only for the HOST the
//!   operator configured (the configured MCP URL anchors that trust): a LAN
//!   MCP server with a same-host IdP is a supported shape, but
//!   server-supplied metadata pointing OAuth traffic at another origin must
//!   resolve to a globally routable address. Cloud-metadata addresses — the
//!   whole IPv4 `169.254.0.0/16` band and the documented provider endpoints,
//!   including NAT64-embedded forms — are rejected everywhere regardless of
//!   trust;
//! - the OAuth fetch client follows **no redirects at all**, and bearer
//!   material never rides along (tokens are only attached by the transport to
//!   the resource origin, never to these discovery URLs);
//! - a server-advertised SSE message endpoint must share the configured
//!   URL's origin: an advertised endpoint is untrusted server output and may
//!   not move the session to another host.

use anyhow::{Context, Result, bail};
use reqwest::Url;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use zeroclaw_config::schema::McpServerConfig;
use zeroclaw_infra::net_guard::Nat64Prefix;

use crate::helpers::domain_guard;

/// OAuth metadata documents are small; anything bigger is treated as a
/// hostile or confused endpoint.
pub const MAX_METADATA_BYTES: u64 = 64 * 1024;

/// Request timeouts for OAuth metadata/token fetches. Connect is bounded
/// tightly; the whole exchange is bounded and callers additionally wrap in a
/// cancellable timeout.
const OAUTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OAUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Exact loopback hosts allowed for plain `http` (case-insensitive; `Url`
/// already strips IPv6 brackets).
pub fn is_exact_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

/// `scheme://host[:port]` with default ports normalized away. Two URLs share
/// an origin iff these keys match.
pub fn origin_key(url: &Url) -> Result<String> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported URL scheme {:?}", url.scheme());
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .context("URL has no host")?;
    let port = match (url.scheme(), url.port()) {
        (_, Some(port)) => Some(port),
        // `Url::port()` elides the default; keep the normalization explicit so
        // callers cannot bypass it with a hand-built URL string.
        ("http", None) => Some(80),
        ("https", None) => Some(443),
        _ => None,
    };
    Ok(match port {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// Validate a URL the OAuth flow wants to touch. `what` names the endpoint
/// class for the actionable error (and stays free of credentials).
pub fn ensure_oauth_url(url: &Url, what: &str) -> Result<()> {
    match url.scheme() {
        "https" => {}
        "http" => {
            let host = url
                .host_str()
                .filter(|host| !host.is_empty())
                .with_context(|| format!("{what} URL has no host"))?;
            if !is_exact_loopback_host(host) {
                bail!("{what} URL may only use http for exact loopback hosts: {url}");
            }
        }
        scheme => bail!("{what} URL must use https (or http on loopback), got {scheme:?}"),
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{what} URL must not carry userinfo");
    }
    if url.fragment().is_some() {
        bail!("{what} URL must not carry a fragment");
    }
    if url.host_str().filter(|host| !host.is_empty()).is_none() {
        bail!("{what} URL has no host");
    }
    Ok(())
}

/// Reject a server-advertised SSE message endpoint that does not share the
/// configured MCP URL's origin. The configured URL is trusted operator
/// input; the advertised endpoint is not.
pub fn validate_advertised_message_endpoint(configured: &Url, advertised: &Url) -> Result<()> {
    ensure_oauth_url(advertised, "SSE message endpoint")?;
    if origin_key(configured)? != origin_key(advertised)? {
        bail!(
            "advertised SSE message endpoint must share the configured MCP origin, got {} for {}",
            advertised,
            configured
        );
    }
    Ok(())
}

/// Reject resolved DNS answers that must never receive MCP OAuth traffic,
/// using the crate-wide SSRF guard (`zeroclaw_infra::net_guard` via
/// `helpers::domain_guard`) — the same classifier every other network tool
/// in this crate goes through, including the `a2a_client` this module
/// mirrors. `nat64_prefixes` carries the operator-declared
/// `security.nat64_prefixes` translators, so a v6 answer embedding a private
/// or metadata IPv4 cannot slip through a translation layer.
///
/// `allow_non_public` encodes the trust boundary of S1-1: an address that is
/// loopback/RFC1918/CGNAT/ULA is only acceptable when the operator chose the
/// host — i.e. the target shares its HOST with the configured MCP URL (a
/// private-network IdP next to a private MCP server, or a local mock pair on
/// one loopback host). A PUBLIC configured MCP server pointing its metadata
/// at a private address is the SSRF shape, so with `allow_non_public=false`
/// every non-globally-routable answer is rejected.
///
/// Mode mapping (intentionally the shared guard's semantics, not a bespoke
/// list): trusted targets get `validate_resolved_ips_exclude_metadata` —
/// cloud metadata (the whole v4 `169.254.0.0/16` link-local band, the
/// documented provider endpoints e.g. Azure/GCP/Alibaba/AWS-v6, and anything
/// NAT64-embedded under a declared prefix) stays blocked, while other
/// private-space answers pass; untrusted targets get
/// `validate_resolved_ips_are_public`, which additionally rejects every
/// non-globally-reachable IANA special-purpose range (loopback, RFC1918,
/// CGNAT, `0.0.0.0/8`, benchmarking, documentation, reserved, multicast) and
/// unspecified v6.
pub fn validate_resolved_oauth_ips(
    host: &str,
    ips: &[IpAddr],
    allow_non_public: bool,
    nat64_prefixes: &[Nat64Prefix],
) -> Result<()> {
    if allow_non_public {
        domain_guard::validate_resolved_ips_exclude_metadata(host, ips, nat64_prefixes)
    } else {
        domain_guard::validate_resolved_ips_are_public(host, ips, nat64_prefixes)
    }
}

/// The trusted host for the S1-1 boundary: the HOST of the configured MCP
/// URL. Matching HOST (not origin) is deliberate — a same-host IdP on
/// another port, typical of private deployments and local dev pairs, stays
/// reachable, while any other host must be public.
fn trusted_oauth_host(trusted_origin: &Url) -> Option<String> {
    trusted_origin
        .host_str()
        .filter(|host| !host.is_empty())
        .map(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .to_ascii_lowercase()
        })
}

fn target_is_trusted_private(url: &Url, trusted_origin: &Url) -> bool {
    match (url.host_str(), trusted_oauth_host(trusted_origin)) {
        (Some(host), Some(trusted)) => {
            let host = host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_ascii_lowercase();
            host == trusted
                // MINOR-1: `localhost`, `127.0.0.1`, and `::1` are ONE
                // machine; a dev pair configured as MCP-on-localhost with
                // its AS-on-127.0.0.1 (or containers on either spelling)
                // must stay working. This unification deliberately covers
                // loopback only — RFC1918/CGNAT/ULA targets still require
                // the exact host match, so a private target never inherits
                // trust from a different loopback spelling.
                || (is_exact_loopback_host(&host) && is_exact_loopback_host(&trusted))
        }
        _ => false,
    }
}

/// Resolve `url`'s host, validate every answer against the SSRF posture, and
/// return the pin list (empty for IP literals, which need no pinning).
/// `trusted_origin` is the configured MCP URL (see
/// [`validate_resolved_oauth_ips`]).
pub async fn resolve_and_pin_oauth_target(
    url: &Url,
    trusted_origin: &Url,
    nat64_prefixes: &[Nat64Prefix],
) -> Result<Vec<SocketAddr>> {
    // `Url::host()` decodes the host properly, so IPv6 literals arrive as
    // IpAddr, not as the bracketed "[::1]" string that `host_str` gives
    // (S1-2: `"[::1]".parse::<IpAddr>()` fails, which silently rerouted
    // IPv6 literals through a doomed DNS lookup).
    let allow_non_public = target_is_trusted_private(url, trusted_origin);
    match url.host() {
        // Bindings split: v4 and v6 addresses are different types behind the
        // same `Host` enum, so each arm coerces to `IpAddr` on its own.
        Some(url::Host::Ipv4(ip)) => {
            // IP literal: reqwest connects directly; still validate the literal.
            validate_resolved_oauth_ips(
                url.as_ref(),
                &[IpAddr::V4(ip)],
                allow_non_public,
                nat64_prefixes,
            )?;
            return Ok(Vec::new());
        }
        Some(url::Host::Ipv6(ip)) => {
            validate_resolved_oauth_ips(
                url.as_ref(),
                &[IpAddr::V6(ip)],
                allow_non_public,
                nat64_prefixes,
            )?;
            return Ok(Vec::new());
        }
        Some(url::Host::Domain(_)) | None => {}
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .context("OAuth URL has no host")?;
    let port = url
        .port_or_known_default()
        .context("OAuth URL has no port")?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("DNS resolution failed for {host}"))?
        .collect();
    let ips: Vec<IpAddr> = addrs.iter().map(SocketAddr::ip).collect();
    validate_resolved_oauth_ips(host, &ips, allow_non_public, nat64_prefixes)?;
    Ok(addrs)
}

/// True when a configured runtime proxy (not plain environment proxies)
/// applies to MCP. Same canonical read as the a2a client
/// (`a2a_client.rs:447-456`): the live `runtime_proxy_config()`, service
/// scope check, and any non-empty proxy URL.
fn runtime_proxy_active_for_mcp() -> bool {
    let config = zeroclaw_config::schema::runtime_proxy_config();
    if !config.should_apply_to_service("tool.mcp") {
        return false;
    }
    [&config.http_proxy, &config.https_proxy, &config.all_proxy]
        .into_iter()
        .flatten()
        .any(|url| !url.trim().is_empty())
}

/// Build the HTTP client for ONE OAuth target URL (metadata discovery,
/// dynamic registration, token exchange). Per-target construction is the
/// point: every URL the flow wants to fetch — including URLs that came from
/// server-supplied metadata — gets its host validated and DNS-pinned before
/// the connection. Posture: no redirects; DNS answers rejected per
/// [`validate_resolved_oauth_ips`] (cloud metadata always; all
/// non-globally-routable space for targets other than the configured host);
/// pinned addrs, and the server's additive custom CA exactly like its MCP
/// transport.
///
/// Proxy posture mirrors `a2a_client::pinned_client` (a2a_client.rs:336-400):
/// an active *runtime* proxy resolves hostnames on the proxy side, where the
/// local pin cannot hold, so the flow fails closed instead of shipping a
/// silent SSRF hole; the direct path uses `.no_proxy()` so inherited
/// environment proxies cannot route around the pin either.
///
/// `trusted_origin` is the CONFIGURED MCP URL — the only host the operator
/// vouched for. Private/loopback DNS answers are accepted only for targets
/// whose host matches it; server-supplied metadata pointing anywhere else
/// must resolve to a public address (S1-1).
pub async fn build_oauth_http_client(
    config: &McpServerConfig,
    target: &Url,
    trusted_origin: &Url,
    nat64_prefixes: &[Nat64Prefix],
) -> Result<reqwest::Client> {
    ensure_oauth_url(target, "OAuth endpoint")?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(OAUTH_CONNECT_TIMEOUT)
        .timeout(OAUTH_REQUEST_TIMEOUT)
        .no_proxy();
    if let Some(ca_path) = config.tls_ca_cert_path.as_deref() {
        crate::mcp_transport::require_https_url(&config.name, target.as_str(), "OAuth endpoint")?;
        let pem = crate::mcp_transport::load_tls_ca_pem(config, ca_path)?;
        let bundle = reqwest::Certificate::from_pem_bundle(&pem)
            .context("OAuth custom CA bundle contained no parseable certificates")?;
        if bundle.is_empty() {
            bail!("OAuth custom CA bundle at {ca_path} is empty");
        }
        for cert in bundle {
            builder = builder.add_root_certificate(cert);
        }
    }
    let addrs = resolve_and_pin_oauth_target(target, trusted_origin, nat64_prefixes).await?;
    let proxied = runtime_proxy_active_for_mcp();
    match url_host_kind_literal(target) {
        // Hostname branch: the local pin is only authoritative without a
        // proxy; a proxy would resolve (and rebind) on its own side.
        // Both hostname arms run after `resolve_and_pin_oauth_target` succeeded,
        // which already rejected any host-less or empty-host target, so
        // `host_str()` is guaranteed `Some` here; `unwrap_or_default` only
        // keeps that a non-panic path (NIT-2: the previous "target" fallback
        // was an unreachable fake value).
        None if proxied => {
            let host = target.host_str().unwrap_or_default();
            bail!(
                "MCP OAuth for {host}: a runtime proxy is active for MCP, which \
                 resolves hostnames on the proxy side; the local DNS pin cannot \
                 guarantee the SSRF-safe destination. Remove `tool.mcp` (or the \
                 `tool.*`/zeroclaw-wide scope that selected it) from the proxy \
                 services to use OAuth over a pinned direct connection"
            );
        }
        None => {
            let host = target.host_str().unwrap_or_default();
            builder = builder.resolve_to_addrs(host, &addrs);
        }
        // Literal-IP branch: no DNS to pin, but routing must still be direct.
        Some(()) if proxied => {
            bail!(
                "MCP OAuth literal-IP target {target}: a runtime proxy is active \
                 for MCP; the direct no-proxy path is required to guarantee the destination"
            );
        }
        Some(()) => {}
    }
    builder.build().context("failed to build OAuth HTTP client")
}

/// True when `url`'s host is an IP literal (v4 or v6), via the decoding
/// `Url::host()` accessor (S1-2: never string-parse `host_str`).
fn url_host_kind_literal(url: &Url) -> Option<()> {
    match url.host() {
        Some(url::Host::Ipv4(_)) | Some(url::Host::Ipv6(_)) => Some(()),
        _ => None,
    }
}

/// A reusable per-server factory producing one validated, DNS-pinned,
/// no-redirect client per target URL. Discovery, registration, and token
/// exchange each touch server-supplied URLs, so every fetch must go through
/// [`OAuthClientFactory::client_for`] rather than sharing one client across
/// hosts. Deliberately no `Debug`: the held config carries decrypted
/// `headers` secret material.
#[derive(Clone)]
pub struct OAuthClientFactory {
    config: McpServerConfig,
    /// Operator-declared `security.nat64_prefixes`, threaded from the process
    /// config at manager construction. Empty for embedders/tests without a
    /// security section: the well-known `64:ff9b::/96` decoding still applies
    /// in the shared guard, network-specific translators just stay opaque.
    nat64_prefixes: Vec<Nat64Prefix>,
}

impl OAuthClientFactory {
    pub fn new(config: &McpServerConfig) -> Self {
        Self {
            config: config.clone(),
            nat64_prefixes: Vec::new(),
        }
    }

    /// The production constructor: same as [`Self::new`] plus the parsed
    /// `security.nat64_prefixes` from the process [`zeroclaw_config::schema::Config`].
    pub fn new_with_nat64(config: &McpServerConfig, nat64_prefixes: Vec<Nat64Prefix>) -> Self {
        Self {
            config: config.clone(),
            nat64_prefixes,
        }
    }

    /// The configured MCP URL this factory's flow is bound to: the trusted
    /// origin for the S1-1 private-address boundary. Canonical from the
    /// config the factory was built with — deliberately NOT a parameter, so
    /// no caller can pass the discovered endpoint itself and make the check
    /// meaningless.
    fn trusted_origin(&self) -> Result<Url> {
        let raw = self.config.url.as_deref().with_context(|| {
            format!(
                "MCP server `{}`: no URL to anchor OAuth trust",
                self.config.name
            )
        })?;
        Url::parse(raw).with_context(|| {
            format!(
                "MCP server `{}`: configured URL is unparsable",
                self.config.name
            )
        })
    }

    /// Validate + pin, then build. Per-URL construction is intentional:
    /// OAuth fetches are rare, and per-target pinning is a security
    /// requirement, not a micro-optimization.
    pub async fn client_for(&self, target: &Url) -> Result<reqwest::Client> {
        let trusted = self.trusted_origin()?;
        build_oauth_http_client(&self.config, target, &trusted, &self.nat64_prefixes).await
    }

    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("fixture URL parses")
    }

    #[test]
    fn https_allowed_anywhere_http_only_on_exact_loopback() {
        ensure_oauth_url(&url("https://mcp.example.com/mcp"), "probe").expect("https ok");
        for host in ["127.0.0.1", "localhost", "[::1]", "LOCALHOST"] {
            ensure_oauth_url(
                &Url::parse(&format!("http://{host}:9000/mcp")).expect("fixture URL parses"),
                "probe",
            )
            .unwrap_or_else(|err| panic!("loopback http for {host} must pass: {err}"));
        }
        ensure_oauth_url(&url("http://mcp.example.com/mcp"), "probe")
            .expect_err("non-loopback http must be rejected");
        ensure_oauth_url(&url("http://localhost.example.com/mcp"), "probe")
            .expect_err("loopback lookalikes are not exact loopback");
        ensure_oauth_url(&url("http://127.0.0.2:9000/mcp"), "probe")
            .expect_err("only 127.0.0.1 is the exact loopback host");
        ensure_oauth_url(&url("ftp://host/mcp"), "probe").expect_err("scheme");
    }

    #[test]
    fn userinfo_and_fragment_rejected() {
        ensure_oauth_url(&url("https://u@host/x"), "probe").expect_err("userinfo");
        ensure_oauth_url(&url("https://host/x#f"), "probe").expect_err("fragment");
    }

    #[test]
    fn origin_key_normalizes_default_ports() {
        assert_eq!(
            origin_key(&url("https://host/x")).expect("ok"),
            "https://host:443"
        );
        assert_eq!(
            origin_key(&url("https://host:443/y")).expect("ok"),
            origin_key(&url("https://host/z")).expect("ok")
        );
        assert_ne!(
            origin_key(&url("https://host:8443/x")).expect("ok"),
            origin_key(&url("https://host/x")).expect("ok")
        );
    }

    #[test]
    fn advertised_endpoint_must_share_origin() {
        let configured = url("https://mcp.example.com/mcp");
        validate_advertised_message_endpoint(
            &configured,
            &url("https://mcp.example.com/sse/messages"),
        )
        .expect("same origin ok");
        validate_advertised_message_endpoint(
            &configured,
            &url("https://evil.example.com/messages"),
        )
        .expect_err("cross-origin rejected");
        validate_advertised_message_endpoint(&configured, &url("http://mcp.example.com/messages"))
            .expect_err("downgrade rejected");
        validate_advertised_message_endpoint(&configured, &url("https://mcp.example.com:8443/msg"))
            .expect_err("port change rejected");
    }

    #[test]
    fn resolved_ip_validation_blocks_metadata_linklocal_and_empty() {
        use std::net::Ipv4Addr;
        let meta: IpAddr = Ipv4Addr::new(169, 254, 169, 254).into();
        let link: IpAddr = Ipv4Addr::new(169, 254, 1, 1).into();
        let public: IpAddr = Ipv4Addr::new(93, 184, 216, 34).into();
        let private: IpAddr = Ipv4Addr::new(10, 0, 0, 5).into();
        // `true` = the host was vouched for by the operator (configured MCP
        // URL); the non-public relaxation only applies under that trust
        // (S1-1), which these unit cases simulate. Semantics are the shared
        // `net_guard` pair: metadata (all of 169.254/16) is refused even
        // under trust; non-global space only under trust.
        validate_resolved_oauth_ips("host", &[public], true, &[]).expect("public ok");
        validate_resolved_oauth_ips("host", &[private], true, &[]).expect("operator LAN ok");
        validate_resolved_oauth_ips("host", &[private], false, &[])
            .expect_err("server-supplied target may not be RFC1918 (S1-1)");
        validate_resolved_oauth_ips("host", &[meta], true, &[]).expect_err("metadata");
        validate_resolved_oauth_ips("host", &[link], true, &[]).expect_err("link-local");
        validate_resolved_oauth_ips("host", &[public, meta], true, &[])
            .expect_err("one bad answer poisons");
        validate_resolved_oauth_ips("host", &[], true, &[]).expect_err("empty answers");
        // v6 link-local: under trust the shared guard's metadata-only mode
        // accepts it (same posture as the other network tools); untrusted it
        // is non-global and refused.
        let v6_link: IpAddr = "fe80::1".parse().expect("v6");
        validate_resolved_oauth_ips("host", &[v6_link], true, &[]).expect("trusted v6 link-local");
        validate_resolved_oauth_ips("host", &[v6_link], false, &[])
            .expect_err("untrusted v6 link-local");
    }

    #[test]
    fn cgnat_and_ula_are_not_public_even_when_trusted_never_reaches_them() {
        // S1-1 hardening: CGNAT 100.64.0.0/10 and ULA fc00::/7 count as
        // non-public; under `false` (server-supplied) they must be refused.
        let cgnat: IpAddr = "100.64.0.1".parse().expect("cgnat");
        let cgnat_hi: IpAddr = "100.127.255.254".parse().expect("cgnat hi");
        let just_out: IpAddr = "100.128.0.1".parse().expect("not cgnat");
        let ula: IpAddr = "fd12::1".parse().expect("ula");
        validate_resolved_oauth_ips("host", &[cgnat], false, &[]).expect_err("CGNAT blocked");
        validate_resolved_oauth_ips("host", &[cgnat_hi], false, &[]).expect_err("CGNAT hi blocked");
        validate_resolved_oauth_ips("host", &[just_out], false, &[]).expect("100.128/10 is public");
        validate_resolved_oauth_ips("host", &[ula], false, &[]).expect_err("ULA blocked");
    }

    #[test]
    fn nat64_embedded_addresses_follow_the_embedded_ipv4_under_declared_prefixes() {
        // tidux-review blocker: a NAT64 answer embedding a private IPv4
        // behind an OPERATOR-DECLARED prefix must be classified by what it
        // actually reaches - the shared guard decodes declared translators
        // (and the well-known 64:ff9b::/96), closing the hole a private-copy
        // range filter had.
        use crate::helpers::domain_guard::parse_nat64_prefixes;
        let prefixes =
            parse_nat64_prefixes(&["2620:10a::/96".to_string()], "security.nat64_prefixes")
                .expect("test prefix");
        let embedded_private: IpAddr = "2620:10a::a00:5".parse().expect("v6"); // 10.0.0.5
        let embedded_metadata: IpAddr = "2620:10a::a9fe:a9fe".parse().expect("v6"); // 169.254.169.254
        let embedded_public: IpAddr = "2620:10a::5db8:d822".parse().expect("v6"); // 93.184.216.34
        let wellknown_metadata: IpAddr = "64:ff9b::a9fe:a9fe".parse().expect("v6");
        let wellknown_private: IpAddr = "64:ff9b::a00:5".parse().expect("v6");

        // Untrusted target: the embedded RFC1918 destination is refused even
        // though the v6 address itself looks globally routable.
        validate_resolved_oauth_ips("host", &[embedded_private], false, &prefixes)
            .expect_err("NAT64-embedded private refused");
        // The report's exact shape: well-known prefix, no declaration needed.
        validate_resolved_oauth_ips("host", &[wellknown_private], false, &[])
            .expect_err("64:ff9b-embedded private refused");
        // Under trust the private destination is exactly what trust means.
        validate_resolved_oauth_ips("host", &[embedded_private], true, &prefixes)
            .expect("NAT64-embedded private ok for trusted host");
        // Metadata is metadata in every mode and behind every prefix.
        validate_resolved_oauth_ips("host", &[embedded_metadata], true, &prefixes)
            .expect_err("NAT64-embedded metadata refused even trusted");
        validate_resolved_oauth_ips("host", &[embedded_metadata], false, &prefixes)
            .expect_err("NAT64-embedded metadata refused untrusted");
        validate_resolved_oauth_ips("host", &[wellknown_metadata], false, &[])
            .expect_err("well-known NAT64 metadata refused without any declaration");
        // Control: a declared translator to a genuinely public v4 still works.
        validate_resolved_oauth_ips("host", &[embedded_public], false, &prefixes)
            .expect("NAT64-embedded public accepted under a declared prefix");
    }

    #[test]
    fn mapped_link_local_metadata_address_is_blocked() {
        // S1-4: `::ffff:169.254.169.254` is the metadata endpoint wearing an
        // IPv6 mask; `is_loopback()`-style v6 checks would miss it.
        let mapped: IpAddr = "::ffff:169.254.169.254".parse().expect("mapped");
        let err = validate_resolved_oauth_ips("host", &[mapped], false, &[])
            .expect_err("v4-mapped metadata must not slip through as v6");
        assert!(err.to_string().contains("metadata"), "got: {err}");
    }

    #[test]
    fn broadcast_and_multicast_dns_answers_are_blocked_for_untrusted_targets() {
        use std::net::Ipv4Addr;
        let broadcast: IpAddr = Ipv4Addr::BROADCAST.into();
        let mcast4: IpAddr = Ipv4Addr::new(224, 0, 0, 251).into();
        let mcast6: IpAddr = "ff02::1".parse().expect("v6");
        // Untrusted: non-global space per the shared IANA-registry posture.
        validate_resolved_oauth_ips("host", &[broadcast], false, &[]).expect_err("broadcast");
        validate_resolved_oauth_ips("host", &[mcast4], false, &[]).expect_err("v4 multicast");
        validate_resolved_oauth_ips("host", &[mcast6], false, &[]).expect_err("v6 multicast");
        // Trusted: the shared guard's metadata-only mode accepts them (a
        // vouched host answering multicast is odd, not an SSRF shape - same
        // posture as every other network tool's private-network mode).
        validate_resolved_oauth_ips("host", &[broadcast, mcast4, mcast6], true, &[])
            .expect("trusted host may answer non-global oddities");
    }

    #[tokio::test]
    async fn hostname_targets_resolve_validate_and_produce_pin_addrs() {
        // Exercises the non-literal DNS path with the one hostname guaranteed
        // by every POSIX host file: localhost must resolve to loopback, pass
        // the posture checks, and yield pin addresses. Trusted = the same
        // URL (the operator-configured case).
        let self_url = url("http://localhost:9/mcp");
        let addrs = resolve_and_pin_oauth_target(&self_url, &self_url, &[])
            .await
            .expect("localhost resolves locally");
        assert!(!addrs.is_empty());
        assert!(
            addrs.iter().all(|addr| addr.ip().is_loopback()),
            "localhost answered non-loopback: {addrs:?}"
        );
    }

    #[tokio::test]
    async fn ip_literal_targets_need_no_dns_pin() {
        let u = url("http://127.0.0.1:9999/mcp");
        let addrs = resolve_and_pin_oauth_target(&u, &u, &[])
            .await
            .expect("loopback literal ok");
        assert!(addrs.is_empty());
        let meta = url("http://169.254.169.254/mcp");
        let trusted = url("http://127.0.0.1:9/mcp");
        resolve_and_pin_oauth_target(&meta, &trusted, &[])
            .await
            .expect_err("metadata literal blocked");
        // S1-2: the bracketed IPv6 literal decodes to an IpAddr (never the
        // doomed "[::1]" string parse) and is filtered like any other answer.
        // Against a PUBLIC trusted host it is refused as non-public; against
        // a loopback trusted origin it belongs to the loopback identity class
        // (MINOR-1) and passes.
        let public_mcp_trust = url("https://93.184.216.34/mcp");
        let foreign_v6 = url("https://[::1]:8443/token");
        resolve_and_pin_oauth_target(&foreign_v6, &public_mcp_trust, &[])
            .await
            .expect_err("public origin may not adopt a [::1] AS");
        resolve_and_pin_oauth_target(&foreign_v6, &trusted, &[])
            .await
            .expect("[::1] shares trust with the 127.0.0.1 dev host");
        let own_v6 = url("http://[::1]:8080/mcp");
        resolve_and_pin_oauth_target(&own_v6, &own_v6, &[])
            .await
            .expect("trusted ::1 literal ok");
    }

    #[tokio::test]
    async fn server_supplied_targets_may_not_point_at_private_space() {
        // S1-1: private/loopback targets are accepted only for the host the
        // operator configured. Same HOST (any port/scheme/path) is trusted;
        // every other origin must be a public address.
        let as_on_same_host = url("http://10.0.0.5:8443/token");
        let mcp = url("http://10.0.0.5/mcp");
        resolve_and_pin_oauth_target(&as_on_same_host, &mcp, &[])
            .await
            .expect("same-host private IdP stays working");
        let foreign_private = url("http://10.0.0.5/mcp");
        let public_mcp = url("https://mcp.example.com/mcp");
        resolve_and_pin_oauth_target(&foreign_private, &public_mcp, &[])
            .await
            .expect_err("server-named private target refused");
        let foreign_loopback = url("http://127.0.0.1:1/x");
        resolve_and_pin_oauth_target(&foreign_loopback, &public_mcp, &[])
            .await
            .expect_err("server-named loopback refused");
        let public = url("https://93.184.216.34/token");
        resolve_and_pin_oauth_target(&public, &public_mcp, &[])
            .await
            .expect("public target always allowed");
    }

    #[tokio::test]
    async fn loopback_spellings_are_one_identity_for_trust() {
        // MINOR-1 (reviewer A): `localhost` + `127.0.0.1` + `::1` are one
        // machine; mixing spellings between the configured MCP URL and a
        // loopback AS must not split trust. Private (RFC1918) targets do
        // NOT get this leniency — exact host match only.
        let mcp_at_localhost = url("http://localhost:9/mcp");
        let as_at_127 = url("http://127.0.0.1:9999/token");
        resolve_and_pin_oauth_target(&as_at_127, &mcp_at_localhost, &[])
            .await
            .expect("loopback spellings share trust");
        let private_as = url("http://10.0.0.5/token");
        resolve_and_pin_oauth_target(&private_as, &mcp_at_localhost, &[])
            .await
            .expect_err("RFC1918 needs the exact host, not the loopback class");
    }

    #[tokio::test]
    async fn dns_shape_foreign_target_to_loopback_is_refused() {
        // MINOR-2 (reviewer A): the gate is not literal-only — on the DNS
        // branch the ANSWERS are gated too. `localhost` resolves to
        // loopback, which a foreign (public IP) trusted host may not adopt.
        let err = resolve_and_pin_oauth_target(
            &url("http://localhost:1/x"),
            &url("https://93.184.216.34/mcp"),
            &[],
        )
        .await
        .expect_err("domain branch gates resolved answers");
        assert!(err.to_string().contains("non-global"), "got: {err}");
    }

    #[tokio::test]
    async fn factory_without_config_url_fails_loudly() {
        // MINOR-2 (reviewer A): production always has `url` for remote
        // transports (config validation), so a missing one is an invariant
        // violation — it must fail closed, never silently trust any host.
        let factory = OAuthClientFactory::new(&McpServerConfig {
            name: "no-url".to_string(),
            ..Default::default()
        });
        let err = factory
            .client_for(&url("https://93.184.216.34/token"))
            .await
            .expect_err("no trust anchor, no client");
        assert!(
            err.to_string().contains("no URL to anchor OAuth trust"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn oauth_client_refuses_redirects_and_plaintext_remote() {
        // IP-literal targets keep this unit test hermetic (no live DNS).
        let config = McpServerConfig {
            name: "probe".to_string(),
            ..Default::default()
        };
        let target = url("https://127.0.0.1:9443/mcp");
        let client = build_oauth_http_client(&config, &target, &target, &[])
            .await
            .expect("client builds");
        assert!(
            client
                .get("https://127.0.0.1:9443/.well-known/oauth-protected-resource")
                .build()
                .is_ok()
        );
        let config_https_only = McpServerConfig {
            name: "probe".to_string(),
            tls_ca_cert_path: Some("/nonexistent-ca.pem".to_string()),
            ..Default::default()
        };
        let plain = url("http://127.0.0.1:1/x");
        let err = build_oauth_http_client(&config_https_only, &plain, &plain, &[])
            .await
            .expect_err("custom CA forbids plaintext OAuth targets");
        assert!(
            !err.to_string().contains("nonexistent-ca"),
            "https gate must run before CA file IO: {err}"
        );
    }
}
