use crate::helpers::domain_guard;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult, with_ephemeral_workspace_warning};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::FileDownloadConfig;

/// Result type produced by the DNS resolver seam.
type ResolveResult = Result<Vec<std::net::SocketAddr>, String>;
/// Async DNS resolver seam, injectable so tests can count or forbid resolver
/// calls. Defaults to [`resolve_endpoint_ips`]. Takes an owned host so the
/// returned future can outlive the call (no borrowed-lifetime coupling).
type EndpointResolver =
    Arc<dyn Fn(String, u16) -> Pin<Box<dyn Future<Output = ResolveResult> + Send>> + Send + Sync>;

fn default_endpoint_resolver() -> EndpointResolver {
    Arc::new(
        |host: String, port: u16| -> Pin<Box<dyn Future<Output = ResolveResult> + Send>> {
            Box::pin(async move { resolve_endpoint_ips(&host, port).await })
        },
    )
}

const RESPONSE_BODY_LIMIT_BYTES: usize = 4 * 1024;
const TOOL_DESCRIPTION_KEY: &str = "tool-file-download";
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

/// Actionable error surfaced when the configured runtime proxy would defeat
/// the SSRF address pin. The secure client must never route through a proxy
/// (proxy-side DNS would bypass `resolve_to_addrs`), so a conflicting proxy
/// configuration is rejected loudly at dispatch rather than silently ignored,
/// mirroring `http_request` and `web_fetch`.
const FILE_DOWNLOAD_PROXY_PINNING_ERROR: &str = "file_download requires direct transport so validated DNS \
     answers remain pinned; set proxy.scope = \"services\" and omit tool.file_download and tool.* from \
     proxy.services, or disable the proxy; proxy.scope = \"environment\" is incompatible with pinned HTTP requests";

/// Mirror of `http_request::proxy_conflicts_with_dns_pinning` / `web_fetch`:
/// a runtime proxy conflicts with direct transport when `proxy.scope =
/// "environment"`, or when a proxy URL is set and applies to this tool.
fn proxy_conflicts_with_dns_pinning(config: &zeroclaw_config::schema::ProxyConfig) -> bool {
    (config.enabled && config.scope == zeroclaw_config::schema::ProxyScope::Environment)
        || (config.has_any_proxy_url() && config.should_apply_to_service("tool.file_download"))
}

/// The SSRF policy view one dispatch reads atomically. Both fields are
/// captured together, so a config generation can never present a dispatch with
/// an allowlist from one version and NAT64 prefixes from another (splitting
/// them into two independent resolvers would let a config swap land between two
/// reads, admitting a hybrid view no valid configuration permits — this shape
/// is what the live-config slice needs to read under a single `live.read()`).
#[derive(Clone, Default)]
pub struct FileDownloadSsrfPolicy {
    /// Operator-declared private-host allowlist (from `[file_download]`).
    pub allowed_private_hosts: Vec<String>,
    /// Operator-declared network-specific RFC 6052 NAT64 prefixes (from the
    /// canonical `[security] nat64_prefixes` fact).
    pub nat64_prefixes: Vec<String>,
}

pub struct FileDownloadTool {
    security: Arc<SecurityPolicy>,
    config: FileDownloadConfig,
    /// Resolves the atomic SSRF policy snapshot at use time. Defaults to the
    /// construction-time snapshot; the live-config propagation slice threads a
    /// canonical-config resolver here instead. The single-policy shape keeps
    /// `allowed_private_hosts` and `nat64_prefixes` from the same config
    /// generation on every dispatch (AGENTS.md single-source-of-truth).
    policy_resolver: Arc<dyn Fn() -> FileDownloadSsrfPolicy + Send + Sync>,
    /// DNS resolver seam, injectable so tests can observe that a rejected
    /// dispatch performs zero resolver I/O. Defaults to [`resolve_endpoint_ips`].
    endpoint_resolver: EndpointResolver,
    /// Whether the downloaded file persists on the host filesystem. `false` on
    /// an ephemeral runtime (Docker tmpfs / no volume mount), where the file is
    /// written inside the container but invisible on the host and discarded at
    /// session end. When `false`, a successful download carries a loud
    /// ephemeral-workspace warning. Mirrors
    /// [`super::file_write::FileWriteTool`].
    persistent_writes: bool,
}

impl FileDownloadTool {
    pub fn new(security: Arc<SecurityPolicy>, config: FileDownloadConfig) -> Self {
        Self::new_with_persistence(security, config, true, Vec::new())
    }

    /// Construct with an explicit persistence flag derived from the active
    /// runtime adapter's `has_filesystem_access()`. Mirrors
    /// [`super::file_write::FileWriteTool::new_with_persistence`].
    ///
    /// The SSRF policy (`allowed_private_hosts` + `nat64_prefixes`) is resolved
    /// from the construction-time `config` / `nat64_prefixes` snapshot on each
    /// dispatch by the `policy_resolver`. The NAT64 prefixes are the canonical
    /// host-level `security.nat64_prefixes` fact, passed explicitly because the
    /// canonical owner is not the `file_download` config section.
    pub fn new_with_persistence(
        security: Arc<SecurityPolicy>,
        config: FileDownloadConfig,
        persistent_writes: bool,
        nat64_prefixes: Vec<String>,
    ) -> Self {
        let snapshot = FileDownloadSsrfPolicy {
            allowed_private_hosts: config.allowed_private_hosts.clone(),
            nat64_prefixes,
        };
        Self {
            security,
            config,
            policy_resolver: Arc::new(move || snapshot.clone()),
            endpoint_resolver: default_endpoint_resolver(),
            persistent_writes,
        }
    }

    /// Construct with an explicit DNS resolver seam, mirroring
    /// [`Self::new_with_persistence`]. Tests inject a counting or
    /// forbidding resolver to prove a rejected dispatch performs zero resolver
    /// I/O; production callers use the default [`resolve_endpoint_ips`].
    #[cfg(test)]
    fn new_with_endpoint_resolver<F>(
        security: Arc<SecurityPolicy>,
        config: FileDownloadConfig,
        persistent_writes: bool,
        policy_resolver: F,
        endpoint_resolver: EndpointResolver,
    ) -> Self
    where
        F: Fn() -> FileDownloadSsrfPolicy + Send + Sync + 'static,
    {
        Self {
            security,
            config,
            policy_resolver: Arc::new(policy_resolver),
            endpoint_resolver,
            persistent_writes,
        }
    }

    /// Gate the configured download URL against the SSRF policy. The endpoint
    /// URL is operator-configured, but a typo or copy-paste (e.g.
    /// `http://127.0.0.1`, `http://169.254.169.254/...`) must surface as a
    /// clear rejection before any network call. Returns the validated
    /// transport host + its resolved `SocketAddr` set so the caller can
    /// bind them via `resolve_to_addrs`, closing the TOCTOU window
    /// between validation and connect.
    ///
    /// Thin dispatch over three module-level helpers:
    ///
    /// - [`parse_endpoint_url`] — returns transport_host (for reqwest binding),
    ///   policy_host (for SSRF policy comparison), and port.
    /// - [`resolve_endpoint_ips`] — DNS resolution using transport_host
    ///   (short-circuits on IP literals).
    /// - [`ssrf_check_endpoint`] — applies the private-host / metadata policy
    ///   using policy_host and emits the operator-audit WARN/INFO log signals.
    ///
    /// Mirrors the `ValidatedHttpRequestTarget` pattern from
    /// `http_request.rs:172-191` / `:363`.
    async fn validate_endpoint_host(
        &self,
        raw_url: &str,
    ) -> Result<(String, Vec<std::net::SocketAddr>), String> {
        let (transport_host, policy_host, port) = parse_endpoint_url(raw_url)?;
        // Resolve the atomic SSRF policy snapshot (allowlist + NAT64 prefixes
        // together). Normalize the operator-declared policy view BEFORE any DNS
        // I/O: a malformed `allowed_private_hosts` entry fails the dispatch
        // closed to an empty allowlist, and a malformed `nat64_prefixes` set
        // fails the whole declaration set closed, before the endpoint-resolution
        // side effect runs, so the configured hostname is not leaked into the
        // resolver while the local policy is invalid.
        let policy = (self.policy_resolver)();
        let allowed = normalize_allowed_private_hosts(&policy.allowed_private_hosts);
        let declared_prefixes = normalize_nat64_prefixes(&policy.nat64_prefixes)?;
        // Resolve the exact transport hostname, which may carry a terminal DNS
        // dot. A trailing dot marks an absolute name, so resolving it forces an
        // explicitly absolute lookup: resolver search-list behavior cannot
        // substitute a different relative name, and the validated address set
        // is guaranteed to belong to the exact hostname reqwest connects to.
        // `policy_host` is retained only for allowlist comparison + diagnostics.
        let resolved_addrs = (self.endpoint_resolver)(transport_host.to_string(), port).await?;
        // The shared `net_guard` validator always applies the well-known
        // `64:ff9b::/96` classification; `declared_prefixes` adds the
        // operator-declared network-specific RFC 6052 prefixes.
        ssrf_check_endpoint(&policy_host, &resolved_addrs, &allowed, &declared_prefixes)?;
        // Return transport_host for resolve_to_addrs binding — this preserves
        // the exact hostname spelling (including trailing dot) that reqwest
        // will use as the key for its resolver override.
        Ok((transport_host, resolved_addrs))
    }

    /// Stream a response body into `temp_path`, treating `max_bytes` as a hard
    /// ceiling so an unbounded or oversized body never fully buffers in memory.
    /// Returns the number of bytes written, or an error message. The caller is
    /// responsible for removing `temp_path` on any error.
    async fn stream_to_temp(
        response: reqwest::Response,
        temp_path: &Path,
        max_bytes: u64,
    ) -> Result<u64, String> {
        let mut file = tokio::fs::File::create(temp_path).await.map_err(|e| {
            tool_msg_with_args(
                "tool-file-download-error-temp-create",
                &[("err", &e.to_string())],
            )
        })?;

        let mut stream = response.bytes_stream();
        let mut written: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                tool_msg_with_args(
                    "tool-file-download-error-read-body",
                    &[("err", &e.to_string())],
                )
            })?;
            written = written.saturating_add(chunk.len() as u64);
            if written > max_bytes {
                let limit = max_bytes.to_string();
                return Err(tool_msg_with_args(
                    "tool-file-download-error-too-large-stream",
                    &[("limit", &limit)],
                ));
            }
            file.write_all(&chunk).await.map_err(|e| {
                tool_msg_with_args(
                    "tool-file-download-error-write-body",
                    &[("err", &e.to_string())],
                )
            })?;
        }

        file.flush().await.map_err(|e| {
            tool_msg_with_args("tool-file-download-error-flush", &[("err", &e.to_string())])
        })?;
        Ok(written)
    }
}

/// Look up a required tool string from the Fluent catalogue. Thin wrapper
/// around [`crate::i18n::get_required_tool_string`] kept as a module-level
/// free function so the URL-resolution seam (`parse_endpoint_url` /
/// `resolve_endpoint_ips` / `ssrf_check_endpoint`) and the `Tool` impl both
/// call into the same lookup without reaching into the impl block.
fn tool_msg(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

/// Variant of [`tool_msg`] that interpolates named arguments into the
/// localized string. Mirrors [`crate::i18n::get_required_tool_string_with_args`].
fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_tool_string_with_args(key, args)
}

/// Extract the transport host from an `http://` or `https://` URL.
/// The endpoint is parsed through `reqwest::Url` so alternate and percent-encoded
/// IPv4 representations are canonicalised the same way reqwest's transport will
/// (e.g. `http://2130706433/` → `127.0.0.1`). Userinfo, non-http(s) schemes,
/// IPv6 hosts, and empty hosts are all rejected.
///
/// **Crucially**: this preserves trailing dots (FQDN root labels) because
/// reqwest's `resolve_to_addrs` uses the exact `host_str()` as the key for
/// its resolver override. The policy layer (`ssrf_check_endpoint`) will
/// canonicalize by stripping the trailing dot for allowlist comparison.
fn extract_download_url_host(url: &str) -> anyhow::Result<String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| anyhow::Error::msg(format!("Invalid download URL: {e}")))?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => anyhow::bail!("Only http:// and https:// URLs are allowed"),
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("URL userinfo is not allowed");
    }

    let host_str = parsed
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow::Error::msg("URL must include a valid host"))?;

    // IPv6 hosts appear as e.g. "::1" in host_str(); reject them.
    if host_str.contains(':') {
        anyhow::bail!("IPv6 hosts are not supported in file_download endpoint URLs");
    }

    // Preserve trailing dot (FQDN root label) for exact transport binding.
    // reqwest's resolve_to_addrs uses the exact host_str() as the key,
    // so "files.corp.lan." must be preserved to match the request hostname.
    // The policy layer (ssrf_check_endpoint) canonicalizes by stripping the dot.
    Ok(host_str.to_ascii_lowercase())
}

/// Parse the configured `[file_download].url` into three components:
///
/// - `transport_host`: exact host string for reqwest's `resolve_to_addrs` key
///   (preserves trailing dot, e.g. `"files.corp.lan."`)
/// - `policy_host`: canonical host for SSRF policy comparison
///   (strips trailing dot, e.g. `"files.corp.lan"`)
/// - `port`: explicit port number
///
/// Pure: no network I/O. Rejects empty URLs, non-http(s) schemes, URLs without
/// an explicit port, userinfo, and IPv6 hosts. The host is taken through
/// [`extract_download_url_host`] so alternate IPv4 / percent-encoded loopback
/// forms canonicalise the same way reqwest's transport will classify them.
fn parse_endpoint_url(raw_url: &str) -> Result<(String, String, u16), String> {
    let url = raw_url.trim();
    if url.is_empty() {
        return Err(tool_msg("tool-file-download-error-disabled"));
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| {
        tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &e.to_string())],
        )
    })?;
    // URL schemes are case-insensitive (RFC 3986 §3.1). `reqwest::Url`
    // lowercases the scheme during parse, so compare against the parsed
    // scheme rather than a case-sensitive string prefix — an operator-written
    // `HTTPS://...` endpoint is valid and must not regress to a scheme error.
    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            // Report only the scheme, not the full configured URL. The raw
            // endpoint may embed userinfo or a query with credentials, and the
            // error is model-visible tool output; echoing it here would leak
            // operator-configured secrets before the userinfo guard in
            // `extract_download_url_host` runs.
            return Err(tool_msg_with_args(
                "tool-file-download-error-bad-scheme",
                &[("scheme", parsed.scheme())],
            ));
        }
    }
    let port = parsed.port_or_known_default().ok_or_else(|| {
        tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", "URL must include a valid port")],
        )
    })?;

    // Extract transport_host (preserves trailing dot for reqwest binding)
    // Use extract_download_url_host which handles userinfo rejection
    let transport_host = extract_download_url_host(url).map_err(|e| {
        tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &e.to_string())],
        )
    })?;

    // Canonicalize for policy: strip trailing dot
    let policy_host = transport_host.trim_end_matches('.').to_string();

    Ok((transport_host, policy_host, port))
}

/// DNS resolution for `(host, port)`. IP literals short-circuit
/// to a one-element vector (no I/O) — `ssrf_check_endpoint` then
/// classifies the literal directly. Hostnames go through
/// `tokio::net::lookup_host` and the address set is collected.
///
/// The caller passes the exact `transport_host`, which may carry a terminal
/// DNS dot. A trailing dot marks an absolute name, so resolving it forces an
/// explicitly absolute lookup and prevents resolver search-list behavior from
/// substituting a different relative name than the one being validated.
async fn resolve_endpoint_ips(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>, String> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(vec![std::net::SocketAddr::new(ip, port)]);
    }
    let addrs: Vec<std::net::SocketAddr> = match tokio::net::lookup_host((host, port)).await {
        Ok(s) => s.collect(),
        Err(e) => {
            return Err(tool_msg_with_args(
                "tool-file-download-error-invalid-url",
                &[("err", &format!("Failed to resolve host '{host}': {e}"))],
            ));
        }
    };
    if addrs.is_empty() {
        return Err(tool_msg_with_args(
            "tool-file-download-error-invalid-url",
            &[("err", &format!("Failed to resolve host '{host}'"))],
        ));
    }
    Ok(addrs)
}

/// The IPv4 embedded in `ip` under any operator-declared network-specific
/// NAT64 prefix, or an empty Vec when no declared prefix translates it.
///
/// The declared-prefix set is the only source of truth for canonicalization: a
/// network-specific prefix (RFC 8215 local-use `64:ff9b:1::/48`, or any
/// operator-assigned prefix) cannot be detected from an address alone, so the
/// shared `net_guard` classifier canonicalizes each resolved address against
/// every declared prefix as part of its `validate_resolved_ips_*` entry points.
fn declared_nat64_embedded_v4s(
    ip: std::net::IpAddr,
    prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
) -> Vec<std::net::Ipv4Addr> {
    let std::net::IpAddr::V6(v6) = ip else {
        return Vec::new();
    };
    // Decode under EVERY containing prefix, not just the first match, so an
    // overlapping declaration cannot hide a more-specific translation (a broad
    // public /32 vouch must not mask a nested /96 that carries the address to a
    // metadata endpoint). The shared `net_guard` validator is order-independent
    // for the same reason; callers that classify diagnostics must match it.
    prefixes
        .iter()
        .filter_map(|p| p.embedded_ipv4(v6))
        .collect()
}

/// The IPv4 metadata/credential endpoint embedded in `ip` under a declared
/// network-specific NAT64 prefix, if any. Mirrors the raw-address metadata
/// check so a DNS64 answer under a declared prefix can never reach a metadata
/// endpoint, even through the allowlist path (the shared validator also does
/// this, but the file_download error surface needs to distinguish the metadata
/// case to avoid suggesting an allowlist entry that cannot help).
fn declared_nat64_metadata(
    ip: std::net::IpAddr,
    prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
) -> Option<std::net::Ipv4Addr> {
    // Consider every containing prefix so an overlapping declaration cannot
    // hide a translation to a metadata endpoint behind a broader public one.
    declared_nat64_embedded_v4s(ip, prefixes)
        .into_iter()
        .find(|v4| domain_guard::is_cloud_metadata_ip(std::net::IpAddr::V4(*v4)))
}

/// Apply the shared SSRF policy. `policy_host` is canonical (no trailing dot)
/// for allowlist comparison. `resolved_addrs` are the IPs for that policy host.
/// The function preserves the operator-visibility audit signal: a WARN log on
/// rejection of a private literal host and an INFO log when the allowlist
/// path admits a private host.
///
/// Mirrors `http_request::validate_resolved_ips_for_ssrf`
/// (`http_request.rs:707-717`):
///
/// - A hostname covered by `allowed_hosts` lifts the *non-global* check
///   but never lifts the metadata-IP check.
/// - If the hostname is not allowlisted and resolves to a public IP, it
///   passes through; if it resolves to a private / loopback / link-local
///   IP, it is rejected with `tool-file-download-error-private-host`.
///
/// `nat64_prefixes` is threaded through to the shared `net_guard` validator;
/// this base slice always passes the empty set (no operator-declared
/// prefixes), while the shared validator independently applies the well-known
/// `64:ff9b::/96` classification to every resolved address.
fn ssrf_check_endpoint(
    policy_host: &str,
    resolved_addrs: &[std::net::SocketAddr],
    allowed_hosts: &[String],
    nat64_prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
) -> Result<(), String> {
    let ips: Vec<std::net::IpAddr> = resolved_addrs.iter().map(|sa| sa.ip()).collect();
    let private_allowed = domain_guard::host_matches_allowlist(policy_host, allowed_hosts);

    // Cloud metadata / credential-delivery addresses are rejected regardless
    // of `allowed_private_hosts` (the schema contract at
    // `file_download.allowed_private_hosts` documents that the carve-out never
    // lifts the metadata exclusion). Surface a DISTINCT error with no allowlist
    // suggestion — the generic private-host message would tell the operator to
    // add a host that cannot be enabled.
    let metadata_hit = ips.iter().find_map(|ip| {
        if domain_guard::is_cloud_metadata_ip(*ip) {
            Some((*ip, *ip))
        } else {
            // A DNS64 answer under a declared NAT64 prefix must be canonicalized
            // to its embedded IPv4 first, so it can never reach a
            // metadata/credential endpoint either. Consider every containing
            // prefix (order-independent).
            declared_nat64_metadata(*ip, nat64_prefixes).map(|v4| (*ip, std::net::IpAddr::V4(v4)))
        }
    });
    if let Some((raw_ip, metadata_ip)) = metadata_hit {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "tool": "file_download",
                    "host": policy_host,
                    "ip": raw_ip.to_string(),
                })),
            "file_download: rejected cloud metadata/credential endpoint host"
        );
        return Err(tool_msg_with_args(
            "tool-file-download-error-metadata-endpoint",
            &[("host", policy_host), ("ip", &metadata_ip.to_string())],
        ));
    }

    // Delegate the actual decision to the shared validator, which runs the
    // same metadata / non-global classification on every resolved address (the
    // allowlist path lifts only the non-global check, never the metadata
    // exclusion).
    let validation_err = if private_allowed {
        domain_guard::validate_resolved_ips_exclude_metadata(policy_host, &ips, nat64_prefixes)
    } else {
        domain_guard::validate_resolved_ips_are_public(policy_host, &ips, nat64_prefixes)
    }
    .err();

    if let Some(err) = validation_err {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "tool": "file_download",
                    "host": policy_host,
                })),
            "file_download: rejected private/local endpoint host"
        );
        return Err(tool_msg_with_args(
            "tool-file-download-error-private-host",
            &[
                ("host", policy_host),
                ("config_key", "file_download.allowed_private_hosts"),
                ("err", &err.to_string()),
            ],
        ));
    }

    // The INFO audit event describes what actually happened, not merely that
    // the hostname matched the allowlist. A wildcard allowlist that admits a
    // public endpoint must not log "allowing private host"; only when the
    // resolved addresses actually use the private carve-out is the event
    // accurate.
    let resolved_uses_private = ips.iter().any(|ip| match ip {
        std::net::IpAddr::V4(v4) => zeroclaw_infra::net_guard::is_non_global_v4(*v4),
        std::net::IpAddr::V6(v6) => {
            zeroclaw_infra::net_guard::is_non_global_v6(*v6)
                || declared_nat64_embedded_v4s(std::net::IpAddr::V6(*v6), nat64_prefixes)
                    .iter()
                    .any(|v4| zeroclaw_infra::net_guard::is_non_global_v4(*v4))
        }
    });

    if private_allowed && resolved_uses_private {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "tool": "file_download",
                    "host": policy_host,
                })),
            "file_download: allowing private host via allowed_private_hosts"
        );
    }

    Ok(())
}

/// Per-dispatch normalization of the canonical `config.allowed_private_hosts`
/// Vec. Strips trailing dots from hostnames so `"files.corp.lan."` matches
/// `"files.corp.lan"` in allowlist comparison. Returns filtered list on `Ok`.
/// On `Err`, emits a once-per-process WARN (so spam doesn't flood the logs
/// on every dispatch) and falls back to an empty allowlist — the SSRF gate
/// still functions and any future config-layer regression surfaces in logs
/// instead of silently disabling the gate.
fn normalize_allowed_private_hosts(allowed: &[String]) -> Vec<String> {
    match domain_guard::normalize_allowed_domains(
        allowed.to_vec(),
        "file_download.allowed_private_hosts",
    ) {
        Ok(v) => v,
        Err(e) => {
            NORMALIZE_WARNING_EMITTED.get_or_init(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "file_download: failed to normalize allowed_private_hosts; using empty list"
                );
            });
            Vec::new()
        }
    }
}

/// Normalize the operator-declared network-specific NAT64 prefix set, delegating
/// to the shared `net_guard` parser (sort/dedupe) and failing the whole list
/// closed on any malformed entry so a typo cannot silently narrow the SSRF
/// boundary to "no prefixes".
fn normalize_nat64_prefixes(
    raw: &[String],
) -> Result<Vec<zeroclaw_infra::net_guard::Nat64Prefix>, String> {
    match zeroclaw_infra::net_guard::parse_nat64_prefixes(raw, "security.nat64_prefixes") {
        Ok(parsed) => Ok(parsed),
        Err(err) => Err(tool_msg_with_args(
            "tool-file-download-error-invalid-nat64-prefix",
            &[
                ("prefix", &err.to_string()),
                ("config_key", "security.nat64_prefixes"),
            ],
        )),
    }
}

/// Set by `normalize_allowed_private_hosts` the first time the
/// configured allowlist fails to normalize, so the WARN fires at most
/// once per process. Drops the per-dispatch noise that would otherwise
/// flood logs for a permanently-misconfigured entry.
static NORMALIZE_WARNING_EMITTED: OnceLock<()> = OnceLock::new();

/// Build the reqwest client used to fetch the configured endpoint. The
/// validated `(transport_host, resolved_addrs)` pair from
/// [`FileDownloadTool::validate_endpoint_host`] is bound into the client
/// via `resolve_to_addrs`, so the connection cannot perform a second
/// unbound DNS lookup at connect time. Redirect-following is disabled
/// because the configured `[file_download].url` is operator-approved and
/// a 3xx must surface as a status, not silently rehome the request.
///
/// The client never routes through a proxy. The validated address set is
/// the connection authority: an HTTP proxy, HTTPS `CONNECT`, or proxy-side
/// hostname resolution would let the proxy re-resolve the target
/// independently of `resolve_to_addrs`, defeating the SSRF gate. `no_proxy()`
/// explicitly opts this security-specific client out of both the runtime
/// proxy config and any system/env proxy the surrounding process inherits.
///
/// This helper is intentionally a free function (not an instance method)
/// so the wire-up contract can be unit-tested directly: callers can
/// supply a hand-crafted `(transport_host, resolved_addrs)` and assert that
/// a real reqwest request lands on the validated address set rather than on
/// a second DNS lookup. See `resolve_to_addrs_binds_resolved_addrs_not_real_dns`.
async fn build_secure_download_client(
    transport_host: &str,
    resolved_addrs: &[std::net::SocketAddr],
    timeout_secs: u64,
) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(transport_host, resolved_addrs)
        .no_proxy();
    builder.build().map_err(|e| {
        tool_msg_with_args(
            "tool-file-download-error-client-build",
            &[("err", &e.to_string())],
        )
    })
}

#[async_trait]
impl Tool for FileDownloadTool {
    fn name(&self) -> &str {
        "file_download"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| crate::i18n::get_required_tool_string(TOOL_DESCRIPTION_KEY))
            .as_str()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "document_id": {
                    "type": "string",
                    "description": tool_msg("tool-file-download-param-document-id")
                },
                "dest_path": {
                    "type": "string",
                    "description": tool_msg("tool-file-download-param-dest-path")
                }
            },
            "required": ["document_id", "dest_path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(url) = self
            .config
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-file-download-error-disabled")),
            });
        };

        // SSRF gate + DNS resolution are intentionally deferred until AFTER
        // the local authorization / input / destination checks below: a
        // read-only, rate-limited, missing-arg, or traversal-rejected call
        // must NOT reach the resolver.
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-file-download-error-read-only")),
            });
        }

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-file-download-error-rate-limited-hour")),
            });
        }

        let document_id = args
            .get("document_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "document_id"})),
                    "file_download: missing document_id parameter"
                );
                anyhow::Error::msg(tool_msg("tool-file-download-error-missing-document-id"))
            })?;

        let dest_path = args
            .get("dest_path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "dest_path"})),
                    "file_download: missing dest_path parameter"
                );
                anyhow::Error::msg(tool_msg("tool-file-download-error-missing-dest-path"))
            })?;

        // The downloaded bytes are attacker-influenceable, so the write target
        // must resolve inside the workspace allowlist before any network call.
        let full = self.security.resolve_tool_path(dest_path);

        let file_name = match full.file_name().and_then(|s| s.to_str()) {
            Some(name) if name != "." && name != ".." => name.to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_msg_with_args(
                        "tool-file-download-error-invalid-file-name",
                        &[("dest_path", dest_path)],
                    )),
                });
            }
        };

        let Some(parent) = full.parent() else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg_with_args(
                    "tool-file-download-error-no-parent",
                    &[("dest_path", dest_path)],
                )),
            });
        };

        // Canonicalize the parent (which must already exist) so a symlinked
        // parent cannot redirect the write outside the workspace. `full` itself
        // does not exist yet, so it is never canonicalized.
        let canonical_parent = match tokio::fs::canonicalize(parent).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_msg_with_args(
                        "tool-file-download-error-resolve-dir",
                        &[("dest_path", dest_path), ("err", &e.to_string())],
                    )),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&canonical_parent) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(
                    self.security
                        .resolved_path_violation_message(&canonical_parent),
                ),
            });
        }

        let dest = canonical_parent.join(&file_name);
        if !self.security.is_resolved_path_allowed(&dest) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(self.security.resolved_path_violation_message(&dest)),
            });
        }

        // Egress-policy checks run BEFORE the SSRF/DNS gate so an incompatible
        // proxy configuration is surfaced before any DNS I/O, matching
        // `http_request` and `web_fetch` (which reject a conflicting runtime
        // proxy and WARN on an ignored environment proxy ahead of the request).
        //
        // 1) A configured runtime proxy scoped to file_download (or any tool)
        // would re-resolve the target independently of the validated address
        // set and defeat the SSRF pin, so reject it with an actionable error
        // instead of silently bypassing the operator's egress/audit policy.
        let proxy_config = zeroclaw_config::schema::runtime_proxy_config();
        if proxy_conflicts_with_dns_pinning(&proxy_config) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"service": "tool.file_download"})),
                "file_download: configured runtime proxy rejected to preserve validated DNS pin"
            );
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(FILE_DOWNLOAD_PROXY_PINNING_ERROR.into()),
            });
        }

        // 2) The direct client also ignores raw process environment proxies
        // (`HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY`) so their DNS cannot
        // re-resolve the target. Unlike a conflicting runtime proxy these are
        // surfaced as a diagnostic, not a hard error, mirroring http_request /
        // web_fetch: the operator's egress is documented rather than silently
        // bypassed.
        if let Some(variable) = zeroclaw_config::schema::environment_proxy_for_url(url) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"proxy_variable": variable})),
                "file_download: environment proxy ignored to preserve validated DNS pin"
            );
        }

        // SSRF gate: the configured URL must point at a non-private host
        // (or an explicitly allowlisted one) AND its resolved IPs must
        // satisfy the same policy. Catches typos / copy-paste mistakes
        // (e.g. `http://127.0.0.1` or `http://169.254.169.254/...`) at
        // dispatch time before any network call, and binds the validated
        // address set into the reqwest client so a second unbound DNS
        // lookup at connect time cannot bypass this gate. Runs AFTER
        // local authorization / arg / destination validation and BEFORE
        // the action-budget debit so a request that fails the SSRF gate
        // never burns budget.
        //
        // `validate_endpoint_host` returns `transport_host` (exact hostname
        // for reqwest binding, preserves trailing dot) and resolved addresses.
        let (transport_host, resolved_addrs) = match self.validate_endpoint_host(url).await {
            Ok(target) => target,
            Err(msg) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(msg),
                });
            }
        };

        // Debit the action budget only once the request is validated, mirroring
        // file_upload — right before the network call.
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg("tool-file-download-error-rate-limited-budget")),
            });
        }

        // Disable redirect-following: the configured `[file_download].url` is
        // the operator-approved endpoint, so a 3xx response from it must surface
        // as a non-success status rather than silently rehome the request.
        // Bind the validated address set into the reqwest client keyed by
        // the transport_host (exact hostname for resolver override), so a
        // second unbound DNS lookup at connect time cannot bypass the SSRF gate.
        let client = match build_secure_download_client(
            &transport_host,
            &resolved_addrs,
            self.config.timeout_secs,
        )
        .await
        {
            Ok(c) => c,
            Err(msg) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(msg),
                });
            }
        };

        let mut request = client.get(url).query(&[("document_id", document_id)]);
        for (k, v) in &self.config.headers {
            request = request.header(k.as_str(), v.as_str());
        }

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_msg_with_args(
                        "tool-file-download-error-request",
                        &[("err", &e.to_string())],
                    )),
                });
            }
        };

        let status = response.status();

        if !status.is_success() {
            let raw_body = response.text().await.unwrap_or_default();
            let truncated = if raw_body.len() > RESPONSE_BODY_LIMIT_BYTES {
                let mut cut = RESPONSE_BODY_LIMIT_BYTES;
                while cut > 0 && !raw_body.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!(
                    "{}... [truncated {} bytes]",
                    &raw_body[..cut],
                    raw_body.len() - cut
                )
            } else {
                raw_body
            };
            return Ok(ToolResult {
                success: false,
                output: truncated.into(),
                error: Some(tool_msg_with_args(
                    "tool-file-download-error-status",
                    &[("status", &status.to_string())],
                )),
            });
        }

        // Fast-reject when the endpoint advertises an oversized body, before
        // opening the destination file at all.
        if let Some(len) = response.content_length()
            && len > self.config.max_file_size_bytes
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_msg_with_args(
                    "tool-file-download-error-too-large-reported",
                    &[
                        ("len", &len.to_string()),
                        ("limit", &self.config.max_file_size_bytes.to_string()),
                    ],
                )),
            });
        }

        // Stream into a temp file in the destination directory so a failed or
        // oversized transfer never leaves a partial artifact at `dest`; on
        // success the rename is atomic within the same directory.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_path = canonical_parent.join(format!(".{file_name}.part-{nanos}"));

        match Self::stream_to_temp(response, &temp_path, self.config.max_file_size_bytes).await {
            Ok(written) => match tokio::fs::rename(&temp_path, &dest).await {
                Ok(()) => {
                    let output = tool_msg_with_args(
                        "tool-file-download-success",
                        &[
                            ("written", &written.to_string()),
                            ("dest_path", dest_path),
                            ("status", &status.to_string()),
                        ],
                    );
                    // The download landed in an ephemeral workspace and will not
                    // reach the host — warn loudly rather than report a bare
                    // success.
                    let output = if self.persistent_writes {
                        output
                    } else {
                        with_ephemeral_workspace_warning(&output)
                    };
                    Ok(ToolResult {
                        success: true,
                        output: output.into(),
                        error: None,
                    })
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(tool_msg_with_args(
                            "tool-file-download-error-move",
                            &[("err", &e.to_string())],
                        )),
                    })
                }
            },
            Err(msg) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(msg),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::schema::{ProxyConfig, ProxyScope, set_runtime_proxy_config};

    /// Serializes the process-global runtime proxy config against every
    /// `execute()` test. `execute()` reads `runtime_proxy_config()` at dispatch
    /// time (to surface the conflicting-proxy error that mirrors http_request /
    /// web_fetch); the real-request execute tests and the proxy-bypass tests
    /// both touch that global, so they must not run concurrently — holding this
    /// lock in every test that reads or writes the global makes the suite
    /// deterministic instead of racing on shared proxy state.
    static PROXY_TEST_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> =
        std::sync::OnceLock::new();

    /// RAII guard that installs a runtime proxy config for a test and restores
    /// the default (proxy disabled) on drop — including on panic, so a failing
    /// proxy-bypass test cannot leak its config into sibling tests. Serializes
    /// against real-request execute tests via [`PROXY_TEST_LOCK`].
    struct RuntimeProxyGuard {
        _lock: tokio::sync::MutexGuard<'static, ()>,
    }

    impl RuntimeProxyGuard {
        async fn install(config: ProxyConfig) -> Self {
            let _lock = PROXY_TEST_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock()
                .await;
            set_runtime_proxy_config(config);
            RuntimeProxyGuard { _lock }
        }
    }

    impl Drop for RuntimeProxyGuard {
        fn drop(&mut self) {
            set_runtime_proxy_config(ProxyConfig::default());
        }
    }

    /// Acquire the proxy test lock for a real-request execute test, so it does
    /// not race with a sibling test that toggles the process-global runtime
    /// proxy. `execute()` reads the global at dispatch time; holding the lock
    /// for the test's whole duration keeps proxy state stable while reading it.
    async fn proxy_test_lock_guard() -> tokio::sync::MutexGuard<'static, ()> {
        PROXY_TEST_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    /// Scoped cleanup for the process-wide log broadcast hook: clears the hook
    /// on drop so a panicking assertion cannot leak the installed hook into
    /// later tests. Declare after `__private_test_hook_lock()` so the clear
    /// runs while the hook lock is still held (guards drop in reverse
    /// declaration order).
    struct BroadcastHookGuard;

    impl Drop for BroadcastHookGuard {
        fn drop(&mut self) {
            zeroclaw_log::clear_broadcast_hook();
        }
    }

    fn test_security(workspace: PathBuf, level: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour: 100,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    fn cfg(url: Option<String>) -> FileDownloadConfig {
        FileDownloadConfig {
            url,
            ..FileDownloadConfig::default()
        }
    }

    /// Build a tool whose DNS resolver counts every invocation and returns a
    /// resolvable loopback answer, so a rejected dispatch can assert zero
    /// resolver I/O deterministically (no reliance on IP-literal short-circuit).
    fn tool_with_counting_resolver(
        security: Arc<SecurityPolicy>,
        config: FileDownloadConfig,
        nat64_prefixes: Vec<String>,
        resolver_calls: Arc<AtomicUsize>,
    ) -> FileDownloadTool {
        let counter = Arc::clone(&resolver_calls);
        let endpoint_resolver: EndpointResolver = Arc::new(
            move |_host: String,
                  port: u16|
                  -> Pin<Box<dyn Future<Output = ResolveResult> + Send>> {
                counter.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    Ok(vec![std::net::SocketAddr::new(
                        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                        port,
                    )])
                })
            },
        );
        let snapshot = FileDownloadSsrfPolicy {
            allowed_private_hosts: config.allowed_private_hosts.clone(),
            nat64_prefixes: nat64_prefixes.to_vec(),
        };
        FileDownloadTool::new_with_endpoint_resolver(
            security,
            config,
            true,
            move || snapshot.clone(),
            endpoint_resolver,
        )
    }

    /// Count files in `dir` whose name marks an in-progress download temp file.
    fn part_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.contains(".part-"))
            })
            .collect()
    }

    #[test]
    fn tool_name_and_description() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/download".into())),
        );
        assert_eq!(tool.name(), "file_download");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_requires_document_id_and_dest_path() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/download".into())),
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("document_id".into())));
        assert!(required.contains(&serde_json::Value::String("dest_path".into())));
        assert_eq!(
            schema["properties"]["document_id"]["description"],
            crate::i18n::get_required_tool_string("tool-file-download-param-document-id")
        );
    }

    #[tokio::test]
    async fn execute_fails_when_url_unset() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(None),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("disabled"));
        assert!(!tmp.path().join("out.bin").exists());
    }

    #[tokio::test]
    async fn execute_blocks_readonly_autonomy() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::ReadOnly),
            cfg(Some("https://example.com/download".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
        assert!(!tmp.path().join("out.bin").exists());
    }

    #[tokio::test]
    async fn execute_errors_on_missing_arguments() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/download".into())),
        );

        assert!(
            tool.execute(json!({ "dest_path": "out.bin" }))
                .await
                .is_err()
        );
        assert!(
            tool.execute(json!({ "document_id": "doc-1" }))
                .await
                .is_err()
        );
        // Present-but-empty values are treated the same as missing.
        assert!(
            tool.execute(json!({ "document_id": "  ", "dest_path": "out.bin" }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn execute_rejects_traversal_dest_path() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/download".into())),
        );

        // A dest_path that terminates in `..` has no concrete file name.
        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "nested/.." }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("concrete file name"));
    }

    #[tokio::test]
    async fn execute_rejects_dest_outside_workspace() {
        let server = MockServer::start().await;
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();

        // The endpoint must never be contacted when the destination is rejected.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"should-not-arrive".to_vec()))
            .expect(0)
            .mount(&server)
            .await;

        let dest_abs = outside.path().join("escape.bin");
        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(workspace.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({
                "document_id": "doc-1",
                "dest_path": dest_abs.to_string_lossy(),
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            !dest_abs.exists(),
            "no file should be written outside workspace"
        );
    }

    #[tokio::test]
    async fn execute_downloads_file_to_dest() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let body = b"the-downloaded-bytes-\x00\x01\x02".to_vec();

        Mock::given(method("GET"))
            .and(path("/download"))
            .and(query_param("document_id", "doc-123"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-123", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        let written = fs::read(tmp.path().join("out.bin")).unwrap();
        assert_eq!(written, body);
        assert!(result.output.contains("out.bin"));
        assert!(
            part_files(tmp.path()).is_empty(),
            "temp file must be cleaned up"
        );
    }

    /// On an ephemeral runtime a successful download lands in a workspace that
    /// won't persist; the output must carry the loud warning while preserving
    /// the original status, and the bytes must still be written.
    #[tokio::test]
    async fn execute_warns_on_ephemeral_workspace() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let body = b"downloaded-bytes".to_vec();

        Mock::given(method("GET"))
            .and(path("/download"))
            .and(query_param("document_id", "doc-eph"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new_with_persistence(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
            false,
            Vec::new(),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-eph", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        assert!(
            result.output.contains("EPHEMERAL WORKSPACE"),
            "ephemeral warning must be present, got: {}",
            result.output
        );
        assert!(result.output.contains("mount_workspace"));
        assert!(
            result.output.contains("out.bin"),
            "original download status must be preserved, got: {}",
            result.output
        );
        assert_eq!(fs::read(tmp.path().join("out.bin")).unwrap(), body);
    }

    #[tokio::test]
    async fn execute_sends_configured_bearer_header() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        Mock::given(method("GET"))
            .and(path("/download"))
            .and(header("Authorization", "Bearer secret-token"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer secret-token".into());
        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            headers,
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();

        // The mock only matches when the Bearer header is present, so success
        // proves the configured header was attached to the request.
        assert!(result.success, "expected success, got {result:?}");
        assert_eq!(fs::read(tmp.path().join("out.bin")).unwrap(), b"ok");
    }

    #[tokio::test]
    async fn execute_reports_non_2xx_without_writing() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not_found"))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "missing", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("404"));
        assert!(!tmp.path().join("out.bin").exists());
        assert!(part_files(tmp.path()).is_empty());
    }

    #[tokio::test]
    async fn execute_rejects_oversized_via_content_length() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        // Body of 2048 bytes; wiremock serves it with a Content-Length header.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 2048]))
            .mount(&server)
            .await;

        let mut config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        config.max_file_size_bytes = 1024;
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "big", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        // The advertised Content-Length must trigger the fast pre-stream reject.
        assert!(
            result.error.unwrap().contains("endpoint reports"),
            "expected the Content-Length fast-reject path"
        );
        assert!(!tmp.path().join("out.bin").exists());
        assert!(
            part_files(tmp.path()).is_empty(),
            "no partial file may remain"
        );
    }

    #[tokio::test]
    async fn execute_rejects_oversized_while_streaming_without_content_length() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        // `Transfer-Encoding: chunked` makes the served response omit
        // Content-Length, so the size ceiling can only be enforced by the
        // streaming accumulator rather than the fast Content-Length check.
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Transfer-Encoding", "chunked")
                    .set_body_bytes(vec![0u8; 4096]),
            )
            .mount(&server)
            .await;

        let mut config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        config.max_file_size_bytes = 1024;
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "big", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        // With no Content-Length, only the streaming accumulator can catch the
        // overage, which emits this distinct message.
        assert!(
            result.error.unwrap().contains("exceeded limit"),
            "expected the streaming size-cap path"
        );
        assert!(!tmp.path().join("out.bin").exists());
        assert!(
            part_files(tmp.path()).is_empty(),
            "no partial file may remain"
        );
    }

    #[tokio::test]
    async fn execute_does_not_follow_redirects_from_configured_endpoint() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/elsewhere", server.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/elsewhere"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"redirected-bytes".to_vec()))
            .expect(0)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap_or("").contains("302"),
            "expected the 302 status to surface; got {result:?}"
        );
        assert!(
            !tmp.path().join("out.bin").exists(),
            "no file may be written when the configured endpoint returns 3xx"
        );
        assert!(
            part_files(tmp.path()).is_empty(),
            "no partial file may remain after a 3xx response"
        );
    }

    #[tokio::test]
    async fn execute_truncates_non_ascii_error_body_safely() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        let mut body = "x".repeat(4094);
        body.push_str("世界世界世界世界世界世界");
        assert!(!body.is_char_boundary(4096));

        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(500).set_body_string(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        // Must not panic when slicing the body at a non-char-boundary byte
        // index. The truncated output must still be valid UTF-8 and must
        // include the "[truncated ...]" marker.
        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("500"));
        assert!(result.output.contains("[truncated"));
        assert!(
            result.output.len() < body.len(),
            "expected the body to be shortened"
        );
        assert!(!tmp.path().join("out.bin").exists());
    }

    // ── SSRF gate tests ────────────────────────────────────────────────
    //
    // The configured `[file_download].url` is operator-only, but a typo or
    // copy-paste (e.g. `http://127.0.0.1`, `http://169.254.169.254/...`,
    // `http://10.0.0.5/...`) must surface as a clear rejection before any
    // network call. Redirects are already disabled by the production code,
    // so the gate only needs to inspect the initial URL host.

    #[tokio::test]
    async fn execute_rejects_loopback_endpoint_without_opt_in() {
        let _proxy_lock = proxy_test_lock_guard().await;
        // No `allowed_private_hosts` and no mock — the rejection must happen
        // before any HTTP call. The endpoint is operator-configured here to a
        // loopback URL; the only way that URL is contacted is if the gate
        // fails, so the test is over-determined (no real server is bound).
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://127.0.0.1:9999/download".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("loopback") || err.contains("private"),
            "SSRF rejection should mention private/loopback; got: {err}"
        );
        assert!(err.contains("allowed_private_hosts"));
        assert!(!tmp.path().join("out.bin").exists());
    }

    #[tokio::test]
    async fn execute_rejects_metadata_endpoint_without_opt_in() {
        let _proxy_lock = proxy_test_lock_guard().await;
        // AWS / GCP / Azure instance metadata services — the canonical
        // SSRF target. `169.254.169.254` is a link-local address; without
        // opt-in the gate must reject it.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://169.254.169.254/latest/meta-data/".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("cloud metadata") || err.contains("credential"),
            "metadata rejection must be operator-visible as metadata; got: {err}"
        );
        assert!(
            !err.contains("To allow this host"),
            "metadata endpoint must not suggest an allowlist; got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_rejects_ecs_credentials_endpoint_without_opt_in() {
        let _proxy_lock = proxy_test_lock_guard().await;
        // AWS ECS task credentials (169.254.170.2) are credential-delivery
        // endpoints. Even though they sit in the 169.254.0.0/16 link-local
        // range and would be rejected as private, the SSRF gate must also
        // classify them as cloud metadata so a future allowlist that lifts
        // the private check cannot re-admit them.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://169.254.170.2/credentials".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("cloud metadata") || err.contains("credential"),
            "credential-delivery rejection must be operator-visible as metadata; got: {err}"
        );
        assert!(
            !err.contains("To allow this host"),
            "credential-delivery endpoint must not suggest an allowlist; got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_rejects_rfc1918_endpoint_without_opt_in() {
        let _proxy_lock = proxy_test_lock_guard().await;
        // 10.0.0.0/8 private range. No mock — the rejection is a string
        // comparison and must happen before any TCP connect.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://10.0.0.5/internal/file".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("private"));
    }

    #[tokio::test]
    async fn execute_rejects_localhost_name_without_opt_in() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://localhost:8080/file".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("private"));
    }

    #[tokio::test]
    async fn execute_rejects_userinfo_in_endpoint_url() {
        let _proxy_lock = proxy_test_lock_guard().await;
        // `user@host` form is a separate SSRF vector (userinfo can sneak
        // through naive host parsers). The extractor rejects it before the
        // private-host check.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://attacker@example.com/file".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("userinfo"));
    }

    #[tokio::test]
    async fn execute_allows_loopback_endpoint_with_explicit_opt_in() {
        let _proxy_lock = proxy_test_lock_guard().await;
        // The legitimate internal-document-service case: operator opts the
        // loopback IP into `allowed_private_hosts` and the gate lets it
        // through. The endpoint still has to actually serve the file —
        // we wiremock it and verify the success path.
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let body = b"internal-bytes".to_vec();

        Mock::given(method("GET"))
            .and(path("/download"))
            .and(query_param("document_id", "doc-int"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        // `server.uri()` is `http://127.0.0.1:port`; allowlist covers it.
        let config = FileDownloadConfig {
            url: Some(format!("{}/download", server.uri())),
            allowed_private_hosts: vec!["127.0.0.1".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-int", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(result.success, "expected success, got {result:?}");
        assert_eq!(fs::read(tmp.path().join("out.bin")).unwrap(), body);
    }

    #[tokio::test]
    async fn validate_endpoint_host_wildcard_lifts_literal_private_host_block() {
        // Wildcard semantics: `"*"` in `allowed_private_hosts` lifts the
        // literal private-host block for the host classifier — but the
        // classifier is *only* the host classifier. The wildcard does not
        // widen the tool to non-private hosts, it does not bypass
        // redirect validation (redirects are still disabled), and it does
        // not turn the gate into a blanket bypass of classification.
        //
        // This test pins that contract by calling `validate_endpoint_host`
        // directly (no network I/O, no metadata-service request) for both
        // sides of the contract:
        //
        // - With `"*"` in the allowlist, a non-metadata private IP
        //   (10.0.0.1) is admitted at the gate (returns `Ok`); a future
        //   refactor that re-tightens the wildcard back to a literal-only
        //   check would surface as `Err` here.
        // - With `"*"` removed, the same URL is rejected with the
        //   `private-host` error so the wildcard is the *only* reason
        //   the URL would have been admitted.
        let tmp = TempDir::new().unwrap();

        // Side 1: wildcard set → gate admits a non-metadata private IP.
        let tool_with_wildcard = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            FileDownloadConfig {
                url: Some("http://10.0.0.1/test.bin".into()),
                allowed_private_hosts: vec!["*".into()],
                ..FileDownloadConfig::default()
            },
        );
        let (admitted_host, _) = tool_with_wildcard
            .validate_endpoint_host("http://10.0.0.1/test.bin")
            .await
            .expect("wildcard must lift the literal private-host block for a non-metadata IP");
        assert_eq!(admitted_host, "10.0.0.1");

        // Side 2: no wildcard → same host is rejected (proves the
        // wildcard is the only reason the URL is admitted above).
        let tool_without_wildcard = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            FileDownloadConfig {
                url: Some("http://10.0.0.1/test.bin".into()),
                allowed_private_hosts: Vec::new(),
                ..FileDownloadConfig::default()
            },
        );
        let rejected = tool_without_wildcard
            .validate_endpoint_host("http://10.0.0.1/test.bin")
            .await
            .expect_err("without wildcard the private host must be rejected");
        assert!(
            rejected.contains("10.0.0.1"),
            "expected the SSRF rejection string, got: {rejected}"
        );
    }

    /// Cloud metadata IPs (e.g. 169.254.169.254) are rejected even with
    /// the wildcard opt-in, because `validate_resolved_ips_exclude_metadata`
    /// applies unconditionally. This pins the metadata-IP carve-out from
    /// the shared SSRF policy.
    #[tokio::test]
    async fn validate_endpoint_host_wildcard_does_not_lift_metadata_block() {
        // The private-host carve-out must never lift the metadata / credential
        // delivery exclusion. Covers EC2 IMDS plus the AWS ECS task and EKS
        // Pod Identity credential-delivery endpoints introduced with the
        // shared `is_cloud_metadata_ip` classification.
        let cases = [
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.170.2/credentials",
            "http://169.254.170.23/credentials",
        ];
        for url in cases {
            let tmp = TempDir::new().unwrap();
            let tool_with_wildcard = FileDownloadTool::new(
                test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
                FileDownloadConfig {
                    url: Some(url.into()),
                    allowed_private_hosts: vec!["*".into()],
                    ..FileDownloadConfig::default()
                },
            );
            let rejected = tool_with_wildcard
                .validate_endpoint_host(url)
                .await
                .expect_err("wildcard must NOT lift the metadata-IP block");
            // `ssrf_check_endpoint` wraps every rejection in the operator-facing
            // private-host message keyed by the host, so the host string is the
            // stable marker. The metadata-IP classification itself is pinned at
            // the `domain_guard::validate_resolved_ips_exclude_metadata` layer
            // (see `validate_resolved_ips_blocks_*_metadata_even_for_private_opt_in`).
            assert!(
                rejected.contains("169.254") || rejected.contains("cloud metadata"),
                "expected the metadata rejection string for {url}, got: {rejected}"
            );
        }
    }

    #[tokio::test]
    async fn execute_rejects_non_http_scheme() {
        let _proxy_lock = proxy_test_lock_guard().await;
        // `file://` / `gopher://` / etc. must surface as a clear scheme
        // error before any I/O. The endpoint is operator-configured, but a
        // hand-edited TOML can still smuggle a non-HTTP scheme.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("file:///etc/passwd".into())),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("http://") || err.contains("scheme"),
            "got: {err}"
        );
    }

    #[test]
    fn extract_download_url_host_preserves_trailing_dot() {
        // Pins that the transport host preserves trailing dots for exact
        // reqwest resolve_to_addrs binding. The policy layer canonicalizes
        // by stripping the dot.
        assert_eq!(
            extract_download_url_host("https://Example.com.:8443/path").unwrap(),
            "example.com."
        );
        assert_eq!(
            extract_download_url_host("http://files.corp.lan./api").unwrap(),
            "files.corp.lan."
        );
        // Non-dotted forms pass through unchanged
        assert_eq!(
            extract_download_url_host("https://example.com:8443/path").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn extract_download_url_host_handles_canonical_forms() {
        // Pins the helper that the SSRF gate sits on top of. The canonical host
        // is obtained through reqwest::Url so it matches what the transport will
        // actually contact.
        assert_eq!(
            extract_download_url_host("https://Example.com:8443/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_download_url_host("http://10.0.0.5/").unwrap(),
            "10.0.0.5"
        );
        assert_eq!(
            extract_download_url_host("https://example.com").unwrap(),
            "example.com"
        );
        // userinfo rejected.
        assert!(extract_download_url_host("https://user@example.com").is_err());
        assert!(extract_download_url_host("https://user:pass@example.com").is_err());
        // IPv6 unsupported (file_download doesn't speak v6).
        assert!(extract_download_url_host("https://[::1]/p").is_err());
        // Wrong scheme.
        assert!(extract_download_url_host("ftp://example.com/").is_err());
        // Garbage URL — the parser rejects non-URL input.
        assert!(extract_download_url_host("not-a-url").is_err());
    }

    #[test]
    fn parse_endpoint_url_returns_transport_and_policy_hosts() {
        // Pins the two-identity contract: transport_host preserves trailing dot,
        // policy_host canonicalizes by stripping it.
        let (transport, policy, port) =
            parse_endpoint_url("http://files.corp.lan.:8080/api").unwrap();
        assert_eq!(
            transport, "files.corp.lan.",
            "transport host must preserve trailing dot"
        );
        assert_eq!(
            policy, "files.corp.lan",
            "policy host must strip trailing dot"
        );
        assert_eq!(port, 8080);

        // Non-dotted form: transport and policy are identical
        let (transport, policy, port) =
            parse_endpoint_url("http://files.corp.lan:8080/api").unwrap();
        assert_eq!(transport, "files.corp.lan");
        assert_eq!(policy, "files.corp.lan");
        assert_eq!(port, 8080);

        // IP literal: transport and policy are identical
        let (transport, policy, port) = parse_endpoint_url("http://10.0.0.5:9000/api").unwrap();
        assert_eq!(transport, "10.0.0.5");
        assert_eq!(policy, "10.0.0.5");
        assert_eq!(port, 9000);
    }

    #[test]
    fn parse_endpoint_url_accepts_uppercase_scheme() {
        // URL schemes are case-insensitive (RFC 3986 §3.1): an operator
        // may write `HTTPS://...` and it must not regress to a scheme error.
        let (transport, policy, port) =
            parse_endpoint_url("HTTPS://Example.com.:8443/api").unwrap();
        assert_eq!(transport, "example.com.");
        assert_eq!(policy, "example.com");
        assert_eq!(port, 8443);

        // Mixed-case scheme also accepted; non-http(s) schemes still rejected.
        let (transport, _, _) = parse_endpoint_url("HtTp://example.com:8080/x").unwrap();
        assert_eq!(transport, "example.com");
        let err = parse_endpoint_url("ftp://example.com:21/x").unwrap_err();
        assert!(
            err.contains("http://") || err.contains("scheme"),
            "non-http scheme must still be rejected; got: {err}"
        );
    }

    #[test]
    fn parse_endpoint_url_bad_scheme_does_not_echo_credentials() {
        // A non-http(s) endpoint that carries userinfo or a query with a
        // credential must not echo that secret in the model-visible error.
        // The bad-scheme error reports only the scheme, never the full URL, so
        // an operator-configured `ftp://user:pass@host/file` leaks neither the
        // userinfo nor the query.
        let err =
            parse_endpoint_url("ftp://user:secret@example.invalid:21/file?v=key").unwrap_err();
        assert!(
            !err.contains("user:secret") && !err.contains("secret") && !err.contains("key"),
            "bad-scheme error must not echo URL credentials; got: {err}"
        );
        assert!(
            err.contains("ftp") && err.contains("http://"),
            "bad-scheme error should still identify the rejected scheme; got: {err}"
        );
    }

    #[tokio::test]
    async fn resolver_seam_receives_exact_dotted_transport_host() {
        // The preflight resolver must receive the exact transport hostname
        // (terminal DNS dot preserved) for a dotted URL. `policy_host` strips
        // the dot for allowlist comparison, but the DNS lookup must be for the
        // absolute name — otherwise resolver search-list behavior can
        // substitute a different relative name whose addresses get validated
        // and then pinned to the request.
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let tmp = TempDir::new().unwrap();
        let config = FileDownloadConfig {
            url: Some("http://files.corp.lan.:8080/x".into()),
            allowed_private_hosts: vec!["files.corp.lan".into()],
            ..FileDownloadConfig::default()
        };
        let snapshot = FileDownloadSsrfPolicy {
            allowed_private_hosts: config.allowed_private_hosts.clone(),
            nat64_prefixes: Vec::new(),
        };
        let endpoint_resolver: EndpointResolver = Arc::new({
            let captured = Arc::clone(&captured);
            move |host: String, port: u16| -> Pin<Box<dyn Future<Output = ResolveResult> + Send>> {
                *captured.lock().unwrap() = host.clone();
                Box::pin(async move {
                    Ok(vec![std::net::SocketAddr::new(
                        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                        port,
                    )])
                })
            }
        });
        let tool = FileDownloadTool::new_with_endpoint_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
            true,
            move || snapshot.clone(),
            endpoint_resolver,
        );
        let (transport, addrs) = tool
            .validate_endpoint_host("http://files.corp.lan.:8080/x")
            .await
            .expect("dotted allowlisted host must validate");
        assert_eq!(
            *captured.lock().unwrap(),
            "files.corp.lan.",
            "the resolver must receive the exact dotted transport host"
        );
        assert_eq!(transport, "files.corp.lan.");
        assert_eq!(addrs.len(), 1);
    }

    #[tokio::test]
    async fn execute_validates_and_connects_exact_dotted_transport_host() {
        let _proxy_lock = proxy_test_lock_guard().await;
        // Production-path identity: the validated address set (returned by the
        // resolver, then bound via resolve_to_addrs) must belong to the exact
        // dotted transport hostname the request connects to. If validation had
        // resolved a dot-stripped name, the pinned addresses could come from a
        // different service than the request's absolute host.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hit"))
            .expect(1)
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let config = FileDownloadConfig {
            url: Some(format!(
                "http://files.corp.invalid.:{}/probe",
                server.address().port()
            )),
            allowed_private_hosts: vec!["files.corp.invalid".into()],
            ..FileDownloadConfig::default()
        };
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let bound = *server.address();
        let snapshot = FileDownloadSsrfPolicy {
            allowed_private_hosts: config.allowed_private_hosts.clone(),
            nat64_prefixes: Vec::new(),
        };
        let endpoint_resolver: EndpointResolver = Arc::new({
            let captured = Arc::clone(&captured);
            move |host: String, _port: u16| -> Pin<Box<dyn Future<Output = ResolveResult> + Send>> {
                *captured.lock().unwrap() = host.clone();
                Box::pin(async move { Ok(vec![bound]) })
            }
        });
        let tool = FileDownloadTool::new_with_endpoint_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
            true,
            move || snapshot.clone(),
            endpoint_resolver,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .expect("tool execute must return a ToolResult");
        assert!(
            result.success,
            "dotted hostname must download through the validated address set"
        );
        assert_eq!(
            *captured.lock().unwrap(),
            "files.corp.invalid.",
            "validation must resolve the exact dotted transport host"
        );
        // wiremock's expect(1) is the authoritative detector that the request
        // connected through the validated address set.
    }

    #[test]
    fn host_matches_allowlist_undotted_allows_dotted() {
        // Allows an exact match; it does not itself normalize a trailing DNS
        // dot. Trailing-dot handling lives in the layers above: the policy host
        // is stripped of any terminal dot by `parse_endpoint_url`, and each
        // allowlist entry by `normalize_allowed_private_hosts`, before
        // `host_matches_allowlist` runs. Pin that the two sides agree once
        // normalized, and that match is exact on the already-normalized forms.
        assert!(
            domain_guard::host_matches_allowlist("files.corp.lan", &["files.corp.lan".to_string()]),
            "undotted must match undotted"
        );
        // Both a host and an allowlist entry carrying the same terminal dot are
        // still the same name after normalization (the caller strips it), so
        // match remains exact on the canonical form.
        assert!(
            domain_guard::host_matches_allowlist("files.corp.lan", &["files.corp.lan".to_string()]),
            "canonical (no trailing dot) host must match the canonical allowlist entry"
        );
        // A dotted form must not silently match an undotted one at this layer;
        // only the normalization step (tested separately) brings them together.
        assert!(
            !domain_guard::host_matches_allowlist(
                "files.corp.lan.",
                &["files.corp.lan".to_string()]
            ),
            "match layer is exact and does not strip trailing dots"
        );
    }

    #[test]
    fn normalize_allowed_private_hosts_strips_trailing_dot() {
        // Pins that normalization strips trailing dots so "files.corp.lan."
        // becomes "files.corp.lan" for allowlist comparison.
        let allowed = vec!["files.corp.lan.".into(), "internal.corp".into()];
        let normalized = normalize_allowed_private_hosts(&allowed);
        assert!(normalized.contains(&"files.corp.lan".to_string()));
        assert!(normalized.contains(&"internal.corp".to_string()));
        assert!(
            !normalized.iter().any(|h| h.ends_with('.')),
            "normalized hosts should not have trailing dots"
        );
    }

    /// Alternate and percent-encoded IPv4 loopback forms must classify as
    /// `127.0.0.1` — the same canonical host that reqwest's transport contacts.
    /// This pins the SSRF-bypass fix: the gate no longer does manual string
    /// splitting that sees a bare integer and lets it through as non-private.
    #[test]
    fn extract_download_url_host_canonicalises_alternate_ipv4_loopback() {
        // Decimal IPv4: http://2130706433/ → 127.0.0.1
        assert_eq!(
            extract_download_url_host("http://2130706433/path").unwrap(),
            "127.0.0.1"
        );
        // Hex IPv4: http://0x7f000001/ → 127.0.0.1
        assert_eq!(
            extract_download_url_host("http://0x7f000001/path").unwrap(),
            "127.0.0.1"
        );
        // Octal IPv4: http://0177.0.0.1/ → 127.0.0.1
        assert_eq!(
            extract_download_url_host("http://0177.0.0.1/path").unwrap(),
            "127.0.0.1"
        );
        // Dotted-quad with leading zeros (some parsers normalise these).
        assert_eq!(
            extract_download_url_host("http://127.0.0.1/path").unwrap(),
            "127.0.0.1"
        );
    }

    /// Percent-encoded loopback host: the URL parser percent-decodes the
    /// authority, so a percent-encoded `127.0.0.1` becomes canonical
    /// `127.0.0.1` and is blockable by the private-host check. This test
    /// pins that the gate sees the canonical form rather than the encoded
    /// wrapper.
    #[test]
    fn extract_download_url_host_canonicalises_percent_encoded_loopback() {
        let host = extract_download_url_host("http://%31%32%37%2e%30%2e%30%2e%31/").unwrap();
        assert_eq!(host, "127.0.0.1");
    }

    #[tokio::test]
    async fn validate_endpoint_host_surfaces_loopback_audit_signal() {
        // The rejection path emits a structured audit log event. We don't
        // capture logs here — this test just pins the gating decision so
        // future refactors can't silently drop the SSRF check.
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://127.0.0.1:9000/".into())),
        );
        let result = tool.validate_endpoint_host("http://127.0.0.1:9000/").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("private"));
    }

    // ── SSRF regression tests ────────────────────────────────────────
    //
    // These cover the transport boundary and DNS ordering requirements:
    // - Transport tests must drive a real hostname through the full gate
    //   and prove the reqwest client connects only to the validated
    //   address set.
    // - DNS resolution must run AFTER local authorization / arg /
    //   destination validation, so read-only / missing-arg /
    //   traversal-rejected calls never reach the resolver.

    /// Full round-trip with a real hostname (`localhost`) allowlisted.
    /// The wiremock is mounted on `127.0.0.1:<port>`; the configured URL
    /// uses `localhost:<port>`. With `localhost` in `allowed_private_hosts`,
    /// the SSRF gate admits the request and the wiremock receives exactly
    /// one GET — proving the private-host path is reachable when the
    /// operator has explicitly opted in. The counterpart test
    /// `execute_rejects_localhost_name_without_opt_in` proves the gate
    /// rejects the same hostname when the allowlist is empty.
    #[tokio::test]
    async fn execute_allows_private_hostname_via_local_mock_when_allowlisted() {
        let _proxy_lock = proxy_test_lock_guard().await;
        let server = MockServer::start().await;
        let mock_port = server.address().port();
        let tmp = TempDir::new().unwrap();
        let body = b"private-hostname-bytes".to_vec();

        Mock::given(method("GET"))
            .and(path("/x"))
            .and(query_param("document_id", "doc-priv"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        // Use `localhost` as the URL hostname (resolved by /etc/hosts to
        // 127.0.0.1) so this test exercises the real DNS path through the
        // SSRF gate, not an IP-literal shortcut. The wiremock listens on
        // 127.0.0.1:<mock_port>; the URL is `http://localhost:<mock_port>/x`.
        let config = FileDownloadConfig {
            url: Some(format!("http://localhost:{mock_port}/x")),
            allowed_private_hosts: vec!["localhost".into()],
            ..FileDownloadConfig::default()
        };
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "document_id": "doc-priv", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        let on_disk = fs::read(tmp.path().join("out.bin")).unwrap();
        assert_eq!(on_disk, body);
        // Sanity: wiremock must have observed exactly the one GET.
        let received = server.received_requests().await.expect("infallible");
        assert_eq!(received.len(), 1);
    }

    /// Direct unit test on `ssrf_check_endpoint` over a hand-crafted IP
    /// set. Deterministic without controlling real DNS: the synthetic
    /// hostname `public-looking.example.com` is paired with both private
    /// and public IP sets so we can prove the rejection is private-driven,
    /// not hostname-driven, and that the allowlist correctly lifts the
    /// non-global check (but never the metadata carve-out — covered by
    /// `validate_endpoint_host_wildcard_does_not_lift_metadata_block`).
    #[test]
    fn ssrf_check_endpoint_rejects_hostname_resolving_to_private_ip_without_opt_in() {
        // Side A: public-looking hostname + private IP + no allowlist →
        // rejected. The user-facing message names the host (the IP is
        // captured in the structured `$err` arg but not interpolated in
        // the catalogue — the policy fires, but only `host` is shown to
        // the operator). The shape of the rejection — host-named,
        // private-driven — proves the policy fired correctly. The IP-
        // set classifier is the trigger here (10.0.0.5 is RFC1918, the
        // host classifier on `public-looking.example.com` returns false).
        let err = ssrf_check_endpoint(
            "public-looking.example.com",
            &[std::net::SocketAddr::from(([10, 0, 0, 5], 80))],
            &[],
            &[],
        )
        .expect_err("private IP without opt-in must be rejected");
        assert!(
            err.contains("public-looking.example.com"),
            "rejection should name the host; got: {err}"
        );
        assert!(
            err.contains("private"),
            "rejection should mention private; got: {err}"
        );

        // Side B: same hostname + same private IP + allowlisted hostname
        // → admitted. Proves the rejection above is driven by the policy,
        // not by the synthetic hostname shape or the IP literal.
        ssrf_check_endpoint(
            "public-looking.example.com",
            &[std::net::SocketAddr::from(([10, 0, 0, 5], 80))],
            &["public-looking.example.com".into()],
            &[],
        )
        .expect("allowlisted hostname with non-metadata private IP must pass");

        // Side C: same hostname + public IP → always admitted (proves
        // policy is private-driven, not hostname-driven).
        ssrf_check_endpoint(
            "public-looking.example.com",
            &[std::net::SocketAddr::from(([8, 8, 8, 8], 443))],
            &[],
            &[],
        )
        .expect("public-IP hostname must pass without opt-in");
    }

    /// Operator-visibility contract for metadata rejections: the error must
    /// NOT suggest adding the host to `allowed_private_hosts`, because the
    /// metadata/credential exclusion is non-overridable (schema contract). A
    /// future wording change that implies the allowlist can lift it would be
    /// false remediation.
    #[test]
    fn ssrf_check_endpoint_metadata_rejection_has_no_allowlist_suggestion() {
        // EC2 IMDS — rejected even with a wildcard allowlist.
        let err = ssrf_check_endpoint(
            "metadata.example.com",
            &[std::net::SocketAddr::from(([169, 254, 169, 254], 80))],
            &["*".into()],
            &[],
        )
        .expect_err("metadata address must be rejected even under a wildcard allowlist");
        assert!(
            err.contains("cloud metadata") || err.contains("credential"),
            "metadata rejection must be operator-visible as such; got: {err}"
        );
        assert!(
            !err.contains("To allow this host")
                && !err.contains("add it")
                && !err.contains("To allow this"),
            "metadata rejection must NOT instruct the operator to add the host to \
             the allowlist (it cannot lift the exclusion); got: {err}"
        );
        // It MAY name the config key to say the allowlist does NOT apply.
        assert!(
            err.contains("cannot be enabled"),
            "metadata rejection should state the allowlist cannot lift it; got: {err}"
        );

        // EKS Pod Identity credentials — same contract through the non-opt-in path.
        let err2 = ssrf_check_endpoint(
            "eks-credentials.example.com",
            &[std::net::SocketAddr::from(([169, 254, 170, 23], 80))],
            &[],
            &[],
        )
        .expect_err("EKS credential address must be rejected");
        assert!(
            err2.contains("cloud metadata") || err2.contains("credential"),
            "EKS credential rejection must be operator-visible as such; got: {err2}"
        );
        assert!(
            !err2.contains("To allow this host") && !err2.contains("add it"),
            "EKS credential rejection must NOT instruct adding to the allowlist; got: {err2}"
        );
    }

    /// Operator-visibility contract for the SSRF audit events: every blocked
    /// endpoint emits a WARN rejection, an allowlisted host whose resolved
    /// addresses actually use the private carve-out emits an INFO admission,
    /// and a wildcard allowlist that admits a public endpoint must NOT claim
    /// it as private.
    #[test]
    fn ssrf_check_endpoint_audit_events_match_decisions() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        let _hook_cleanup = BroadcastHookGuard;
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        // 1. Rejection: private IP without opt-in → WARN reject event.
        ssrf_check_endpoint(
            "blocked.example.com",
            &[std::net::SocketAddr::from(([10, 0, 0, 5], 80))],
            &[],
            &[],
        )
        .expect_err("private IP without opt-in must be rejected");

        // 2. Admission via carve-out: allowlisted hostname resolving to a
        //    private IP → INFO "allowing private host" event.
        ssrf_check_endpoint(
            "corp-files.example.com",
            &[std::net::SocketAddr::from(([10, 0, 0, 9], 80))],
            &["corp-files.example.com".into()],
            &[],
        )
        .expect("allowlisted private resolve must pass");

        // 3. Wildcard allowlist + public resolve → passes but must NOT emit
        //    the private-carve-out INFO event.
        ssrf_check_endpoint(
            "public.example.com",
            &[std::net::SocketAddr::from(([8, 8, 8, 8], 443))],
            &["*".into()],
            &[],
        )
        .expect("public IP must pass");

        let mut warn_found = false;
        let mut info_found = false;
        let mut wildcard_public_info_found = false;
        while let Ok(value) = rx.try_recv() {
            let msg = value.get("message").and_then(|v| v.as_str()).unwrap_or("");
            if msg.contains("rejected private/local endpoint host") {
                warn_found = true;
                assert_eq!(
                    value.get("severity_text").and_then(|v| v.as_str()),
                    Some("WARN"),
                    "rejection must be a WARN event: {value}"
                );
            }
            if msg.contains("allowing private host via allowed_private_hosts") {
                info_found = true;
                assert_eq!(
                    value.get("severity_text").and_then(|v| v.as_str()),
                    Some("INFO"),
                    "carve-out admission must be an INFO event: {value}"
                );
                let host = value
                    .get("attributes")
                    .and_then(|v| v.get("host"))
                    .and_then(|v| v.as_str());
                if host == Some("public.example.com") {
                    wildcard_public_info_found = true;
                }
            }
        }
        assert!(
            warn_found,
            "a blocked endpoint must emit a WARN rejection audit event"
        );
        assert!(
            info_found,
            "a real private carve-out admission must emit an INFO audit event"
        );
        assert!(
            !wildcard_public_info_found,
            "a wildcard allowlist admitting a public endpoint must not log it as private"
        );
    }

    #[test]
    fn ssrf_check_endpoint_rejects_nat64_non_global_targets_without_opt_in() {
        for (ip, host) in [
            ("64:ff9b::a00:1", "internal.example.com"), // embeds 10.0.0.1
            ("64:ff9b::7f00:1", "loopback.example.com"), // embeds 127.0.0.1
            ("64:ff9b::a9fe:1", "link-local.example.com"), // embeds 169.254.0.1
        ] {
            let addr = std::net::SocketAddr::new(ip.parse().unwrap(), 80);
            let err = ssrf_check_endpoint(host, &[addr], &[], &[])
                .expect_err("empty allowlist must reject a NAT64-synthesized non-global target");
            let msg = err.to_lowercase();
            assert!(
                msg.contains("private")
                    || msg.contains("non-global")
                    || msg.contains("loopback")
                    || msg.contains("link-local"),
                "NAT64 target {ip} ({host}) must be rejected on the public path; got: {err}"
            );
        }

        // Positive control: a NAT64 form embedding a genuinely public IPv4
        // (64:ff9b::808:808 embeds 8.8.8.8) reaches the same public endpoint
        // as the IPv4 form and must NOT be rejected.
        let public_addr = std::net::SocketAddr::new("64:ff9b::808:808".parse().unwrap(), 443);
        ssrf_check_endpoint("public.example.com", &[public_addr], &[], &[])
            .expect("NAT64 embedding a public IPv4 must pass without opt-in");
    }

    /// Operator-declared network-specific NAT64 prefix (RFC 6052 §2.2): a
    /// DNS64 answer under a declared prefix embedding a non-global IPv4 must
    /// be rejected on the empty-allowlist public path, exactly like the
    /// built-in `64:ff9b::/96` well-known form. Unlike the well-known prefix,
    /// a network-specific prefix cannot be detected from an address alone, so
    /// this only closes the path once the operator declares it.
    #[test]
    fn ssrf_check_endpoint_rejects_declared_nat64_non_global_without_opt_in() {
        let prefixes = [zeroclaw_infra::net_guard::Nat64Prefix::parse("64:ff9b:1::/48").unwrap()];
        // RFC 6052 §2.2 /48 embedding: bytes 6-7 + u + bytes 9-10.
        for (ip, host) in [
            ("64:ff9b:1:a00:0:100::", "internal.example.com"), // embeds 10.0.0.1
            ("64:ff9b:1:7f00:0:100::", "loopback.example.com"), // embeds 127.0.0.1
            ("64:ff9b:1:a9fe:0:100::", "link-local.example.com"), // embeds 169.254.0.1
        ] {
            let addr = std::net::SocketAddr::new(ip.parse().unwrap(), 80);
            let err = ssrf_check_endpoint(host, &[addr], &[], &prefixes)
                .expect_err("declared NAT64 prefix embedding a non-global target must be rejected");
            let msg = err.to_lowercase();
            assert!(
                msg.contains("private")
                    || msg.contains("non-global")
                    || msg.contains("loopback")
                    || msg.contains("link-local"),
                "declared NAT64 target {ip} ({host}) must be rejected on the public path; got: {err}"
            );
        }
    }

    /// Declared-prefix metadata: an address under a declared prefix embedding
    /// a metadata/credential IPv4 must be rejected EVEN when the host is
    /// allowlisted — the carve-out never lifts the metadata exclusion, so a
    /// DNS64 answer cannot route the gate to EC2 IMDS / ECS / EKS.
    #[test]
    fn ssrf_check_endpoint_rejects_declared_nat64_metadata_even_allowlisted() {
        let prefixes = [zeroclaw_infra::net_guard::Nat64Prefix::parse("64:ff9b:1::/48").unwrap()];
        // 64:ff9b:1:a9fe:a9:fe00:: embeds 169.254.169.254 (EC2 IMDS).
        let addr = std::net::SocketAddr::new("64:ff9b:1:a9fe:a9:fe00::".parse().unwrap(), 80);
        for allowed in [
            &["*".to_string()][..],
            &["corp.example.com".to_string()][..],
        ] {
            let err = ssrf_check_endpoint("corp.example.com", &[addr], allowed, &prefixes)
                .expect_err("declared NAT64 metadata must be rejected even under an allowlist");
            assert!(
                err.contains("cloud metadata") || err.contains("credential"),
                "must be operator-visible as metadata; got: {err}"
            );
        }
    }

    /// Declared-prefix positive control + allowlist carve-out: a declared
    /// prefix embedding a public IPv4 passes without opt-in; embedding a
    /// private IPv4 is admitted only once the host is allowlisted (the same
    /// carve-out the well-known form has).
    #[test]
    fn ssrf_check_endpoint_declared_nat64_public_and_carveout() {
        // Declared prefix must be in genuine global unicast space: the base
        // validator flags `2001:db8::/32` (RFC 3849 documentation) as
        // non-global before NAT64 logic runs, which would mask this control.
        let prefixes =
            [zeroclaw_infra::net_guard::Nat64Prefix::parse("2606:4700:64::/96").unwrap()];
        // 2606:4700:64::808:808 embeds 8.8.8.8 (public).
        let public_addr = std::net::SocketAddr::new("2606:4700:64::808:808".parse().unwrap(), 443);
        ssrf_check_endpoint("public.example.com", &[public_addr], &[], &prefixes)
            .expect("declared NAT64 embedding a public IPv4 must pass without opt-in");

        // 2606:4700:64::a00:1 embeds 10.0.0.1 (RFC 1918).
        let private_addr = std::net::SocketAddr::new("2606:4700:64::a00:1".parse().unwrap(), 80);
        ssrf_check_endpoint("internal.example.com", &[private_addr], &[], &prefixes)
            .expect_err("declared NAT64 private target without opt-in must be rejected");
        // Allowlisted hostname → admitted via the carve-out.
        ssrf_check_endpoint(
            "internal.example.com",
            &[private_addr],
            &["internal.example.com".into()],
            &prefixes,
        )
        .expect("allowlisted hostname resolving through a declared NAT64 prefix must pass");
    }

    /// The RFC 8215 local-use space (`64:ff9b:1::/48`) is outside the global
    /// IPv6 unicast allocation, so the shared `net_guard` classifier treats any
    /// address there as non-global and the public validator rejects it without
    /// needing a declared prefix — it is never silently admitted as "SSRF-safe".
    #[test]
    fn ssrf_check_endpoint_undeclared_prefix_is_ordinary_address_space() {
        // 64:ff9b:1:a00:0:100:: embeds 10.0.0.1 under the RFC 8215 local-use
        // prefix; with NO declared prefixes it is still classified as a
        // non-global IPv6 address and rejected.
        let addr = std::net::SocketAddr::new("64:ff9b:1:a00:0:100::".parse().unwrap(), 80);
        ssrf_check_endpoint("internal.example.com", &[addr], &[], &[])
            .expect_err("RFC 8215 local-use prefix must be rejected as non-global IPv6");
    }

    /// Same contract as `ssrf_check_endpoint_undeclared_prefix_is_ordinary_
    /// address_space`, but on the ALLOWLISTED path (where the wildcard opts in
    /// to private-host rejection being lifted). A DNS64 answer embedding a cloud
    /// metadata / credential IPv4 under a network-specific prefix that is NOT
    /// declared still passes the allowlisted validator: RFC 8215 §5 forbids
    /// assuming where an embedded IPv4 sits inside the local-use space, so the
    /// gate cannot auto-detect it and the operator must declare the prefix in
    /// `nat64_prefixes`. This regression pins that documented contract boundary
    /// — it does NOT assert SSRF safety, and is deliberately paired with the
    /// declared-prefix regressions below that actually reject the same embedding
    /// once the prefix is declared.
    #[test]
    fn ssrf_check_endpoint_undeclared_prefix_metadata_passes_allowlisted_path() {
        // 64:ff9b:1:a9fe:a9:fe00:: embeds 169.254.169.254 (EC2 IMDS) under the
        // RFC 8215 local-use prefix, but with NO declared prefixes the shared
        // classifier cannot recognize it (the well-known-only path checks
        // 64:ff9b::/96), so on the wildcard-carve-out path the gate admits it.
        let addr = std::net::SocketAddr::new("64:ff9b:1:a9fe:a9:fe00::".parse().unwrap(), 80);
        ssrf_check_endpoint("internal.example.com", &[addr], &["*".to_string()], &[]).expect(
            "undeclared network-specific prefix is outside auto-detection even on the \
             allowlisted path; operators using this prefix must declare it in nat64_prefixes",
        );
    }

    /// RFC 6052 §2.2 nonzero-suffix forms must be classified by their embedded
    /// IPv4, not treated as ordinary public IPv6. A compliant translator
    /// IGNORES nonzero reserved suffix bits (the RFC says it proceeds as if
    /// they were zero), so `64:ff9b:1:a00:0:100:0:1` is routed to 10.0.0.1 on
    /// the wire even though byte 15 is nonzero.
    #[test]
    fn ssrf_check_endpoint_rejects_declared_nat64_nonzero_suffix_private_without_opt_in() {
        let prefixes = [zeroclaw_infra::net_guard::Nat64Prefix::parse("64:ff9b:1::/48").unwrap()];
        // 64:ff9b:1:a00:0:100:0:1 embeds 10.0.0.1 with nonzero suffix byte 15.
        let addr = std::net::SocketAddr::new("64:ff9b:1:a00:0:100:0:1".parse().unwrap(), 80);
        let err = ssrf_check_endpoint("internal.example.com", &[addr], &[], &prefixes)
            .expect_err("nonzero-suffix declared NAT64 private target must be rejected");
        assert!(
            err.contains("non-global") || err.contains("private") || err.contains("10.0.0.1"),
            "must be classified by the embedded IPv4; got: {err}"
        );
    }

    /// A nonzero-suffix address under a declared prefix embedding a
    /// metadata/credential IPv4 is rejected EVEN under an allowlist — same
    /// contract as the zero-suffix form.
    #[test]
    fn ssrf_check_endpoint_rejects_declared_nat64_nonzero_suffix_metadata_even_allowlisted() {
        let prefixes = [zeroclaw_infra::net_guard::Nat64Prefix::parse("64:ff9b:1::/48").unwrap()];
        // 64:ff9b:1:a9fe:a9:fe00:0:1 embeds 169.254.169.254 (EC2 IMDS) with
        // nonzero suffix byte 15.
        let addr = std::net::SocketAddr::new("64:ff9b:1:a9fe:a9:fe00:0:1".parse().unwrap(), 80);
        for allowed in [
            &["*".to_string()][..],
            &["corp.example.com".to_string()][..],
        ] {
            let err = ssrf_check_endpoint("corp.example.com", &[addr], allowed, &prefixes)
                .expect_err(
                    "nonzero-suffix declared NAT64 metadata must be rejected under an allowlist",
                );
            assert!(
                err.contains("cloud metadata") || err.contains("credential"),
                "must be operator-visible as metadata; got: {err}"
            );
        }
    }

    /// The RFC 8215 local-use prefix (`64:ff9b:1::/48`) is itself outside the
    /// global IPv6 unicast allocation, so an address inside it is rejected as
    /// non-global even when the declared prefix's embedded IPv4 is public.
    /// Declared-prefix "embeds a public IPv4 => passes" behavior is pinned by
    /// [`ssrf_check_endpoint_declared_nat64_public_and_carveout`] on genuine
    /// global unicast prefixes.
    #[test]
    fn ssrf_check_endpoint_rejects_declared_nat64_nonzero_suffix_local_use_prefix() {
        let prefixes = [zeroclaw_infra::net_guard::Nat64Prefix::parse("64:ff9b:1::/48").unwrap()];
        // 64:ff9b:1:808:0:808:0:1 embeds 8.8.8.8 (public) under the declared
        // local-use prefix, yet is rejected because the prefix range itself is
        // non-global IPv6.
        let addr = std::net::SocketAddr::new("64:ff9b:1:808:0:808:0:1".parse().unwrap(), 443);
        ssrf_check_endpoint("public.example.com", &[addr], &[], &prefixes)
            .expect_err("RFC 8215 local-use prefix must be rejected as non-global IPv6");
    }

    /// Production dispatch boundary: the declared-prefix resolver threaded
    /// through the tool must reach `validate_endpoint_host`, so the real entry
    /// point (not just the helper) rejects a DNS64 answer under a declared
    /// prefix without any opt-in.
    #[tokio::test]
    async fn validate_endpoint_host_rejects_declared_nat64_non_global_target() {
        let tmp = tempfile::tempdir().unwrap();
        let config = FileDownloadConfig {
            url: Some("http://internal.example.com/x".into()),
            ..FileDownloadConfig::default()
        };
        // Declared NAT64 prefixes come from the canonical `security.nat64_prefixes`
        // fact (not a `file_download` config field); the policy snapshot below
        // feeds them to the dispatch.
        let declared_prefixes = vec!["64:ff9b:1::/48".into()];
        // The injected resolver answers with a DNS64-synthesized address under
        // the declared 64:ff9b:1::/48 prefix embedding 10.0.0.1.
        let endpoint_resolver: EndpointResolver = Arc::new(
            move |_host: String,
                  port: u16|
                  -> Pin<Box<dyn Future<Output = ResolveResult> + Send>> {
                Box::pin(async move {
                    Ok(vec![std::net::SocketAddr::new(
                        "64:ff9b:1:a00:0:100::".parse().unwrap(),
                        port,
                    )])
                })
            },
        );
        let policy = FileDownloadSsrfPolicy {
            allowed_private_hosts: Vec::new(),
            nat64_prefixes: declared_prefixes,
        };
        let tool = FileDownloadTool::new_with_endpoint_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
            true,
            move || policy.clone(),
            endpoint_resolver,
        );
        let err = tool
            .validate_endpoint_host("http://internal.example.com/x")
            .await
            .unwrap_err();
        let msg = err.to_lowercase();
        assert!(
            msg.contains("private")
                || msg.contains("non-global")
                || msg.contains("loopback")
                || msg.contains("link-local"),
            "dispatch must reject a declared-prefix NAT64 target; got: {err}"
        );
    }

    /// Fail-closed contract for `security.nat64_prefixes`: a malformed entry must
    /// reject the dispatch with a field-specific configuration error instead of
    /// silently dropping the whole declared list. An empty list treats every
    /// network-specific prefix as undeclared ordinary address space, which would
    /// remove the declared-prefix SSRF policy (a fail-open). Reverting
    /// `normalize_nat64_prefixes` to a `filter_map` empty fallback fails this
    /// test: the mixed valid/malformed configuration would then surface a
    /// resolver/SSRF error instead of the field-specific configuration error.
    ///
    /// The policy normalization must also run BEFORE any resolver I/O, so the
    /// injected counting resolver is asserted untouched: invalid configuration
    /// must never reach `tokio::net::lookup_host`, which would leak the
    /// configured hostname into the resolver/search-list.
    #[tokio::test]
    async fn validate_endpoint_host_rejects_malformed_nat64_prefixes_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config = FileDownloadConfig {
            url: Some("http://internal.example.com/x".into()),
            ..FileDownloadConfig::default()
        };
        // The declared NAT64 prefixes are the canonical `security.nat64_prefixes`
        // fact; the mixed valid/malformed list must fail closed at the dispatch
        // boundary with a field-specific error.
        let declared_prefixes = vec!["64:ff9b:1::/48".into(), "not-a-cidr".into()];
        let resolver_calls = Arc::new(AtomicUsize::new(0));
        let tool = tool_with_counting_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
            declared_prefixes,
            Arc::clone(&resolver_calls),
        );
        let err = tool
            .validate_endpoint_host("http://internal.example.com/x")
            .await
            .unwrap_err();
        assert!(
            err.contains("nat64_prefixes") && err.contains("not-a-cidr"),
            "dispatch must reject the malformed nat64_prefixes entry with a field-specific error; got: {err}"
        );
        assert_eq!(
            resolver_calls.load(Ordering::SeqCst),
            0,
            "a malformed nat64_prefixes config must fail closed before any resolver I/O"
        );
    }

    /// Equivalent same-length aliases (`2606:4700:4700::/48` vs
    /// `2606:4700:4700::1/48`) cannot make an overlapping declaration look
    /// disjoint: the shared parser rejects the non-canonical host bit
    /// (`...::1/48` sets bits beyond /48) and fails the whole list closed at
    /// the real dispatch boundary before any DNS I/O, in BOTH declaration
    /// orders. Overlapping broad/specific prefixes are instead handled
    /// order-independently inside `net_guard`'s validator (each declared
    /// prefix is decoded and any denying translation rejects the address), so
    /// they no longer fail closed here.
    #[tokio::test]
    async fn validate_endpoint_host_rejects_equivalent_nat64_prefix_aliases() {
        let tmp = tempfile::tempdir().unwrap();
        for declared_prefixes in [
            vec!["2606:4700:4700::/48".into(), "2606:4700:4700::1/48".into()],
            vec!["2606:4700:4700::1/48".into(), "2606:4700:4700::/48".into()],
        ] {
            let config = FileDownloadConfig {
                url: Some("http://internal.example.com/x".into()),
                ..FileDownloadConfig::default()
            };
            let resolver_calls = Arc::new(AtomicUsize::new(0));
            let tool = tool_with_counting_resolver(
                test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
                config,
                declared_prefixes,
                Arc::clone(&resolver_calls),
            );
            let err = tool
                .validate_endpoint_host("http://internal.example.com/x")
                .await
                .unwrap_err();
            // See the malformed-config regression: the non-canonical alias is
            // rejected through the same `tool-file-download-error-invalid-nat64-prefix`
            // i18n key, which renders the field-specific message.
            assert!(
                err.contains("nat64_prefixes") && err.contains("2606:4700:4700::1/48"),
                "dispatch must fail closed on a non-canonical alias; got: {err}"
            );
            assert_eq!(
                resolver_calls.load(Ordering::SeqCst),
                0,
                "non-canonical aliases must fail closed before any resolver I/O"
            );
        }
    }

    /// Wire-up contract for the resolve_to_addrs binding the production
    /// code applies: a hostname whose real-DNS resolution would land on
    /// the wiremock must NOT reach the wiremock when the override IP
    /// points elsewhere. Detects regressions that drop or miskey the
    /// `resolve_to_addrs(host, addrs)` call in `build_secure_download_client`.
    ///
    /// reqwest's `resolve_to_addrs(host, addrs)` overrides only the IP;
    /// the port always comes from the URL (reqwest 0.12 client-builder
    /// docs). So the binding's IP half is what this test pins:
    ///
    /// - resolve_to_addrs wired to a bogus IP: reqwest connects to the
    ///   bogus IP + URL port → ECONNREFUSED → mock NOT hit.
    /// - regression drops resolve_to_addrs: reqwest does real DNS for
    ///   `localhost` (via /etc/hosts → 127.0.0.1) + URL port → mock
    ///   hit → `expect(0)` violated → test fails.
    ///
    /// `localhost` is used because it is covered by the test
    /// environment's `no_proxy`, so reqwest does not divert the
    /// request through the HTTP proxy (the proxy intercepts and
    /// returns 302 for unknown hostnames, masking the wire-up).
    #[tokio::test]
    async fn resolve_to_addrs_binds_resolved_addrs_not_real_dns() {
        let server = MockServer::start().await;
        let mock_port = server.address().port();

        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hit".to_vec()))
            .expect(0) // MUST not be hit when the override IP is bogus
            .mount(&server)
            .await;

        // Override pins "localhost" to a bogus IP (RFC 5737 documentation
        // range, unrouted). The URL port comes from MOCK_PORT via
        // reqwest's documented behavior; real DNS would point at
        // 127.0.0.1 (which IS the wiremock), so without the override the
        // request would hit the mock.
        let bogus_addrs = [std::net::SocketAddr::from(([192, 0, 2, 1], mock_port))];

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs("localhost", &bogus_addrs)
            .build()
            .expect("client build must succeed");

        let url = format!("http://localhost:{mock_port}/probe");
        let result = client.get(&url).send().await;

        // Drop the result without inspecting it — the wiremock side is
        // the authoritative regression-detector: zero hits means
        // resolve_to_addrs was honored, one hit means it was dropped
        // and reqwest fell through to real DNS.
        let _ = result;
        let received = server.received_requests().await.expect("infallible");
        assert!(
            received.is_empty(),
            "wiremock must NOT be hit when resolve_to_addrs binds localhost to a bogus IP; \
             if it is, resolve_to_addrs was dropped or miskeyed; saw {} request(s)",
            received.len()
        );
    }

    /// Prove that `build_secure_download_client` binds the exact transport_host
    /// via `resolve_to_addrs`. A request to the transport_host must use the
    /// bound IP, not real DNS. This pins the SSRF gate's address-binding fix.
    ///
    /// `localhost` (rather than an unresolvable synthetic hostname) is
    /// deliberate: `localhost` resolves via /etc/hosts to `127.0.0.1`, the
    /// wiremock address, so removing `resolve_to_addrs` would make reqwest
    /// connect to the wiremock through real DNS and this test would turn red.
    /// An unresolvable transport host would keep the test green even without
    /// the binding, because real DNS fails before any request reaches the mock.
    #[tokio::test]
    async fn build_secure_download_client_binds_transport_host() {
        let server = MockServer::start().await;
        let mock_port = server.address().port();

        // Mock expects 0 hits — if resolve_to_addrs binds correctly,
        // the request goes to the bound IP (192.0.2.1), not to the mock server
        // (which is 127.0.0.1:mock_port, where localhost's real DNS would land).
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(500).set_body_bytes(b"should-not-hit".to_vec()))
            .expect(0)
            .mount(&server)
            .await;

        // Bind localhost to a bogus IP (RFC 5737 documentation range).
        // The transport_host is used as-is for reqwest's resolve_to_addrs key.
        let transport_host = "localhost";
        let bound_addrs = [std::net::SocketAddr::from(([192, 0, 2, 1], mock_port))];

        let client = build_secure_download_client(transport_host, &bound_addrs, 30)
            .await
            .expect("client build must succeed");

        // Request using the transport_host — should fail to connect because
        // the bound IP is bogus (192.0.2.1 is not routable).
        let url = format!("http://{transport_host}:{mock_port}/probe");
        let result = client.get(&url).send().await;

        // We expect a connection error, not a successful response.
        // The wiremock side (expect(0)) is the authoritative regression detector.
        let _ = result;
        let received = server.received_requests().await.expect("infallible");
        assert!(
            received.is_empty(),
            "wiremock must NOT be hit when resolve_to_addrs binds localhost to a bogus IP; \
             if it is, resolve_to_addrs was dropped or miskeyed; saw {} request(s)",
            received.len()
        );
    }

    /// Prove that `build_secure_download_client` keys `resolve_to_addrs` by
    /// the exact dotted transport host. A request to `files.corp.invalid.`
    /// must reach the bound address set (the wiremock) — if the terminal dot
    /// were stripped before the override, the request host would not match
    /// the override key and real DNS would NXDOMAIN, leaving the mock unhit.
    ///
    /// This is the production-client half of the policy/transport hostname
    /// split: `parse_endpoint_url` returns a dotted `transport_host` for
    /// reqwest binding while `policy_host` strips the dot for allowlist
    /// comparison. `.invalid` is a reserved TLD (RFC 2606), so real DNS can
    /// never resolve the host — only the `resolve_to_addrs` override can.
    #[tokio::test]
    async fn build_secure_download_client_binds_dotted_transport_host() {
        let server = MockServer::start().await;

        // Mock expects exactly 1 hit — if resolve_to_addrs binds the dotted
        // host to the mock's real address, the request lands here.
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hit".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        // Bind the dotted transport host to the mock's real address. The
        // bound set is the connection authority; the port comes from the URL.
        let transport_host = "files.corp.invalid.";
        let bound_addrs = [*server.address()];

        let client = build_secure_download_client(transport_host, &bound_addrs, 30)
            .await
            .expect("client build must succeed");

        let url = format!("http://{transport_host}:{}/probe", server.address().port());
        let result = client.get(&url).send().await;
        assert!(
            result.is_ok(),
            "dotted transport host must connect through the resolve_to_addrs override"
        );

        // The wiremock's expect(1) is the authoritative detector: if the
        // terminal dot were stripped from the override key, the request host
        // would NXDOMAIN in real DNS and the mock would see zero requests.
    }

    /// DNS resolution must defer until after the can_act() check.
    /// Read-only mode must surface as a read-only error, NOT as a
    /// private-host error (which would prove the SSRF gate ran first).
    #[tokio::test]
    async fn execute_defers_dns_until_after_readonly_check() {
        let tmp = TempDir::new().unwrap();
        let resolver_calls = Arc::new(AtomicUsize::new(0));
        // A hostname URL (not an IP literal) so the resolver would genuinely
        // run if the ordering regressed — the counting seam below then proves
        // it did not.
        let tool = tool_with_counting_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::ReadOnly),
            cfg(Some("http://files.corp.test:1/x".into())),
            Vec::new(),
            Arc::clone(&resolver_calls),
        );

        let result = tool
            .execute(json!({ "document_id": "doc-1", "dest_path": "out.bin" }))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.unwrap().to_lowercase();
        assert!(
            err.contains("read-only") || err.contains("readonly"),
            "read-only check must fire before DNS; got: {err}"
        );
        // Must NOT be a private/loopback error — that would mean DNS ran first.
        assert!(
            !err.contains("private") && !err.contains("loopback"),
            "DNS check must come AFTER the read-only check; got: {err}"
        );
        assert_eq!(
            resolver_calls.load(Ordering::SeqCst),
            0,
            "a read-only rejection must perform zero resolver I/O"
        );
        assert!(!tmp.path().join("out.bin").exists());
    }

    /// DNS resolution must defer until after required-arg validation.
    /// A missing `dest_path` must surface as a missing-arg error, NOT as a
    /// private-host error.
    #[tokio::test]
    async fn execute_defers_dns_until_after_missing_arg_check() {
        let tmp = TempDir::new().unwrap();
        let resolver_calls = Arc::new(AtomicUsize::new(0));
        let tool = tool_with_counting_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://files.corp.test:1/x".into())),
            Vec::new(),
            Arc::clone(&resolver_calls),
        );

        // The missing-arg path bubbles up as `anyhow::Err` with a
        // localized message — `execute()` returns `Err`, not `Ok` with
        // `error` set. That's the established shape (see
        // `execute_errors_on_missing_arguments`).
        let err = tool
            .execute(json!({ "document_id": "doc-1" }))
            .await
            .expect_err("missing dest_path must surface as Err");

        // Use the Display chain (`: #`) so the source text bubbles up.
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("dest_path"),
            "missing-arg check must fire before DNS; got: {msg}"
        );
        assert!(
            !msg.contains("private") && !msg.contains("loopback"),
            "DNS check must come AFTER arg validation; got: {msg}"
        );
        assert_eq!(
            resolver_calls.load(Ordering::SeqCst),
            0,
            "a missing-arg rejection must perform zero resolver I/O"
        );
    }

    /// DNS resolution must defer until after destination validation. A
    /// dest_path that has no concrete file name must surface as a
    /// destination error, NOT as a private-host error.
    #[tokio::test]
    async fn execute_defers_dns_until_after_destination_check() {
        let tmp = TempDir::new().unwrap();
        let resolver_calls = Arc::new(AtomicUsize::new(0));
        let tool = tool_with_counting_resolver(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("http://files.corp.test:1/x".into())),
            Vec::new(),
            Arc::clone(&resolver_calls),
        );

        // `nested/..` terminates in `..` → "no concrete file name"
        // (see `execute_rejects_traversal_dest_path`).
        let result = tool
            .execute(json!({
                "document_id": "doc-1",
                "dest_path": "nested/.."
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.unwrap().to_lowercase();
        assert!(
            err.contains("file name") || err.contains("concrete") || err.contains("invalid"),
            "destination check must fire before DNS; got: {err}"
        );
        assert!(
            !err.contains("private") && !err.contains("loopback"),
            "DNS check must come AFTER destination validation; got: {err}"
        );
        assert_eq!(
            resolver_calls.load(Ordering::SeqCst),
            0,
            "a destination rejection must perform zero resolver I/O"
        );
    }

    /// HTTP-proxy bypass regression: with a runtime HTTP proxy configured,
    /// the SSRF client must still connect to the validated address set
    /// directly. Routing through the proxy would let it re-resolve the target
    /// hostname independently of `resolve_to_addrs`, making the proxy the
    /// connection authority instead of the SSRF gate.
    #[tokio::test]
    async fn build_secure_download_client_ignores_runtime_http_proxy() {
        let proxy = MockServer::start().await;
        let proxy_port = proxy.address().port();
        let target = MockServer::start().await;
        let target_port = target.address().port();

        // The target must be reached directly — the client connects to the
        // validated address (127.0.0.1:target_port), not through the proxy.
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hit".to_vec()))
            .expect(1)
            .mount(&target)
            .await;

        // The proxy must see zero requests: a routed-through client would hit
        // the proxy wiremock with an absolute-form GET.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(502))
            .expect(0)
            .mount(&proxy)
            .await;

        let _guard = RuntimeProxyGuard::install(ProxyConfig {
            enabled: true,
            http_proxy: Some(format!("http://127.0.0.1:{proxy_port}")),
            scope: ProxyScope::Zeroclaw,
            ..Default::default()
        })
        .await;

        // Bind localhost to the target's real address — the validated set.
        // If the client routed through the proxy instead, the proxy would
        // resolve `localhost` itself and the target would not be hit.
        let bound_addrs = [std::net::SocketAddr::from(([127, 0, 0, 1], target_port))];
        let client = build_secure_download_client("localhost", &bound_addrs, 30)
            .await
            .expect("client build must succeed");

        let url = format!("http://localhost:{target_port}/probe");
        let result = client.get(&url).send().await;
        assert!(
            result.is_ok(),
            "request must reach the target directly: {result:?}"
        );
        assert_eq!(result.unwrap().status().as_u16(), 200);

        let proxy_received = proxy.received_requests().await.expect("infallible");
        assert!(
            proxy_received.is_empty(),
            "the SSRF client must NOT route through the runtime HTTP proxy; \
             the proxy would re-resolve the target hostname and bypass the gate; \
             saw {} request(s)",
            proxy_received.len()
        );
    }

    /// HTTPS-CONNECT bypass regression: with a runtime HTTPS proxy configured,
    /// the SSRF client must not tunnel through the proxy via CONNECT. A
    /// proxy-side CONNECT re-resolves the target hostname independently of
    /// `resolve_to_addrs`, so any CONNECT observed proves the gate was bypassed.
    #[tokio::test]
    async fn build_secure_download_client_ignores_runtime_https_proxy_connect() {
        let proxy = MockServer::start().await;
        let proxy_port = proxy.address().port();
        let target = MockServer::start().await;
        let target_port = target.address().port();

        // The proxy must see zero CONNECT requests.
        Mock::given(method("CONNECT"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&proxy)
            .await;

        let _guard = RuntimeProxyGuard::install(ProxyConfig {
            enabled: true,
            https_proxy: Some(format!("http://127.0.0.1:{proxy_port}")),
            scope: ProxyScope::Zeroclaw,
            ..Default::default()
        })
        .await;

        // Bind localhost to the target's address; the https URL port comes
        // from the URL (reqwest 0.12 contract).
        let bound_addrs = [std::net::SocketAddr::from(([127, 0, 0, 1], target_port))];
        let client = build_secure_download_client("localhost", &bound_addrs, 30)
            .await
            .expect("client build must succeed");

        let url = format!("https://localhost:{target_port}/probe");
        // TLS to a plain wiremock fails; the regression signal is that the
        // proxy wiremock must not observe a CONNECT either way.
        let _ = client.get(&url).send().await;

        let proxy_received = proxy.received_requests().await.expect("infallible");
        assert!(
            proxy_received.is_empty(),
            "the SSRF client must NOT tunnel through the runtime HTTPS proxy via CONNECT; \
             the proxy would re-resolve the target hostname and bypass the gate; \
             saw {} request(s)",
            proxy_received.len()
        );
    }

    /// Fail-closed allowlist contract: a single malformed entry in
    /// `allowed_private_hosts` collapses the whole normalized list to empty
    /// (the `normalize_allowed_private_hosts` error fallback), so an endpoint
    /// that a wildcard would otherwise admit is rejected. This is the pairing
    /// control for `validate_endpoint_host_wildcard_lifts_literal_private_host_block`.
    #[tokio::test]
    async fn validate_endpoint_host_fails_closed_when_allowlist_has_malformed_entry() {
        let tmp = TempDir::new().unwrap();
        let tool = FileDownloadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            FileDownloadConfig {
                url: Some("http://127.0.0.1:1/x".into()),
                allowed_private_hosts: vec!["*".into(), "bad entry with space".into()],
                ..FileDownloadConfig::default()
            },
        );

        let err = tool
            .validate_endpoint_host("http://127.0.0.1:1/x")
            .await
            .unwrap_err()
            .to_lowercase();
        assert!(
            err.contains("private")
                || err.contains("non-global")
                || err.contains("loopback")
                || err.contains("link-local"),
            "the malformed allowlist entry must fail closed and reject the loopback endpoint; \
             got: {err}"
        );
    }
}
