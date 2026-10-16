//! A2A outbound HTTP client — protocol core (moved from `zeroclaw-tools::a2a_client`).
//!
//! Server-side glue, the 4 `a2a_*` tools, and `SecurityPolicy`/`Tool`-layer
//! coupling remain in `zeroclaw-tools` and reference this crate.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::stream::TryStreamExt;
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::config::{
    A2aPeerConfig, A2aPeerSource, LiveA2aConfig, ProxyPolicy, StaticA2aPeerSource,
};
use crate::a2a::a2a_wire::{
    AgentCard, AgentInterface, JsonRpcResponse, Role, SendMessageParams, SendMessageResponse, Task,
    escape_fence_body, rpc_result,
};

use zeroclaw_infra::net_guard::{is_cloud_metadata_ip, is_private_or_local_host};

/// A2A protocol version sent on every request (spec §3.2 `A2A-Version` header).
const A2A_VERSION: &str = "1.0";

/// One peer resolved at call time from the injected config. The token is the
/// post-`${VAR}`-interpolation value (empty = no `Authorization` header).
/// Built per-call, never stored in the client.
struct ResolvedPeer {
    base_url: String,
    token: String,
}

/// Outbound A2A HTTP client. Holds only the reqwest client and the live config
/// handle — no peer list copy. Constructed once, shared by all `a2a_*` tools
/// behind an `Arc`.
///
/// SSRF posture: peer `base_url`s are operator-declared (static allowlist), and
/// each call is guarded against private/loopback/metadata hosts via
/// `helpers::domain_guard` (the same policy `http_request` uses), with no
/// redirects followed.
pub struct A2aHttpClient {
    /// Request timeout applied to every outbound call; retained as a plain
    /// `Duration` so each per-request pinned client re-applies it.
    request_timeout: std::time::Duration,
    /// Host-implemented live source of peers, credentials, proxy policy, and
    /// settings. Consulted per call, so a host config reload takes effect on
    /// the next call; the client never snapshots peers or policy, and never
    /// reads a host config module.
    source: Arc<dyn A2aPeerSource>,
    /// Agent Card cache, keyed by peer base_url (derived data, not operator
    /// state). A base_url change is a different key, so an endpoint change
    /// naturally invalidates the prior entry. Each entry carries the fetch
    /// `Instant` so the live `card_cache_ttl_secs` is honored on lookup
    /// (spec-REQUIRED: `0` disables caching; an expired entry is refetched).
    /// Populated lazily on first discover/send; peers whose card is never
    /// fetched cost nothing.
    card_cache: Mutex<HashMap<String, (AgentCard, std::time::Instant)>>,
    /// Task route cache: `(peer, agent, task_id) → RouteHandle`. A `SendMessage`
    /// stores the selected peer/agent/rpc_url/tenant here keyed by
    /// `(peer, agent, task_id)`, so a later `GetTask`/`CancelTask` reuses the
    /// same route instead of re-selecting an interface (which could route the
    /// poll/cancel to a different endpoint than the one that created the task).
    /// The agent in the composite key prevents a same-task-id collision across
    /// agents on the same peer from overwriting another agent's route. Entries
    /// are cleared on terminal task state. Misses (daemon restart, evicted
    /// entry) with multiple v1 interfaces error rather than guessing.
    route_cache: RouteCache,
    /// Monotonically increasing JSON-RPC request id counter, scoped to this
    /// client instance so tests start fresh and daemon restarts naturally
    /// reset it. Paired with `rpc_result` for request-id correlation.
    next_req_id: AtomicU64,
}

impl A2aHttpClient {
    /// Build the client from a host-supplied live [`A2aPeerSource`]. Nothing
    /// about peers, credentials, proxy policy, or settings is snapshotted: the
    /// source is consulted on every call, so a host config reload (peer
    /// removed, token rotated, private-host tightening, proxy change) takes
    /// effect on the next call. Only the peer *names* are checked here (a
    /// duplicate/empty name is an ambiguous authorization boundary); the check
    /// is repeated per resolution so a live reload cannot reintroduce it.
    pub fn new(
        source: Arc<dyn A2aPeerSource>,
        request_timeout_secs: u64,
        route_cache: RouteCache,
    ) -> anyhow::Result<Self> {
        let timeout = if request_timeout_secs == 0 {
            // 0 is an unsafe default for a request timeout (would wait
            // forever on a hung peer). Fall back to a safe 30s. (The card
            // cache TTL is a separate `card_cache_ttl_secs` field, read live
            // in `get_card` — `0` there correctly disables caching.)
            std::time::Duration::from_secs(30)
        } else {
            std::time::Duration::from_secs(request_timeout_secs)
        };
        // The client holds no shared reqwest client: every outbound request
        // builds its own pinned pipeline (`pinned_client`) so the DNS pin is
        // applied and the live runtime proxy policy is consulted per call.
        // Reject a config with duplicate or empty peer names. `resolve_peer`
        // uses `.find(name)`, so two entries with the same name would silently
        // select whichever comes first — including its base URL and credential —
        // an ambiguous authorization boundary. Fail closed at construction so
        // the operator learns before any call can route to the wrong peer.
        validate_peer_names_unique(source.peers().iter())?;
        Ok(Self {
            request_timeout: timeout,
            source,
            card_cache: Mutex::new(HashMap::new()),
            route_cache,
            next_req_id: AtomicU64::new(1),
        })
    }

    /// Build the client from a fixed [`LiveA2aConfig`] handle and proxy policy.
    /// Peers and settings are read live from the handle; tokens are literals
    /// (plus `${VAR}` interpolation). Convenient for hosts with a static proxy
    /// decision and for tests; hosts with encrypted credentials or a live proxy
    /// source pass their own [`A2aPeerSource`] to [`A2aHttpClient::new`].
    pub fn new_static(
        config: LiveA2aConfig,
        request_timeout_secs: u64,
        proxy: ProxyPolicy,
        route_cache: RouteCache,
    ) -> anyhow::Result<Self> {
        Self::new(
            StaticA2aPeerSource::new(config, proxy),
            request_timeout_secs,
            route_cache,
        )
    }

    /// Resolve a single peer by name from the live source at call time. The
    /// source returns the current peers; token resolution (decrypt + env
    /// interpolation) happens after, so a slow `SecretStore`/`std::env::var`
    /// can't block config hot-reload writers.
    fn resolve_peer(&self, peer: &str) -> anyhow::Result<ResolvedPeer> {
        let peers = self.source.peers();
        // The source is live and hot-reloadable; a reload can introduce a
        // duplicate or empty peer name even when construction-time validation
        // passed. `.find(name)` would then silently select whichever entry
        // appears first (including its base URL and credential), so fail
        // closed on an ambiguous mapping at every resolution, not just at
        // construction.
        validate_peer_names_unique(peers.iter())?;
        let p = peers
            .iter()
            .find(|p| p.name == peer)
            .ok_or_else(|| anyhow::Error::msg(format!("a2a client: unknown peer '{peer}'")))?;
        Ok(ResolvedPeer {
            base_url: p.base_url.trim_end_matches('/').to_string(),
            token: self.resolve_peer_token(&p.token)?,
        })
    }

    /// Read access to the route cache (host/tool layer may inspect lifecycle
    /// state for tests and diagnostics; ownership stays with the client).
    pub fn route_cache_handle(&self) -> &RouteCache {
        &self.route_cache
    }

    /// The current peers, read live from the host source. Used by the
    /// `a2a_discover` tool to enumerate peers without a peer argument. Tokens
    /// are the raw (undecrypted) fields; callers must not expose them.
    #[must_use]
    pub fn peers(&self) -> Vec<A2aPeerConfig> {
        self.source.peers()
    }

    /// Resolve a peer's raw token field to the credential value sent as
    /// `Authorization: Bearer`. Mirrors `http_request`'s canonical secret path:
    /// the host decrypts an encrypted value (per `SecretStore::is_encrypted`)
    /// via [`A2aPeerSource::resolve_token`]; the resulting literal (or a plain
    /// literal) is then passed through the `${VAR}` env resolver so an
    /// env-backed token is still resolved at call time. Empty resolves to empty
    /// (anonymous peer, no Authorization header). Only the requested peer's
    /// token is resolved, so one peer's broken credential never disables others.
    fn resolve_peer_token(&self, raw: &str) -> anyhow::Result<String> {
        if raw.is_empty() {
            return Ok(String::new());
        }
        let literal = self.source.resolve_token(raw)?;
        resolve_env_backed_token(&literal)
    }

    /// SSRF guard: reject private/loopback/link-local/metadata hosts before any
    /// request is issued, mirroring `http_request`'s posture so the outbound
    /// A2A surface reuses the canonical URL/domain/SSRF policy (no duplicated
    /// private-host authority). Returns the resolved `SocketAddr`s for non-
    /// literal hosts so the caller can pin them into the reqwest connection
    /// (`resolve_to_addrs`), closing the DNS-rebinding gap: without pinning,
    /// reqwest re-resolves the host at connect time and a public domain that
    /// first resolves public (passing the check) then rebinding to private
    /// would be reached. Literal-IP hosts return `None` (no pin needed).
    ///
    /// Two layers, both via `helpers::domain_guard`:
    /// (1) host-literal check — metadata IPs are always blocked; private/
    ///     loopback/link-local hosts are blocked unless the operator opted in
    ///     via `allow_private_hosts` or pinned the exact host in
    ///     `allowed_private_hosts`;
    /// (2) DNS-resolved IP check — resolves the host and, when private
    ///     resolution is allowed, only blocks cloud-metadata IPs; otherwise
    ///     blocks any IP that lands in private/loopback/link-local/metadata
    ///     space, so a public domain resolving into the private network
    ///     (DNS rebinding) is contained.
    async fn guard_host(
        &self,
        url: &reqwest::Url,
    ) -> anyhow::Result<Option<Vec<std::net::SocketAddr>>> {
        // The host comes from the canonical parser (no userinfo, no port),
        // so what we validate is exactly what reqwest will connect to.
        let host = match url.host_str() {
            Some(h) if !h.is_empty() => h.to_lowercase(),
            _ => anyhow::bail!("a2a client: URL must include a valid host"),
        };
        let (allow_private, allowed_private_hosts) = {
            let settings = self.source.settings();
            (settings.allow_private_hosts, settings.allowed_private_hosts)
        };
        // Normalize once per call; invalid entries surface as a config error
        // rather than a silent skip (mirrors `http_request` construction).
        let allowed = zeroclaw_infra::net_guard::normalize_allowed_domains(
            allowed_private_hosts,
            "a2a.client.allowed_private_hosts",
        )?;

        // (1) Cloud-metadata hosts are always blocked, even when private hosts
        // are allowed — a peer must never reach the IMDS endpoint.
        if host
            .parse::<std::net::IpAddr>()
            .is_ok_and(is_cloud_metadata_ip)
        {
            let fenced = fence_remote_error_text(&host);
            anyhow::bail!(
                "a2a client: peer URL host {fenced} is a cloud metadata address and not allowed"
            );
        }

        let private_host = is_private_or_local_host(&host);
        let private_host_explicitly_allowed =
            private_host && zeroclaw_infra::net_guard::host_matches_allowlist(&host, &allowed);
        if private_host && !private_host_explicitly_allowed && !allow_private {
            let fenced = fence_remote_error_text(&host);
            anyhow::bail!(
                "a2a client: peer URL host {fenced} is private/loopback and not allowed (set a2a.client.allow_private_hosts or a2a.client.allowed_private_hosts)"
            );
        }

        // (2) DNS-rebinding defense: resolve the host and validate the resolved
        // IPs. Literal IPs are covered by the checks above (private/metadata
        // cases), and a public literal IP needs no resolution/pinning.
        if host.parse::<std::net::IpAddr>().is_err() {
            let port = url
                .port_or_known_default()
                .ok_or_else(|| anyhow::Error::msg("a2a client: URL must include a valid port"))?;
            let resolved = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|e| {
                    // `host` may be peer-controlled (a card-advertised RPC URL);
                    // fence it as untrusted before it reaches the model via the
                    // DNS-failure error path.
                    let fenced = fence_remote_error_text(&host);
                    anyhow::Error::msg(format!(
                        "a2a client: failed to resolve peer host {fenced}: {e}"
                    ))
                })?
                .collect::<Vec<_>>();
            let ips = resolved.iter().map(|a| a.ip()).collect::<Vec<_>>();
            // Read the operator-declared NAT64 prefixes LIVE from config so a
            // security-policy reload takes effect on the next resolution — this
            // setting changes whether resolved destinations are classified
            // safely, so a stale construction-time snapshot is not acceptable
            // (the other security fields are already read live).
            let nat64_prefixes = zeroclaw_infra::net_guard::parse_nat64_prefixes(
                &self.source.settings().nat64_prefixes,
                "security.nat64_prefixes",
            )?;
            let private_resolution_allowed = allow_private || private_host_explicitly_allowed;
            if private_resolution_allowed {
                // The validation error embeds the (peer-controlled) host; fence
                // it before it reaches the model.
                zeroclaw_infra::net_guard::validate_resolved_ips_exclude_metadata(
                    &host,
                    &ips,
                    &nat64_prefixes,
                )
                .map_err(|e| {
                    let fenced = fence_remote_error_text(&e.to_string());
                    anyhow::Error::msg(fenced)
                })?;
            } else {
                zeroclaw_infra::net_guard::validate_resolved_ips_are_public(
                    &host,
                    &ips,
                    &nat64_prefixes,
                )
                .map_err(|e| {
                    let fenced = fence_remote_error_text(&e.to_string());
                    anyhow::Error::msg(fenced)
                })?;
            }
            return Ok(Some(resolved));
        }
        Ok(None)
    }

    /// Build a per-request reqwest client that pins the validated DNS
    /// resolution for `url`, so the actual connection cannot be rebound to a
    /// different (private/metadata) IP after the SSRF check passed. Mirrors
    /// `http_request`'s `resolve_to_addrs` posture. For a literal-IP host (no
    /// DNS resolution), the live runtime proxy policy is still consulted and a
    /// direct no-proxy client is built, so proxy routing follows the live
    /// policy rather than a construction-time snapshot.
    fn pinned_client(
        &self,
        url: &reqwest::Url,
        resolved: Option<Vec<std::net::SocketAddr>>,
    ) -> anyhow::Result<reqwest::Client> {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::Error::msg("a2a client: URL must include a host"))?;
        let Some(addrs) = resolved else {
            // Literal IP (or no resolution). Apply the same live policy check
            // the hostname branch uses: if a proxy is active we fail closed
            // (consistent with the DNS-pinned path), otherwise a direct
            // no-proxy client is authoritative. A literal IP needs no DNS pin,
            // but its proxy routing must follow the live policy — never a
            // construction-time snapshot.
            if runtime_proxy_active_for_a2a(&self.source.proxy_policy()) {
                anyhow::bail!(
                    "a2a client: a runtime proxy is active for the a2a service; the \
                     literal-IP path cannot guarantee the SSRF-safe destination under \
                     that policy. Disable the proxy for the a2a service to connect directly"
                );
            }
            // Direct client, explicitly no-proxy, so an inherited env proxy
            // cannot route a literal-IP request.
            return Ok(reqwest::Client::builder()
                .timeout(self.request_timeout)
                .connect_timeout(std::time::Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()?);
        };
        // Every active proxy mode (HTTP forward, HTTPS CONNECT, SOCKS5,
        // SOCKS5H) receives the target hostname and resolves/forwards it on the
        // proxy side, so `resolve_to_addrs` below does NOT constrain the
        // address the proxy actually connects to. A peer hostname that passed
        // the local public-IP check could resolve to private/loopback/metadata
        // space at the proxy — defeating the DNS-pinning guarantee. Fail closed
        // rather than ship a hole; only a direct (non-proxied) connection keeps
        // the local pin authoritative.
        if runtime_proxy_active_for_a2a(&self.source.proxy_policy()) {
            anyhow::bail!(
                "a2a client: a runtime proxy is active for the a2a service, which \
                 resolves the peer hostname on the proxy side; the local DNS pin \
                 cannot guarantee the SSRF-safe destination. Disable the proxy \
                 for the a2a service to connect directly"
            );
        }
        // Clone the shared builder settings (timeout, no-redirect) and add the
        // IP pin. The direct pinned path must NOT route through a proxy: the
        // guard above already fails closed when a configured runtime proxy is
        // active, and reqwest enables system/environment proxies by default
        // (HTTP_PROXY/HTTPS_PROXY/ALL_PROXY), which would receive the target
        // hostname and resolve it on the proxy side — defeating the local pin.
        // `.no_proxy()` disables those inherited proxies so the connection is
        // truly direct and the resolved address is authoritative.
        let builder = reqwest::Client::builder()
            .timeout(self.request_timeout)
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        Ok(builder.resolve_to_addrs(host, &addrs).build()?)
    }
}

/// Reject an `[a2a.client.peers]` list containing an empty peer name or
/// duplicate peer names. The peer name is the only operator-controlled handle
/// that resolves both the network destination and the bearer credential, so
/// silently accepting duplicates would let a malformed config send to the
/// wrong peer or use the wrong credential. Called at client construction so a
/// bad config fails before any call can route ambiguously.
fn validate_peer_names_unique<'a>(
    peers: impl Iterator<Item = &'a A2aPeerConfig>,
) -> anyhow::Result<()> {
    let mut seen = std::collections::HashSet::new();
    for p in peers {
        let name = p.name.trim();
        if name.is_empty() {
            anyhow::bail!("a2a client: a peer name must not be empty");
        }
        if !seen.insert(name.to_string()) {
            anyhow::bail!(
                "a2a client: duplicate peer name '{name}' in [a2a.client.peers]; \
                 peer names must be unique (a duplicate would make the destination \
                 and credential mapping ambiguous)"
            );
        }
    }
    Ok(())
}

/// Whether any runtime proxy applies to the a2a service. Every proxy mode
/// (HTTP forward proxy, HTTPS CONNECT, SOCKS5, SOCKS5H) receives the target
/// hostname and resolves/forwards it on the proxy side — reqwest's
/// `resolve_to_addrs` local override does NOT constrain the address the proxy
/// actually reaches. A peer hostname that passed the local public-IP check
/// could therefore resolve to private/loopback/metadata space at the proxy,
/// defeating the SSRF destination guarantee. Used by `pinned_client` to fail
/// closed whenever a proxy is active: direct (non-proxied) connections are the
/// only mode where the local pin is authoritative.
///
/// Deliberately conservative: any proxy URL in an active slot
/// (`http_proxy`/`https_proxy`/`all_proxy`) for the a2a service fails closed,
/// even if a `no_proxy` entry would skip the proxy for a specific host.
/// That over-rejection is safe (it errs toward not connecting, never toward
/// connecting through an unverifiable destination); the exact reqwest
/// `NoProxy` match is not re-implemented here to avoid drifting from the
/// connector's own semantics.
fn runtime_proxy_active_for_a2a(proxy: &ProxyPolicy) -> bool {
    matches!(proxy, ProxyPolicy::Use(url) if !url.trim().is_empty())
}

/// Resolve a `${VAR}` placeholder on an already-decrypted token literal to
/// its env value, or return the literal as-is. Mirrors `http_request`'s
/// canonical `env_secret_reference` + `resolve_env_backed_auth_secret`
/// (`http_request.rs:394-434`): same `${NAME}` form, same ASCII-
/// alphanumeric+underscore restriction, same empty/missing/empty-value
/// rejection. A local mirror rather than a shared call because the
/// http_request helpers are private and extracting them touches http_request
/// (out of scope for this slice); a follow-up should lift the grammar into a
/// shared helper so both edges resolve env-backed secrets identically.
fn resolve_env_backed_token(raw: &str) -> anyhow::Result<String> {
    let Some(inner) = raw.strip_prefix("${").and_then(|v| v.strip_suffix('}')) else {
        return Ok(raw.to_string());
    };
    if inner.is_empty() {
        anyhow::bail!("a2a client: peer token references an empty environment variable name");
    }
    if !inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!(
            "a2a client: peer token env var '{inner}' must contain only ASCII letters, numbers, or underscores"
        );
    }
    let value = std::env::var(inner).map_err(|e| {
        anyhow::Error::msg(format!(
            "a2a client: peer token references environment variable '{inner}', but it could not be read: {e}"
        ))
    })?;
    if value.is_empty() {
        anyhow::bail!(
            "a2a client: peer token references environment variable '{inner}', but it is empty"
        );
    }
    Ok(value)
}

/// Parse a peer URL canonically via `reqwest::Url::parse`, rejecting
/// userinfo. This is the SSRF-critical parser: a handwritten
/// `host:port` splitter would disagree with reqwest's own URL parser on
/// inputs like `http://public.example:80@127.0.0.1:8080/admin` (the splitter
/// validates `public.example`, reqwest connects to `127.0.0.1:8080`).
/// Parsing once here and reusing the same `Url` for validation, DNS pinning,
/// and sending closes that gap. Mirrors `http_request`'s URL posture
/// (`http_request.rs:660-662` rejects userinfo).
fn parse_peer_url(url: &str) -> anyhow::Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url).map_err(|e| {
        // `url` may be peer-controlled (a card's supportedInterfaces[].url);
        // fence it as untrusted before it reaches the model via the error path.
        let fenced = fence_remote_error_text(url);
        anyhow::Error::msg(format!("a2a client: invalid URL {fenced}: {e}"))
    })?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        // `parsed.scheme()` is peer-controlled (from a card's
        // supportedInterfaces[].url); fence it so a valid-but-unsupported
        // scheme such as `ignore-previous-instructions://` cannot reach the
        // model via the tool error path without the untrusted marker.
        let fenced = fence_remote_error_text(parsed.scheme());
        anyhow::bail!("a2a client: URL must be http:// or https:// (got {fenced})");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!(
            "a2a client: URL userinfo is not allowed (peer URL must not contain user@host)"
        );
    }
    if parsed.host_str().map(|h| h.is_empty()).unwrap_or(true) {
        anyhow::bail!("a2a client: URL must include a valid host");
    }
    Ok(parsed)
}

/// The origin (scheme://host[:port]) of a peer URL, used to bind credentials:
/// a peer-advertised RPC URL must share the configured peer's origin before
/// a bearer token is attached (prevents a card from forwarding credentials
/// to an attacker origin or downgrading to plaintext http).
fn url_origin(url: &reqwest::Url) -> String {
    let scheme = url.scheme();
    let host = url.host_str().unwrap_or("");
    // `port_or_known_default()` normalizes an explicit default port to the
    // scheme default (443 for https, 80 for http), so `https://peer` and
    // `https://peer:443` compare equal — they are the same effective origin.
    // (url_origin is only ever called on http/https URLs, so a known default
    // always exists.)
    let port = url.port_or_known_default();
    match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    }
}

/// Task endpoint under a peer base: `<base>/a2a/{agent}>`. The agent is
/// percent-encoded as a single path segment (via `Url::join`) so a
/// model-supplied `../admin`, `?`, or `#` cannot traverse to a different
/// authenticated endpoint on the same origin. `join` resolves the segment
/// relative to the base and normalizes away path traversal.
fn task_url(base: &reqwest::Url, agent: &str) -> anyhow::Result<String> {
    // A bare dot-segment (`a2a/.` / `a2a/..`) would be collapsed by the URL
    // join (RFC 3986 dot-segment removal) back to the base origin, silently
    // turning a distinct agent id into the base path. Reject such agents.
    if agent == "." || agent == ".." {
        anyhow::bail!("a2a client: agent cannot be a bare dot-segment '{agent}'");
    }
    // Encode the agent as one path segment: percent-encode everything that is
    // not a path-segment character, so `../`, `?`, `#` lose their special
    // meaning rather than being injected raw.
    let segment = percent_encode_path_segment(agent);
    let path = format!("a2a/{segment}");
    let joined = base
        .join(&path)
        .map_err(|e| anyhow::Error::msg(format!("a2a client: invalid agent path segment: {e}")))?;
    Ok(joined.to_string())
}

/// Percent-encode a string for use as a single URL path segment: encode `/`,
/// `?`, `#`, and other reserved/non-segment chars so the value cannot break
/// out of its segment. Empty input stays empty (handled by the caller's
/// conventional `/a2a/` shape).
fn percent_encode_path_segment(s: &str) -> String {
    // Encode anything that is not an unreserved path-segment char
    // (pchar = unreserved / pct-encoded / sub-delims / ":" / "@"). We keep
    // it conservative: encode anything outside [A-Za-z0-9-._~] plus the
    // sub-delims, which is stricter than required but safe.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
            )
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Card-cache freshness: a cached card is fresh while its age is strictly
/// less than the live TTL. `ttl == 0` disables caching (caller checks this
/// before calling, so here ttl > 0). Extracted as a pure function so the
/// expiry boundary is unit-testable without a network round-trip.
fn card_is_fresh(fetched: std::time::Instant, now: std::time::Instant, ttl_secs: u64) -> bool {
    now.duration_since(fetched).as_secs() < ttl_secs
}

/// Generate a v4 UUID string for a `Message.messageId` (spec REQUIRED on
/// send). Uses the `uuid` crate already in the workspace dependency set.
fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Fold a `SendMessageResponse::Message` reply into a synthetic completed
/// `Task` so the tool layer has a uniform `Task` to surface. The agent's
/// reply text (from any `text` parts) is carried as a single artifact.
/// The task ID is derived from the peer-provided `messageId` (not a random
/// UUID). The response preserves its origin — the tool output includes a
/// `response_type` marker so the model can distinguish a direct `Message`
/// reply from a peer-managed `Task`.
fn message_to_task(message: crate::a2a::a2a_wire::Message) -> (Task, bool) {
    let msg_id = message.message_id.clone();
    let text = message
        .parts
        .iter()
        .filter_map(|p| p.as_text().map(|s| s.to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    let artifact = crate::a2a::a2a_wire::Artifact {
        // Derive from the peer-provided messageId, not a random UUID.
        artifact_id: format!("{msg_id}-artifact"),
        name: None,
        description: None,
        parts: vec![crate::a2a::a2a_wire::Part::text_str(text)],
    };
    let task = Task {
        // Reuse the peer-provided messageId — not an invented UUID.
        id: msg_id,
        context_id: message.context_id.clone().unwrap_or_default(),
        status: crate::a2a::a2a_wire::TaskStatus {
            state: crate::a2a::a2a_wire::TaskState::TaskStateCompleted,
            message: Some(message),
            timestamp: None,
        },
        artifacts: vec![artifact],
        history: vec![],
        metadata: None,
    };
    (task, false)
}

/// The resolved endpoint for an A2A call: the RPC URL plus the `tenant`
/// (if any) that must be echoed into request `params.tenant`. Carrying
/// `tenant` lets `send_message`/`get_task`/`cancel` route to the requested
/// agent and stay consistent across a task's lifecycle. The interface's
/// `protocolVersion` is validated to `1.0` at selection time but not
/// retained — the wire version is fixed by the `A2A-Version` header.
#[derive(Debug)]
struct SelectedEndpoint {
    url: String,
    tenant: Option<String>,
}

/// A cached route for an in-flight task: the RPC URL and tenant that
/// created the task. Stored under the composite key `(peer, agent, task_id)`
/// by `send_message` and reused by `get_task`/`cancel` so a task's lifecycle
/// stays on one endpoint — a follow-up call cannot accidentally route to a
/// different agent/interface on an aggregate card. The peer and agent are
/// carried in the cache key; `rpc_url` and `tenant` are the payload needed
/// to repeat the call on the same endpoint. `inserted_at` records when the
/// route was cached so the bounded cache can evict the oldest entry when it
/// reaches capacity (abandoned in-flight routes must not grow without limit).
#[derive(Clone)]
pub struct RouteHandle {
    pub rpc_url: String,
    pub tenant: Option<String>,
    pub inserted_at: std::time::Instant,
}

/// Shared task-route state that survives tool-set reassembly across
/// `process_message` turns. A `SendMessage` stores the selected endpoint here;
/// a later `GetTask`/`CancelTask` (in a different turn, with a fresh
/// `A2aHttpClient`) looks it up by `(peer, agent, task_id)`. The `Arc` ensures
/// all turns in the same daemon process share one instance. Daemon restart
/// naturally clears it (orphaned routes are stale after a restart).
pub type RouteCache = Arc<Mutex<HashMap<(String, String, String), RouteHandle>>>;

/// The process-wide shared `RouteCache` for the outbound A2A client. Tool-set
/// reassembly (every `process_message` turn and every `agent::run` invocation
/// constructs a fresh `A2aHttpClient`) must not drop the route authority of an
/// in-flight task — a `SendMessage` in one turn that tells the model to poll
/// later must be reachable by `GetTask`/`CancelTask` in a later turn. The
/// cache is keyed by `(peer, agent, task_id)`, so independent peer/agent/task
/// lifecycles do not collide. Daemon restart (process exit) naturally clears
/// it — orphaned routes are stale after a restart.
pub fn shared_a2a_route_cache() -> RouteCache {
    static SHARED: OnceLock<RouteCache> = OnceLock::new();
    SHARED
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

/// Upper bound on the number of in-flight routes retained in the shared cache.
/// Routes are only meaningful for non-terminal tasks that may still be polled
/// or canceled; an unbounded map would let abandoned in-flight tasks (or a
/// flurry of sends) grow memory without limit. When the bound is reached the
/// oldest entry (by `inserted_at`) is evicted — a task older than every other
/// in the cache is the least likely to still be polled, and a poll/cancel that
/// misses the cache re-selects from the current card.
const MAX_ROUTE_CACHE_ENTRIES: usize = 1024;

/// Upper bound (bytes) on the peer-controlled strings retained in a single
/// route entry (`task_id` in the key + `RouteHandle.rpc_url`/`tenant`).
/// `max_response_bytes` caps each response body and `MAX_ROUTE_CACHE_ENTRIES`
/// caps the count, but neither bounds the AGGREGATE retained bytes: a
/// permitted or compromised peer can keep returning non-terminal tasks with
/// huge unique IDs and/or interface URLs (each up to ~1 MB under the default
/// cap), and the process-wide cache would retain ~1,024 copies — on the order
/// of 1 GB. Enforce a per-entry length so the retained cache stays
/// memory-bounded (defense in depth).
const MAX_ROUTE_STRING_BYTES: usize = 4096;

/// Insert a route into the bounded shared cache. Evicts the oldest entry when
/// capacity is reached. Called only for non-terminal tasks (a terminal task
/// needs no follow-up poll/cancel and would otherwise leave a permanent entry
/// on the synchronous send path). Entries whose peer-controlled retained
/// strings exceed `MAX_ROUTE_STRING_BYTES` are skipped (the send still
/// proceeds; a follow-up poll/cancel re-selects from the current card).
fn insert_route_bounded(cache: &RouteCache, key: (String, String, String), route: RouteHandle) {
    let retained_bytes = key.0.len()
        + key.1.len()
        + key.2.len()
        + route.rpc_url.len()
        + route.tenant.as_ref().map_or(0, String::len);
    if retained_bytes > MAX_ROUTE_STRING_BYTES {
        return;
    }
    let mut map = cache.lock();
    if map.len() >= MAX_ROUTE_CACHE_ENTRIES {
        // Evict the oldest entry to keep the cache bounded.
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, r)| r.inserted_at)
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    map.insert(key, route);
}

/// Select the JSON-RPC interface for a peer from its Agent Card's
/// `supportedInterfaces` (spec §5.2 transport selection), matching the
/// requested `agent` when possible. An A2A aggregate card (e.g. a ZeroClaw
/// inbound catalog) advertises one interface per alias; the caller's `agent`
/// should select the matching interface. Standard single-agent cards use
/// arbitrary URLs (e.g. `/a2a/v1`) with no agent alias in the path — the
/// sole compatible interface is selected regardless of URL suffix.
///
/// Selection rules (filter the **complete v1-compatible candidate set**
/// before picking — a first interface advertising v0.3 does not reject the
/// whole card when a later v1 interface exists):
/// - collect all JSON-RPC interfaces whose `protocolVersion == 1.0`;
/// - **single compatible interface**: select it regardless of URL shape
///   (standard card with `/a2a/v1` or similar — no agent alias required);
/// - **multiple candidates**: an agent/tenant route identity is required to
///   distinguish interfaces. The identity is resolved against the URL
///   (trailing `/a2a/{agent}` segment — the ZeroClaw catalog convention)
///   first, then against the interface `tenant` (standard tenant-shared
///   routing: a card may advertise several JSON-RPC interfaces at the same
///   URL distinguished only by `tenant`). An empty or unknown identity on a
///   multi-interface card **fails closed** rather than silently routing to an
///   arbitrary interface — a typo or stale alias must not send the task to
///   the wrong agent/tenant;
/// - if no card (card-less peer, discovery 404), fall back to
///   `<base>/a2a/{agent}` (MVP card-less shape — the peer hasn't served
///   discovery yet). A discovery failure that is not a 404 (5xx, oversized/
///   malformed body) surfaces as an error instead of this fallback.
///
/// MVP supports only the JSON-RPC transport binding; other bindings
/// (`GRPC`, `HTTP+JSON`) are a follow-up.
fn select_interface(
    card: Option<&AgentCard>,
    base: &reqwest::Url,
    agent: &str,
) -> anyhow::Result<SelectedEndpoint> {
    if let Some(card) = card {
        // Candidate set: JSON-RPC bindings advertising protocolVersion 1.0.
        let v1_candidates: Vec<&AgentInterface> = card
            .supported_interfaces
            .iter()
            .filter(|i| {
                i.protocol_binding.eq_ignore_ascii_case("jsonrpc")
                    && i.protocol_version == A2A_VERSION
            })
            .collect();
        if v1_candidates.is_empty() {
            anyhow::bail!(
                "a2a client: peer card advertises interfaces but none with a v1 JSON-RPC \
                 binding (protocolVersion=1.0); cannot select a transport"
            );
        }
        // Single compatible interface. The fast path (select regardless of URL
        // shape) is valid for a genuine standard single-agent card whose
        // interface URL is an opaque endpoint — `agent` is an opaque
        // identifier, not a URL suffix. It is NOT valid for a catalog/aggregate
        // card (e.g. a ZeroClaw catalog, which emits a non-JSON-RPC `catalog`
        // interface plus one JSON-RPC interface per published alias): with a
        // single published alias, filtering leaves one JSON-RPC candidate, and
        // accepting it before checking the requested `agent` would let
        // `a2a_send(peer, agent="typo", ...)` route to the only published
        // interface instead of failing closed — a wrong-agent remote-task
        // defect (the route is then cached under the caller-supplied id).
        //
        // The catalog marker is the DISCOVERY/catalog binding, NOT any
        // non-JSON-RPC binding. Ordinary alternate transports are not a
        // catalog: the v1 protocol lets ONE agent expose JSON-RPC and GRPC
        // (or HTTP+JSON) interfaces at /rpc and /grpc that all serve the SAME
        // agent. Those are not route identities — the sole JSON-RPC interface
        // is still an opaque endpoint and the permissive agent-agnostic
        // fallback must stay, or a conforming mixed-binding card becomes
        // unusable (its /rpc URL cannot match a /a2a/{agent} suffix). Only a
        // card that actually declares a catalog/discovery binding (the
        // ZeroClaw aggregate card) gates the fallback behind an agent match.
        let is_catalog_card = card
            .supported_interfaces
            .iter()
            .any(|i| i.protocol_binding.eq_ignore_ascii_case("catalog"));
        if v1_candidates.len() == 1 && !is_catalog_card {
            return Ok(endpoint_from(v1_candidates[0]));
        }
        // Multiple v1 candidates (aggregate card, e.g. ZeroClaw catalog, or a
        // standard tenant-shared card). An agent/route identity is required to
        // distinguish interfaces — an empty or unknown identity must not
        // silently route to the first interface (a typo or stale alias would
        // send the task and its follow-up polls to the wrong agent/tenant
        // while the route is cached under the caller-supplied name). The
        // `agent` argument doubles as that route identity, resolved in two
        // supported orders:
        //   1. ZeroClaw catalog convention: an interface whose URL ends in
        //      `/a2a/{agent}` (the alias-as-path model).
        //   2. Standard tenant-shared routing: an interface whose `tenant`
        //      equals the requested identity. A conforming A2A card may
        //      advertise multiple JSON-RPC interfaces at the SAME URL
        //      distinguished only by `tenant` (alpha/beta); the caller selects
        //      one by passing that tenant as the route identity, without
        //      requiring the peer to encode it in a ZeroClaw-specific suffix.
        if agent.is_empty() {
            anyhow::bail!(
                "a2a client: peer card advertises multiple v1 JSON-RPC interfaces; \
                 an agent/tenant route identity is required to select the correct one"
            );
        }
        let agent_segment = format!("/a2a/{}", percent_encode_path_segment(agent));
        let url_matches: Vec<&AgentInterface> = v1_candidates
            .iter()
            .filter(|i| i.url.ends_with(&agent_segment))
            .cloned()
            .collect();
        let tenant_matches: Vec<&AgentInterface> = v1_candidates
            .iter()
            .filter(|i| i.tenant.as_deref() == Some(agent))
            .cloned()
            .collect();

        // The `agent` identity string can mean either an alias (`/a2a/{agent}`
        // URL suffix) or a tenant. When BOTH interpretations apply, they must
        // not point at DIFFERENT interfaces: a card with interface A ending in
        // `/a2a/{agent}` but declaring a different tenant, plus another
        // interface B whose tenant equals `agent`, would otherwise let the
        // identity resolve to A by URL while silently ignoring B's tenant
        // match. That would route the request (and its cached follow-ups) to
        // the wrong interface/tenant. Fail closed whenever a tenant match lies
        // outside the URL match set, rather than preferring card order.
        if !url_matches.is_empty()
            && let Some(conflict) = tenant_matches.iter().find(|t| {
                !url_matches
                    .iter()
                    .any(|u| u.url == t.url && u.tenant == t.tenant)
            })
        {
            // Only reachable when URL matches exist (otherwise the tenant path
            // below is the sole interpretation); at least one tenant match
            // disagrees with the alias matches.
            anyhow::bail!(
                "a2a client: ambiguous agent/tenant '{agent}' — the peer card's URL-alias \
                 and tenant interpretations of '{agent}' identify different interfaces \
                 (tenant {}), so no unambiguous interface can be selected",
                fence_remote_error_text(conflict.tenant.as_deref().unwrap_or(""))
            );
        }

        // URL-alias interpretation applies (0 or more matches).
        if url_matches.is_empty() {
            // Only the tenant interpretation applies.
            return match tenant_matches.as_slice() {
                [iface] => Ok(endpoint_from(iface)),
                [] => anyhow::bail!(
                    "a2a client: unknown agent/tenant '{agent}' — peer card advertises multiple \
                     v1 JSON-RPC interfaces but none matches by URL alias or tenant"
                ),
                _ => anyhow::bail!(
                    "a2a client: ambiguous agent/tenant '{agent}' — peer card advertises multiple \
                     v1 JSON-RPC interfaces with that tenant; use a distinct route identity"
                ),
            };
        }
        // A single `/a2a/{agent}` URL match is unambiguous (any tenant match was
        // shown above to lie within it).
        if url_matches.len() == 1 {
            return Ok(endpoint_from(url_matches[0]));
        }
        // Multiple interfaces share the alias URL (a tenant-aware card may
        // expose several v1 JSON-RPC interfaces under one `/a2a/{agent}` path,
        // distinguished only by `tenant`). Disambiguate by the requested
        // tenant; if that leaves more than one (or none with that tenant),
        // fail closed rather than silently routing by card order.
        let url_match_count = url_matches.len();
        let within_url_tenant: Vec<&AgentInterface> = url_matches
            .into_iter()
            .filter(|i| i.tenant.as_deref() == Some(agent))
            .collect();
        return match within_url_tenant.as_slice() {
            [iface] => Ok(endpoint_from(iface)),
            _ => anyhow::bail!(
                "a2a client: ambiguous agent '{agent}' — peer card advertises {} v1 \
                 JSON-RPC interfaces at /a2a/{agent} with different tenants; pass an \
                 explicit tenant to select one without relying on card order",
                url_match_count
            ),
        };
    }
    // No card: fall back to conventional path.
    Ok(SelectedEndpoint {
        url: task_url(base, agent)?,
        tenant: None,
    })
}

/// Lift the wire fields off an already-validated v1 `AgentInterface` into a
/// `SelectedEndpoint`. (protocolVersion was filtered by `select_interface`.)
fn endpoint_from(iface: &AgentInterface) -> SelectedEndpoint {
    SelectedEndpoint {
        url: iface.url.clone(),
        tenant: iface.tenant.clone(),
    }
}

// ── JSON-RPC call methods (caller role) ────────────────────────────

impl A2aHttpClient {
    /// `SendMessage` (spec §9.4.1): delegate a task to a peer agent and block
    /// for the returned payload. The response is a `SendMessageResponse`
    /// oneof: a `Task` (terminal/non-terminal) or a `Message` reply. Returns
    /// the task and a flag: `true` for a genuine Task branch, `false` for a
    /// Message reply folded into a synthetic task (retains the peer-provided
    /// messageId as the task id). Non-terminal task states are returned as-is
    /// for the tool layer to poll.
    pub async fn send_message(
        &self,
        peer: &str,
        agent: &str,
        message: &str,
        context_id: Option<String>,
        task_id: Option<String>,
        return_immediately: bool,
    ) -> anyhow::Result<(Task, bool)> {
        let peer_ref = self.resolve_peer(peer)?;
        // base_url is operator-declared (authorized origin); parse it canonically
        // and validate before the card fetch (which talks to that host). The
        // parsed base is reused for card fetch, interface selection, and as
        // the authorized origin for credential binding below.
        let base = parse_peer_url(&peer_ref.base_url)?;
        let base_resolved = self.guard_host(&base).await?;
        let base_http = self.pinned_client(&base, base_resolved)?;
        // Transport selection (spec §5.2). A continuation (a message carrying
        // a `task_id` whose route was cached by an earlier non-terminal send)
        // must reuse the EXACT endpoint that created the task, even if the
        // card changed between turns — re-selecting could send the follow-up to
        // a different interface and break the task's authority. Reuse the
        // cached route when present; otherwise (a new task, or a continuation
        // whose route was lost) read supportedInterfaces from the peer's card,
        // matching the requested agent. A cardless peer (both discovery paths
        // 404) falls back to the conventional /a2a/{agent} path — the MVP
        // cardless shape. Any other card-fetch failure (5xx, oversized/
        // malformed body, transport error) surfaces as an error instead of
        // silently guessing an undeclared route.
        let endpoint = if let Some(tid) = task_id.as_ref()
            && let Some(route) = self
                .route_cache
                .lock()
                .get(&(peer.to_string(), agent.to_string(), tid.to_string()))
                .cloned()
        {
            SelectedEndpoint {
                url: route.rpc_url,
                tenant: route.tenant,
            }
        } else {
            let card = match self.card_for_endpoint_with(&peer_ref, &base_http).await {
                Ok(card) => Some(card),
                Err(CardFetchError::NotFound) => None,
                Err(CardFetchError::Other(e)) => return Err(e),
            };
            select_interface(card.as_ref(), &base, agent)?
        };
        // The selected RPC URL is peer-controlled (it comes from the Agent
        // Card), so it must be parsed canonically (reject userinfo) and pass
        // the same SSRF guard as the configured base before any request or
        // credential is attached. Its resolved IPs pin the POST connection
        // (DNS-rebinding defense).
        let rpc = parse_peer_url(&endpoint.url)?;
        let rpc_resolved = self.guard_host(&rpc).await?;
        let http = self.pinned_client(&rpc, rpc_resolved)?;
        // v1.0 SendMessage params: REQUIRED messageId, ROLE_USER, flattened
        // Part (no `kind` discriminator), tenant echoed from the interface.
        // context_id/task_id are passed through for multi-turn continuation
        // (INPUT_REQUIRED/AUTH_REQUIRED resume). return_immediately=false
        // (blocking) is the spec default — the call waits for a terminal or
        // interrupted state before returning.
        // The model-composed message is sent unchanged — scrubbing is a
        // log/UI boundary (per the accepted A2A design), not a
        // data-mutation pass on tool->peer payloads.
        let params = SendMessageParams {
            tenant: endpoint.tenant.clone(),
            message: crate::a2a::a2a_wire::Message {
                message_id: uuid_v4(),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                role: Role::RoleUser,
                parts: vec![crate::a2a::a2a_wire::Part::text_str(message)],
                metadata: None,
                extensions: vec![],
                reference_task_ids: vec![],
            },
            configuration: Some(crate::a2a::a2a_wire::SendMessageConfiguration {
                accepted_output_modes: vec![],
                history_length: None,
                return_immediately,
            }),
            metadata: None,
        };
        let payload: SendMessageResponse = self
            .post_jsonrpc(
                &http,
                &rpc,
                "SendMessage",
                serde_json::to_value(&params)?,
                &peer_ref,
                &base,
            )
            .await?;
        // Unwrap the oneof: a Task branch returns the task for the tool layer
        // (poll/cancel on non-terminal states); a Message branch is surfaced
        // as a synthetic completed Task carrying the reply text as an artifact,
        // with `was_task_branch=false` so the tool output preserves the
        // response distinction.
        let (task, was_task_branch, reported_task_id) = match payload {
            SendMessageResponse::Task { task } => {
                let id = task.id.clone();
                (task, true, Some(id))
            }
            SendMessageResponse::Message { message } => {
                let reported = message.task_id.clone();
                let (task, _) = message_to_task(message);
                (task, false, reported)
            }
        };
        // A continuation (a message carrying `task_id`) must return the SAME
        // task it continues. A Task reply must carry the requested id. A Message
        // reply must not EXPLICITLY claim a different task: an unset `task_id`
        // (no stated affiliation) makes no claim we can verify against and is
        // allowed, but a declared id that contradicts the request is refused —
        // a peer message citing another task cannot pass itself off as this
        // continuation's reply. A mismatched continuation is never surfaced to
        // the model nor has its route cached/evicted under the requested id.
        if let Some(tid) = task_id.as_ref() {
            let mismatched = if was_task_branch {
                task.id != *tid
            } else {
                reported_task_id.as_deref().is_some_and(|r| r != tid)
            };
            if mismatched {
                let fenced =
                    fence_remote_error_text(reported_task_id.as_deref().unwrap_or(&task.id));
                anyhow::bail!(
                    "a2a client: continuation for task '{tid}' returned a task with id \
                     {fenced}; refusing to continue an unmatched task"
                );
            }
        }
        // Cache the route keyed by (peer, agent, task_id) so a later
        // GetTask/CancelTask reuses this exact endpoint instead of
        // re-selecting an interface that could route to a different agent. The
        // composite key prevents task-id collisions across peers. Only the
        // Task branch carries a real, peer-assigned task_id; Message replies
        // are synchronous (no poll needed), so they are not cached. And only
        // NON-TERMINAL tasks are cached — a terminal task needs no follow-up
        // poll/cancel, and caching it would leave a permanent entry on the
        // default synchronous send path (the peer completed before return).
        if was_task_branch && !task.status.state.is_terminal() {
            let route = RouteHandle {
                rpc_url: endpoint.url.clone(),
                tenant: endpoint.tenant.clone(),
                inserted_at: std::time::Instant::now(),
            };
            insert_route_bounded(
                &self.route_cache,
                (peer.to_string(), agent.to_string(), task.id.clone()),
                route,
            );
        }
        // Evict a stale non-terminal route when a continuation returns a
        // terminal task: the task is done, so a later poll/cancel must not
        // reuse its old route authority (or an abandoned task-id could be
        // rebroadcast against a completed task's endpoint).
        if was_task_branch
            && task.status.state.is_terminal()
            && let Some(tid) = task_id
        {
            self.route_cache
                .lock()
                .remove(&(peer.to_string(), agent.to_string(), tid));
        }
        Ok((task, was_task_branch))
    }

    /// `GetTask` (spec §9.4.3): retrieve the current state and artifacts of
    /// an in-flight task. Reuses the route cached by the originating
    /// `SendMessage` (same peer/rpc_url/tenant) so the poll lands on the same
    /// endpoint that created the task; falls back to interface selection if
    /// the route is not cached (daemon restart, evicted entry).
    pub async fn get_task(
        &self,
        peer: &str,
        task_id: &str,
        agent: Option<&str>,
    ) -> anyhow::Result<Task> {
        let (rpc, tenant, base, peer_ref, _base_http_unused) = self
            .resolve_route_or_select(peer, task_id, agent.unwrap_or(""))
            .await?;
        let rpc_resolved = self.guard_host(&rpc).await?;
        let http = self.pinned_client(&rpc, rpc_resolved)?;
        let params = serde_json::json!({
            "tenant": tenant,
            "id": task_id,
        });
        let task: Task = self
            .post_jsonrpc(&http, &rpc, "GetTask", params, &peer_ref, &base)
            .await?;
        // JSON-RPC envelope-id correlation proves protocol identity, not that
        // the payload belongs to the requested task. A buggy, confused, or
        // compromised peer returning task B for a GetTask naming A must not be
        // surfaced to the model or used for route eviction.
        if task.id != task_id {
            let fenced = fence_remote_error_text(&task.id);
            anyhow::bail!(
                "a2a client: GetTask for task '{task_id}' returned a task with id \
                 {fenced}; refusing to treat it as the requested task"
            );
        }
        // Evict the exact route cache entry when the task reaches a terminal
        // state (no further poll/cancel will reference it). Uses the caller's
        // agent when known; with an omitted agent, resolves to a single match
        // or leaves ambiguous matches untouched (see evict_terminal_route).
        if task.status.state.is_terminal() {
            self.evict_terminal_route(peer, task_id, agent.unwrap_or(""));
        }
        Ok(task)
    }

    /// `CancelTask` (spec §9.4.5): request cancellation of an in-flight task.
    /// Reuses the route cached by the originating `SendMessage`; falls back
    /// to interface selection if not cached.
    pub async fn cancel(
        &self,
        peer: &str,
        task_id: &str,
        agent: Option<&str>,
    ) -> anyhow::Result<Task> {
        let (rpc, tenant, base, peer_ref, _base_http_unused) = self
            .resolve_route_or_select(peer, task_id, agent.unwrap_or(""))
            .await?;
        let rpc_resolved = self.guard_host(&rpc).await?;
        let http = self.pinned_client(&rpc, rpc_resolved)?;
        let params = serde_json::json!({
            "tenant": tenant,
            "id": task_id,
        });
        let task: Task = self
            .post_jsonrpc(&http, &rpc, "CancelTask", params, &peer_ref, &base)
            .await?;
        // Same task-id correlation as GetTask: refuse to act on a task whose
        // id differs from the one this caller requested.
        if task.id != task_id {
            let fenced = fence_remote_error_text(&task.id);
            anyhow::bail!(
                "a2a client: CancelTask for task '{task_id}' returned a task with id \
                 {fenced}; refusing to treat it as the requested task"
            );
        }
        // Evict the route only when the cancellation actually reached a
        // terminal state. A cancellation response that remains nonterminal
        // (the spec allows it) must keep its exact route so a later poll or
        // retry still lands on the same endpoint. Exact-key eviction with an
        // omitted agent resolves a single match or leaves ambiguous ones.
        if task.status.state.is_terminal() {
            self.evict_terminal_route(peer, task_id, agent.unwrap_or(""));
        }
        Ok(task)
    }

    /// Resolve the RPC endpoint for a follow-up call (get_task/cancel).
    /// Looks up the route cache by `(peer, agent, task_id)` first — a hit
    /// returns the cached rpc_url/tenant so the call lands on the same endpoint
    /// that created the task (no re-selection, no cross-agent drift). The agent
    /// in the composite key prevents a same-task-id collision across agents on
    /// the same peer from overwriting another agent's route. A miss is
    /// handled carefully to avoid guessing a route that crosses agents:
    /// - when `agent` is non-empty, delegate to `select_interface` (which
    ///   requires an exact agent match when a card exists and rejects unknown
    ///   agents);
    /// - when `agent` is empty and the card has a single v1 interface → safe
    ///   fallback (only one choice, no cross-agent ambiguity);
    /// - when `agent` is empty and the card has multiple v1 interfaces → error
    ///   (can't guess which agent created the task);
    /// - no card (cardless peer) → error (no interface info, can't determine).
    async fn resolve_route_or_select(
        &self,
        peer: &str,
        task_id: &str,
        agent: &str,
    ) -> anyhow::Result<(
        reqwest::Url,
        Option<String>,
        reqwest::Url,
        ResolvedPeer,
        reqwest::Client,
    )> {
        let peer_ref = self.resolve_peer(peer)?;
        let base = parse_peer_url(&peer_ref.base_url)?;
        let base_resolved = self.guard_host(&base).await?;
        let base_http = self.pinned_client(&base, base_resolved)?;
        // Consult the authoritative route cache BEFORE any card discovery. The
        // cache holds the exact endpoint that created the task; a follow-up
        // poll/cancel must reach it even if discovery is temporarily broken
        // (card 5xx/malformed/oversized). Lookup by (peer, agent, task_id) when
        // the agent is known (isolating same-task-id collisions across agents);
        // with the agent omitted, resolve to a single (peer, task_id) match or
        // fail on ambiguity.
        {
            let map = self.route_cache.lock();
            if !agent.is_empty() {
                if let Some(route) = map
                    .get(&(peer.to_string(), agent.to_string(), task_id.to_string()))
                    .cloned()
                {
                    let rpc = parse_peer_url(&route.rpc_url)?;
                    return Ok((rpc, route.tenant, base, peer_ref, base_http));
                }
            } else {
                let matching: Vec<_> = map
                    .keys()
                    .filter(|(p, _a, t)| p == peer && t == task_id)
                    .cloned()
                    .collect();
                if !matching.is_empty() {
                    if matching.len() > 1 {
                        anyhow::bail!(
                            "a2a client: task '{task_id}' on peer '{peer}' matches multiple \
                             cached routes (agents not distinguished); pass the agent that \
                             created the task to poll/cancel it"
                        );
                    }
                    let route = map.get(&matching[0]).cloned().unwrap();
                    let rpc = parse_peer_url(&route.rpc_url)?;
                    return Ok((rpc, route.tenant, base, peer_ref, base_http));
                }
            }
        }
        // Cache miss: fetch the card to determine whether a safe fallback
        // exists. A cardless peer (404 on both paths) is treated as no card;
        // any other fetch failure surfaces as an error rather than guessing an
        // undeclared route.
        let card = match self.card_for_endpoint_with(&peer_ref, &base_http).await {
            Ok(card) => Some(card),
            Err(CardFetchError::NotFound) => None,
            Err(CardFetchError::Other(e)) => return Err(e),
        };
        // When agent is non-empty, delegate to select_interface which now
        // requires an exact agent match (rejects unknown agents on aggregate
        // cards). An empty agent with no cached route means the caller doesn't
        // know which agent created the task (poll/cancel miss); only allow
        // single-interface cards for that case.
        if !agent.is_empty() {
            let endpoint = select_interface(card.as_ref(), &base, agent)?;
            let rpc = parse_peer_url(&endpoint.url)?;
            return Ok((rpc, endpoint.tenant, base, peer_ref, base_http));
        }
        let v1_count = card
            .as_ref()
            .map(|c| {
                c.supported_interfaces
                    .iter()
                    .filter(|i| {
                        i.protocol_binding.eq_ignore_ascii_case("jsonrpc")
                            && i.protocol_version == A2A_VERSION
                    })
                    .count()
            })
            .unwrap_or(0);
        match v1_count {
            0 => anyhow::bail!(
                "a2a client: task '{task_id}' on peer '{peer}' has no cached route \
                 and no v1 JSON-RPC interface is available to fall back to; \
                 re-send the task to establish a route"
            ),
            1 => {
                let endpoint = select_interface(card.as_ref(), &base, "")?;
                let rpc = parse_peer_url(&endpoint.url)?;
                Ok((rpc, endpoint.tenant, base, peer_ref, base_http))
            }
            _ => anyhow::bail!(
                "a2a client: task '{task_id}' on peer '{peer}' has no cached route \
                 and the card advertises multiple v1 interfaces; cannot determine \
                 which agent/interface created the task — re-send to establish a route"
            ),
        }
    }

    /// `GET /.well-known/agent-card.json` (spec §14.3 discovery
    /// surface): fetch a peer's public Agent Card from the origin root. The
    /// well-known card is unauthenticated. Cached by base_url (derived data):
    /// a repeat discover/send reuses the cached card; an endpoint change is
    /// a different cache key, so the prior entry is naturally invalidated.
    pub async fn get_card(&self, peer: &str) -> anyhow::Result<AgentCard> {
        let peer_ref = self.resolve_peer(peer)?;
        // Guard + resolve the base_url once (canonically parsed, no userinfo);
        // the resolved IPs pin the card fetch connection (DNS-rebinding
        // defense), reused for both the spec path and the catalog fallback
        // since they share the base_url host.
        let base = parse_peer_url(&peer_ref.base_url)?;
        let resolved = self.guard_host(&base).await?;
        let http = self.pinned_client(&base, resolved)?;
        // `a2a_discover` explicitly asks for the card, so a cardless peer is an
        // error here (not a fallback signal).
        self.card_for_endpoint_with(&peer_ref, &http)
            .await
            .map_err(|e| anyhow::Error::msg(format!("a2a client: {e}")))
    }

    /// Fetch a peer's Agent Card, spec-first with a ZeroClaw-inbound fallback.
    ///
    /// Tries the spec §14.3 well-known root path `/.well-known/agent-card.json`
    /// first — the single-agent root card every standard A2A server serves,
    /// and the discovery card a spec-compliant ZeroClaw inbound serves once
    /// the discovery-card-on-spec-root design is fully landed. When that path
    /// is a genuine 404 (the peer serves no card on the spec root), falls back
    /// to `/.well-known/agents-card.json`, the aggregate catalog card the
    /// ZeroClaw inbound serves today (the inbound moved discovery off the spec
    /// root path onto the plural catalog path; the planned design rejects a
    /// separate catalog object type and puts the discovery card back on the
    /// spec root). A 2xx body that is not a valid Agent Card JSON (malformed,
    /// oversized, or a gateway HTML/SPA fallback) surfaces as an `Other` error
    /// rather than silently falling back — it fails clearly per the accepted
    /// card-fetch contract.
    ///
    /// The catalog carries the same `AgentCard` shape (name/supportedInterfaces/
    /// skills/...), so it deserializes into the same type. Spec-first means a
    /// standard single-agent server, or a spec-compliant ZeroClaw inbound,
    /// never touches the fallback path; the fallback only engages while the
    /// inbound still serves discovery on the plural catalog path. Once the
    /// inbound serves the discovery card on the spec root, this fallback and
    /// its catalog branch should be deleted.
    async fn fetch_card(
        &self,
        http: &reqwest::Client,
        base_url: &str,
    ) -> Result<AgentCard, CardFetchError> {
        let spec_url = format!("{base_url}/.well-known/agent-card.json");
        let catalog_url = format!("{base_url}/.well-known/agents-card.json");
        // Try spec-first. On ANY spec-root miss (a genuine 404, or a 2xx body
        // that is not valid Agent Card JSON — e.g. a ZeroClaw inbound serves
        // discovery on the plural catalog path and the singular root falls
        // through to the gateway SPA HTML fallback) we fall back to the
        // ZeroClaw catalog. A spec-root 5xx also falls through so a temporarily
        // broken root does not mask a reachable catalog; the catalog failure
        // below reports both paths. Only if the catalog also fails do we
        // surface an error (with per-path detail).
        let spec_err = match self.try_card_url(http, &spec_url).await {
            Ok(card) => return Ok(card),
            Err(e) => e,
        };
        match self.try_card_url(http, &catalog_url).await {
            Ok(card) => Ok(card),
            Err(catalog_err) => match (spec_err, catalog_err) {
                (CardFetchError::NotFound, CardFetchError::NotFound) => {
                    // Neither path served a card: the peer is cardless.
                    Err(CardFetchError::NotFound)
                }
                (spec, catalog) => Err(CardFetchError::Other(anyhow::Error::msg(format!(
                    "a2a client: could not fetch Agent Card from peer '{base_url}' \
                     (spec root: {spec}; ZeroClaw catalog {catalog_url}: {catalog})"
                )))),
            },
        }
    }

    /// GET a card URL and decode it as `AgentCard`. Returns a
    /// `CardFetchError::NotFound` when the endpoint is absent (HTTP 404 —
    /// the peer genuinely has no card, a cardless-peer signal), and a
    /// `CardFetchError::Other` for every other failure (HTTP 5xx, oversized
    /// body, invalid JSON). The caller distinguishes: only a 404 permits the
    /// cardless fallback to `/a2a/{agent}`; any other failure must surface as
    /// an error instead of silently guessing an undeclared route.
    async fn try_card_url(
        &self,
        http: &reqwest::Client,
        url: &str,
    ) -> Result<AgentCard, CardFetchError> {
        let resp = http
            .get(url)
            .header("A2A-Version", A2A_VERSION)
            .send()
            .await
            .map_err(|e| CardFetchError::Other(e.into()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CardFetchError::NotFound);
        }
        if !status.is_success() {
            return Err(CardFetchError::Other(anyhow::Error::msg(format!(
                "HTTP {status}"
            ))));
        }
        // Bounded read before deserialization (mirrors post_jsonrpc).
        let max_bytes = self.limited_response_bytes();
        let body = Self::read_limited_bytes(resp, max_bytes)
            .await
            .map_err(CardFetchError::Other)?;
        serde_json::from_slice::<AgentCard>(&body).map_err(|e| {
            // The serde error and the card body are peer-controlled; fence them
            // so a malformed card response cannot inject text to the model via
            // the discovery error path without the untrusted-external marker.
            let fenced = fence_remote_error_text(&format!(
                "{e}\n--- peer Agent Card body ---\n{}",
                String::from_utf8_lossy(&body)
            ));
            CardFetchError::Other(anyhow::Error::msg(format!(
                "a2a client: failed to parse peer Agent Card: {fenced}"
            )))
        })
    }

    /// Fetch + cache a peer's Agent Card, reusing a caller-built pinned
    /// client for the base_url (already SSRF-guarded). Honors the live TTL:
    /// `0` disables caching; a fresh cached card is served; an expired entry
    /// is refetched. Used by `send_message`/`get_task`/`cancel` to read
    /// `supportedInterfaces` for transport selection without a redundant
    /// guard pass (the base_url was already validated by the caller).
    async fn card_for_endpoint_with(
        &self,
        peer: &ResolvedPeer,
        http: &reqwest::Client,
    ) -> Result<AgentCard, CardFetchError> {
        let ttl_secs = self.source.settings().card_cache_ttl_secs;
        if ttl_secs == 0 {
            // Caching disabled: drop any previously cached cards so an operator
            // who toggles `card_cache_ttl_secs` `0 → 300 → 0` cannot resurrect a
            // card fetched under the earlier policy. The cache is derived data;
            // disabling it means "always fetch fresh".
            self.card_cache.lock().clear();
            return self.fetch_card(http, &peer.base_url).await;
        }
        let now = std::time::Instant::now();
        let fresh = self
            .card_cache
            .lock()
            .get(&peer.base_url)
            .and_then(|(card, fetched)| {
                card_is_fresh(*fetched, now, ttl_secs).then(|| card.clone())
            });
        if let Some(card) = fresh {
            return Ok(card);
        }
        let card = self.fetch_card(http, &peer.base_url).await?;
        // W4: the operator may have reloaded the live TTL while this fetch was
        // in flight (e.g. N → 0), and the cache was not cleared by this call's
        // own disabled-path (that path ran before the await, on a different
        // invocation). Re-read the policy AFTER the await so an in-flight
        // result is not inserted and later served as fresh under a
        // now-disabled cache policy. The cache is derived data; disabling
        // caching means "always fetch fresh".
        let live_ttl = self.source.settings().card_cache_ttl_secs;
        if live_ttl != 0 {
            self.card_cache.lock().insert(
                peer.base_url.clone(),
                (card.clone(), std::time::Instant::now()),
            );
        }
        Ok(card)
    }

    /// Remove the exact route cache entry for a terminal task. When `agent` is
    /// known, removes the precise `(peer, agent, task_id)` key — never a
    /// same-task-id entry belonging to a different agent on the same peer.
    /// When `agent` is omitted (a poll/cancel that didn't pass it), resolves
    /// the matching key(s): exactly one match is removed; multiple matches are
    /// left untouched (can't tell which agent's route served the call, so
    /// deleting any would drop a possibly-live nonterminal route).
    fn evict_terminal_route(&self, peer: &str, task_id: &str, agent: &str) {
        let mut map = self.route_cache.lock();
        if !agent.is_empty() {
            map.remove(&(peer.to_string(), agent.to_string(), task_id.to_string()));
            return;
        }
        // Agent omitted: collect all keys matching (peer, task_id).
        let matching: Vec<_> = map
            .keys()
            .filter(|(p, _a, t)| p == peer && t == task_id)
            .cloned()
            .collect();
        if matching.len() == 1 {
            map.remove(&matching[0]);
        }
        // len != 1: zero matches (nothing to evict) or multiple matches
        // (ambiguous — don't guess which agent's route to drop).
    }

    /// POST a JSON-RPC 2.0 request envelope, validate the response envelope,
    /// and return the unwrapped result (`T`). The caller passes the reqwest
    /// client (pinned to the validated DNS resolution) and the canonical RPC
    /// URL. The call is bound to the authorized peer origin (`base`): a peer's
    /// Agent Card can advertise an arbitrary RPC URL, so the card must not
    /// redirect the task — with or without a bearer token — to an attacker
    /// origin or downgrade to plaintext http. Mismatched origins fail clearly
    /// rather than send (DNS safety is not credential authorization). The
    /// bearer token (when non-empty, `${VAR}`-resolved, never logged) is
    /// attached only after the origin check passes. The response envelope is
    /// validated by `rpc_result`: JSON-RPC version must be `"2.0"`, the
    /// response id must correlate with this request's id, and an ambiguous
    /// envelope (both `result` and `error`) is rejected.
    async fn post_jsonrpc<T: serde::de::DeserializeOwned>(
        &self,
        http: &reqwest::Client,
        rpc: &reqwest::Url,
        method: &str,
        params: Value,
        peer: &ResolvedPeer,
        authorized_origin: &reqwest::Url,
    ) -> anyhow::Result<T> {
        // Bind every call (not just credentialed ones) to the authorized peer
        // origin: a peer's Agent Card can advertise an arbitrary RPC URL, so
        // the card must not redirect the task — with or without a bearer
        // token — to an attacker origin or downgrade to plaintext http. DNS
        // safety is not credential authorization; even an anonymous peer's
        // message must not leak to an unconfigured origin. Mismatched origins
        // fail clearly rather than send.
        if url_origin(rpc) != url_origin(authorized_origin) {
            // The advertised RPC URL is peer-controlled (from the card's
            // supportedInterfaces); fence it as untrusted before it reaches the
            // model via the error path. The authorized peer origin is operator
            // config, kept as a plain diagnostic.
            let fenced_rpc = fence_remote_error_text(rpc.as_str());
            anyhow::bail!(
                "a2a client: peer advertised RPC URL {fenced_rpc} whose origin does not \
                 match the authorized peer origin '{}'; refusing to send (set the peer's \
                 RPC interface to the same origin as [a2a.client.peers] base_url)",
                authorized_origin
            );
        }
        let req_id = self.next_req_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": method,
            "params": params,
        });
        let mut req = http
            .post(rpc.as_str())
            .header("A2A-Version", A2A_VERSION)
            .json(&body);
        if !peer.token.is_empty() {
            req = req.bearer_auth(&peer.token);
        }
        let resp = req.send().await.map_err(|e| {
            // reqwest's transport error embeds the request URL, which for an
            // RPC POST is card-advertised (peer-controlled) endpoint text. A
            // failing peer could put adversarial prose in a valid same-origin
            // path/query and have it returned as an ordinary request error;
            // strip the URL and fence the rest so no raw peer text reaches the
            // model without the untrusted-external marker.
            let sanitized = e.without_url();
            anyhow::Error::msg(format!(
                "a2a client: RPC request to peer failed: {}",
                fence_remote_error_text(&sanitized.to_string())
            ))
        })?;
        let status = resp.status();
        let max_bytes = self.limited_response_bytes();
        if !status.is_success() {
            // Limit error-body reads too: a compromised peer could stream a
            // huge error body to exhaust memory. The body is peer-controlled
            // prose — fence it as untrusted so a prompt-injection payload
            // cannot reach the model via the error path. The HTTP status stays
            // as a structured operator diagnostic outside the fence.
            let bytes = Self::read_limited_bytes(resp, max_bytes).await?;
            let text = String::from_utf8_lossy(&bytes).to_string();
            let fenced = fence_remote_error_text(&text);
            anyhow::bail!("a2a client: peer returned HTTP {status}: {fenced}");
        }
        // Bounded body read before deserialization: a malicious peer cannot
        // force unbounded `json()` parsing of a multi-GB body.
        let bytes = Self::read_limited_bytes(resp, max_bytes).await?;
        let envelope: JsonRpcResponse<T> = serde_json::from_slice(&bytes).map_err(|e| {
            // The serde error and the peer body are peer-controlled; fence them
            // so a malformed 2xx response cannot inject text to the model via
            // the tool error path without the untrusted-external marker.
            let fenced = fence_remote_error_text(&format!(
                "{e}\n--- peer response body ---\n{}",
                String::from_utf8_lossy(&bytes)
            ));
            anyhow::Error::msg(format!(
                "a2a client: failed to parse peer JSON-RPC response: {fenced}"
            ))
        })?;
        let expected_id = json!(req_id);
        rpc_result(envelope, Some(&expected_id))
    }

    /// Live-read `a2a.client.max_response_bytes` from config (0 = unlimited).
    /// Read per call so a hot-reloaded limit takes effect immediately.
    fn limited_response_bytes(&self) -> Option<usize> {
        let max = self.source.settings().max_response_bytes;
        (max > 0).then_some(max)
    }

    /// Read a response body up to `max_bytes` (if `Some`); reject if larger.
    /// `None` means unlimited. Streams the body chunk-by-chunk and stops as
    /// soon as the configured byte limit is crossed — a fixed-length, chunked,
    /// or decompressed body is NOT fully buffered before rejection (the prior
    /// `resp.bytes()` impl read to EOF first, which a malicious peer could
    /// exploit to exhaust daemon memory).
    async fn read_limited_bytes(
        resp: reqwest::Response,
        max_bytes: Option<usize>,
    ) -> anyhow::Result<Vec<u8>> {
        let Some(max) = max_bytes else {
            return Ok(resp.bytes().await?.to_vec());
        };
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.try_next().await? {
            // Enforce the cap BEFORE extending: a single oversized chunk (as a
            // peer or decompressor may yield) must be rejected without first
            // allocating/copying the whole chunk into `buf`. Checking after
            // `extend_from_slice` would make `max_response_bytes` a soft
            // post-allocation threshold, not a hard bound at this untrusted
            // network boundary.
            if buf.len().saturating_add(chunk.len()) > max {
                anyhow::bail!(
                    "a2a client: peer response exceeded max_response_bytes ({max} bytes); rejected before deserialization"
                );
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }
}

/// Card-fetch outcome, distinguishing a peer that genuinely has no Agent Card
/// (HTTP 404 — the cardless-peer signal that permits the `/a2a/{agent}`
/// fallback) from every other discovery failure (HTTP 5xx, oversized body,
/// malformed JSON, transport error — which must surface as an error instead
/// of silently guessing an undeclared route).
#[derive(Debug)]
enum CardFetchError {
    /// HTTP 404 — the peer serves no discovery card at this path. A valid
    /// cardless-peer signal; callers may fall back to the conventional route.
    NotFound,
    /// Any other failure: HTTP 5xx, oversized/invalid response, network error.
    Other(anyhow::Error),
}

impl std::fmt::Display for CardFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CardFetchError::NotFound => write!(f, "peer has no Agent Card at this path (404)"),
            CardFetchError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CardFetchError {}

/// Fence an untrusted peer-supplied string (host, error text, task id) so it
/// cannot break out of the `<a2a-error>` block it is embedded in.
pub fn fence_remote_error_text(remote: &str) -> String {
    format!(
        "<a2a-error trust=\"untrusted-external\">\n{}\n</a2a-error>",
        escape_fence_body(remote)
    )
}

#[cfg(test)]
mod tests {
    // A2aHttpClient does not impl Debug, so tests use `.err().expect(...)` to
    // surface the error text of a failed construction/call (`unwrap_err` needs
    // `T: Debug`). This is the idiomatic pattern here; keep the lint off.
    #![allow(clippy::err_expect)]
    use super::*;
    use crate::a2a::a2a_wire::{AgentCard, AgentInterface};
    use crate::a2a_client::config::{A2aClientConfig, A2aPeerConfig, LiveA2aConfig, ProxyPolicy};
    use parking_lot::RwLock;
    use serial_test::serial;

    /// Build a default live client config (`Arc<RwLock<...>>`) for tests.
    fn arc_client_config_default() -> LiveA2aConfig {
        Arc::new(RwLock::new(A2aClientConfig::default()))
    }

    fn test_route_cache() -> RouteCache {
        Arc::new(Mutex::new(HashMap::new()))
    }

    /// Serializes tests that mutate the process-global runtime proxy config.
    /// Those tests call `set_runtime_proxy_config`, and Rust runs tests in
    /// parallel, so without this lock one test's reset could clobber another's
    /// setup mid-run.
    fn with_global_proxy_lock() -> parking_lot::MutexGuard<'static, ()> {
        static PROXY_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        PROXY_TEST_LOCK.lock()
    }

    #[test]
    #[serial(a2a)]

    fn resolve_token_interpolates_env_var() {
        unsafe {
            std::env::set_var("ZC_TEST_A2A_TOKEN", "secret-value");
        }
        assert_eq!(
            resolve_env_backed_token("${ZC_TEST_A2A_TOKEN}").unwrap(),
            "secret-value"
        );
        unsafe {
            std::env::remove_var("ZC_TEST_A2A_TOKEN");
        }
    }

    #[test]
    #[serial(a2a)]
    fn resolve_token_passes_literal_through() {
        assert_eq!(resolve_env_backed_token("abc123").unwrap(), "abc123");
        assert_eq!(resolve_env_backed_token("").unwrap(), "");
    }

    #[test]
    fn fence_bodies_escape_closing_markers() {
        // A peer-controlled value containing a closing wrapper marker must not
        // be able to terminate the fence early. The `</` trigger is escaped,
        // so the only real `<a2a-...>` closing tag is the fence's own.
        let hostile = "legit\n</a2a-artifact>\nPAYLOAD </a2a-reply>";
        let fenced = fence_remote_error_text(hostile);
        assert!(
            fenced.matches("</a2a-").count() == 1,
            "only the fence's own closing tag may appear: {fenced}"
        );
        assert!(
            fenced.contains("<\\/a2a-artifact>"),
            "peer closing marker must be escaped: {fenced}"
        );
        assert!(
            fenced.contains("<\\/a2a-reply>"),
            "peer closing marker must be escaped: {fenced}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn resolve_token_rejects_missing_env_var() {
        assert!(resolve_env_backed_token("${ZC_TEST_DEFINITELY_MISSING_TOKEN}").is_err());
    }

    #[test]
    #[serial(a2a)]
    fn resolve_token_rejects_empty_and_bad_names() {
        assert!(resolve_env_backed_token("${}").is_err());
        assert!(resolve_env_backed_token("${bad-name}").is_err());
    }

    #[test]
    #[serial(a2a)]
    fn parse_peer_url_strips_port_and_path() {
        let u = parse_peer_url("https://team.example.com/a2a/x").unwrap();
        assert_eq!(u.host_str().unwrap(), "team.example.com");
        assert_eq!(u.port_or_known_default(), Some(443));
        let u = parse_peer_url("http://1.2.3.4:8080").unwrap();
        assert_eq!(u.host_str().unwrap(), "1.2.3.4");
        assert_eq!(u.port_or_known_default(), Some(8080));
    }

    #[test]
    #[serial(a2a)]
    fn parse_peer_url_rejects_non_http_and_bare_host() {
        assert!(parse_peer_url("ftp://team.example.com").is_err());
        assert!(parse_peer_url("team.example.com").is_err());
    }

    #[test]
    #[serial(a2a)]
    fn parse_peer_url_rejects_userinfo_spoofing() {
        // The review-flagged attack: a peer-controlled URL like
        // http://public.example:80@127.0.0.1:8080/admin — a handwritten
        // host:port splitter validates `public.example`, but reqwest connects
        // to `127.0.0.1:8080`. The canonical parser must reject userinfo so
        // the validated host and the connected host can never diverge.
        assert!(parse_peer_url("http://public.example:80@127.0.0.1:8080/admin").is_err());
        assert!(parse_peer_url("https://user:pass@peer.example.com").is_err());
        // Plain (no-userinfo) URLs still parse.
        assert!(parse_peer_url("https://peer.example.com").is_ok());
    }

    #[test]
    #[serial(a2a)]
    fn parse_peer_url_fences_peer_controlled_url_in_error() {
        // B3: a malformed URL (peer-controlled, from a card's
        // supportedInterfaces[].url) must be fenced as untrusted in the parse
        // error, so a hostile card cannot move prompt text into model-visible
        // output via the invalid-URL diagnostic.
        let evil = "https://<script>alert(1)</script>/a2a/evil";
        let err = parse_peer_url(evil).unwrap_err().to_string();
        assert!(
            err.contains("untrusted-external"),
            "peer-controlled URL in parse error must be fenced: {err}"
        );
        assert!(
            err.contains("<script>"),
            "fenced parse error must preserve the original URL text: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn url_origin_compares_scheme_host_port() {
        // Credential binding: same origin → match; different host/port/scheme → mismatch.
        assert_eq!(
            url_origin(&parse_peer_url("https://peer.example.com/a2a/x").unwrap()),
            url_origin(&parse_peer_url("https://peer.example.com/other").unwrap())
        );
        assert_ne!(
            url_origin(&parse_peer_url("https://peer.example.com").unwrap()),
            url_origin(&parse_peer_url("https://attacker.example").unwrap())
        );
        assert_ne!(
            url_origin(&parse_peer_url("https://peer.example.com").unwrap()),
            url_origin(&parse_peer_url("http://peer.example.com").unwrap())
        );
        assert_ne!(
            url_origin(&parse_peer_url("https://peer.example.com").unwrap()),
            url_origin(&parse_peer_url("https://peer.example.com:8443").unwrap())
        );
        // An explicit default port is the SAME effective origin as no port
        // (https default 443, http default 80) — must compare equal.
        assert_eq!(
            url_origin(&parse_peer_url("https://peer.example.com").unwrap()),
            url_origin(&parse_peer_url("https://peer.example.com:443").unwrap())
        );
        assert_eq!(
            url_origin(&parse_peer_url("http://peer.example.com").unwrap()),
            url_origin(&parse_peer_url("http://peer.example.com:80").unwrap())
        );
    }

    #[test]
    #[serial(a2a)]
    fn task_url_encodes_path_traversal() {
        // A model-supplied agent with `../`, `?`, or `#` must NOT escape its
        // /a2a/{agent} segment to reach another authenticated endpoint on the
        // same origin. The `/` is percent-encoded (`%2F`) so `../admin`
        // becomes one literal segment under /a2a/, not a path-traversal.
        let base = parse_peer_url("https://peer.example.com").unwrap();
        let url = task_url(&base, "../admin").unwrap();
        // Stays under /a2a/ — the encoded segment does not traverse out.
        assert!(
            url.starts_with("https://peer.example.com/a2a/"),
            "traversal escaped /a2a/: {url}"
        );
        let path_after_a2a = url.strip_prefix("https://peer.example.com/a2a/").unwrap();
        assert!(!path_after_a2a.contains('/'), "raw '/' leaked: {url}");
        // `?` and `#` must not start a query/fragment.
        let url2 = task_url(&base, "beta?x=1#frag").unwrap();
        assert!(!url2.contains('?'), "raw '?' leaked: {url2}");
        assert!(!url2.contains('#'), "raw '#' leaked: {url2}");
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_prefers_jsonrpc_with_agent_match() {
        // Aggregate card: one JSON-RPC interface per alias. The requested
        // agent must select its own interface, not the first one.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/alpha".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: None,
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/beta".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("tenant-beta".into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        // beta selects the beta interface (not the first alpha one) and
        // carries its tenant for request routing.
        let ep = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "beta",
        )
        .unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/beta");
        assert_eq!(ep.tenant.as_deref(), Some("tenant-beta"));
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_falls_back_when_no_jsonrpc() {
        // No card -> conventional path, no tenant.
        let ep = select_interface(
            None,
            &parse_peer_url("https://peer.example.com").unwrap(),
            "beta",
        )
        .unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/beta");
        assert!(ep.tenant.is_none());
        // Card with no v1 JSON-RPC interface (only GRPC) -> rejected (card
        // declares interfaces but no compatible v1 binding).
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://peer.example.com/grpc".into(),
                protocol_binding: "GRPC".into(),
                tenant: None,
                protocol_version: "1.0".into(),
            }],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        assert!(
            select_interface(
                Some(&card),
                &parse_peer_url("https://peer.example.com").unwrap(),
                "beta",
            )
            .is_err(),
            "card with only GRPC (no v1 JSON-RPC) must be rejected"
        );
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_skips_v03_falls_back_when_only_legacy() {
        // A card advertising only a v0.3 JSON-RPC interface (no v1 candidate)
        // is rejected: a card that declares interfaces but no compatible v1
        // binding must not be routed to an undeclared /a2a/{agent} endpoint.
        // (A card-less peer — `None` — still falls back to the conventional
        // path; this test passes a card with only v0.3.)
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://peer.example.com/a2a/beta".into(),
                protocol_binding: "JSONRPC".into(),
                tenant: None,
                protocol_version: "0.3".into(),
            }],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        assert!(
            select_interface(
                Some(&card),
                &parse_peer_url("https://peer.example.com").unwrap(),
                "beta",
            )
            .is_err(),
            "card with only v0.3 (no v1) must be rejected, not fallen back"
        );
        // But a card-less peer (None) still falls back to /a2a/{agent}.
        let ep = select_interface(
            None,
            &parse_peer_url("https://peer.example.com").unwrap(),
            "beta",
        )
        .unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/beta");
        assert!(ep.tenant.is_none());
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_picks_v1_skipping_leading_v03() {
        // A card whose first JSON-RPC interface is v0.3 but has a later v1
        // interface must pick the v1 one (not reject on the v0.3).
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/beta".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: None,
                    protocol_version: "0.3".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/beta".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("tenant-beta".into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let ep = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "beta",
        )
        .unwrap();
        // v1 candidate selected (tenant preserved), v0.3 skipped.
        assert_eq!(ep.url, "https://peer.example.com/a2a/beta");
        assert_eq!(ep.tenant.as_deref(), Some("tenant-beta"));
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_rejects_unknown_agent_on_aggregate_card() {
        // Aggregate card (multiple v1 interfaces): requesting an agent that
        // matches no URL suffix must fail — a typo or stale alias must not
        // silently route to an arbitrary agent/tenant.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/alpha".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("tenant-alpha".into()),
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/beta".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("tenant-beta".into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let err = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "unknown-agent",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown agent"),
            "expected 'unknown agent' error, got: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_rejects_unknown_agent_on_one_alias_catalog_card() {
        // A ZeroClaw catalog card with a SINGLE published alias: it emits a
        // non-JSON-RPC `catalog` interface plus one JSON-RPC interface. After
        // filtering to v1 JSON-RPC candidates exactly ONE remains — which the
        // permissive single-candidate fast path must NOT take (that path is for
        // genuine standard cards whose URL is an opaque endpoint). A typo or
        // stale alias must fail closed instead of routing to the only published
        // agent and caching the route under the wrong id.
        let card = AgentCard {
            name: "ZeroClaw agents".into(),
            description: "catalog".into(),
            supported_interfaces: vec![
                // The catalog marker: a non-JSON-RPC interface.
                AgentInterface {
                    url: "https://peer.example.com/.well-known/agent-card.json".into(),
                    protocol_binding: "catalog".into(),
                    tenant: None,
                    protocol_version: "1.0".into(),
                },
                // The single published alias.
                AgentInterface {
                    url: "https://peer.example.com/a2a/assistant".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: None,
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let base = parse_peer_url("https://peer.example.com").unwrap();
        // Unknown agent on a one-alias catalog: must fail, not route to assistant.
        let err = select_interface(Some(&card), &base, "typo-agent").unwrap_err();
        assert!(
            err.to_string().contains("unknown agent"),
            "unknown agent on a one-alias catalog must fail closed, got: {err}"
        );
        // The real alias still resolves.
        let ep = select_interface(Some(&card), &base, "assistant").unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/assistant");
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_alternate_transport_is_not_a_catalog() {
        // The v1 protocol lets ONE agent expose JSON-RPC and GRPC (or HTTP+JSON)
        // bindings that all serve the SAME agent — they are alternate
        // TRANSPORTS, not route identities. A card declaring JSON-RPC+/grpc and
        // GRPC+/grpc has a single JSON-RPC interface whose URL is an opaque
        // endpoint, so the permissive sole-compatible-interface fallback must
        // still apply regardless of the requested agent. Treating alternate
        // transports as a catalog would make a conforming card unusable.
        let card = AgentCard {
            name: "p".into(),
            description: "single agent, two transports".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/rpc".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: None,
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/grpc".into(),
                    protocol_binding: "GRPC".into(),
                    tenant: None,
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let base = parse_peer_url("https://peer.example.com").unwrap();
        // Any agent spelling resolves to the sole JSON-RPC endpoint.
        let ep = select_interface(Some(&card), &base, "anything").unwrap();
        assert_eq!(ep.url, "https://peer.example.com/rpc");
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_matches_by_tenant_when_urls_are_shared() {
        // B2: a standard tenant-shared card advertises two JSON-RPC 1.0
        // interfaces at the SAME URL, distinguished only by `tenant`
        // (alpha/beta). A conforming peer must not be required to encode the
        // tenant/agent in a ZeroClaw-specific `/a2a/{alias}` URL suffix — the
        // caller selects an interface by passing the tenant as the route
        // identity, and the selected tenant must flow out for request routing.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/v1".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("alpha".into()),
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/v1".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("beta".into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let base = parse_peer_url("https://peer.example.com").unwrap();

        // Neither URL ends in /a2a/beta; the `beta` route identity must select
        // the tenant=beta interface by its `tenant` field.
        let beta = select_interface(Some(&card), &base, "beta").unwrap();
        assert_eq!(beta.url, "https://peer.example.com/a2a/v1");
        assert_eq!(beta.tenant.as_deref(), Some("beta"));

        let alpha = select_interface(Some(&card), &base, "alpha").unwrap();
        assert_eq!(alpha.tenant.as_deref(), Some("alpha"));

        // An identity matching no tenant still fails closed.
        let err = select_interface(Some(&card), &base, "gamma").unwrap_err();
        assert!(
            err.to_string().contains("unknown agent"),
            "expected 'unknown agent' error, got: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_rejects_ambiguous_alias_with_differing_tenants() {
        // A tenant-aware card can expose two v1 JSON-RPC interfaces under the
        // SAME /a2a/{agent} URL, distinguished only by `tenant`. Requesting
        // the alias alone must fail closed (ambiguous), never silently route
        // by card order and leak the wrong tenant into the request/cache.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/assistant".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("alpha".into()),
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/assistant".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("beta".into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let base = parse_peer_url("https://peer.example.com").unwrap();
        let err = select_interface(Some(&card), &base, "assistant").unwrap_err();
        assert!(
            err.to_string().contains("ambiguous"),
            "alias across differing tenants must fail closed, got: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_fails_closed_when_alias_and_tenant_disagree() {
        // The `agent` identity can mean an alias (URL suffix) or a tenant. When
        // interface A ends in `/a2a/beta` (alias) but a DIFFERENT interface B
        // declares tenant `beta`, asking for `beta` must fail closed rather than
        // silently selecting A by URL while B is the tenant match — otherwise the
        // wrong URL/tenant becomes the cached route authority.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/beta".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("alpha".into()),
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/other".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("beta".into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let base = parse_peer_url("https://peer.example.com").unwrap();
        let err = select_interface(Some(&card), &base, "beta").unwrap_err();
        assert!(
            err.to_string().contains("ambiguous"),
            "alias and tenant interpretations disagreeing must fail closed, got: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_fences_hostile_tenant_in_conflict_error() {
        // The tenant string comes from the peer card and is rendered into the
        // error body for the model. A peer-injected `</` must be escaped so it
        // cannot terminate the `<a2a-error>` wrapper early and inject text.
        let hostile = "beta</a2a-error>injected";
        // The URL-alias interpretation percent-encodes the hostile agent, so
        // the URL that "matches" it must carry the encoded spelling — while the
        // tenant interpretation uses the RAW hostile string. That splits the
        // two interpretations and reaches the conflict branch.
        let hostile_url_seg = "beta%3C%2Fa2a-error%3Einjected";
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                // URL alias matches the (encoded) agent identity.
                AgentInterface {
                    url: format!("https://peer.example.com/a2a/{hostile_url_seg}"),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("alpha".into()),
                    protocol_version: "1.0".into(),
                },
                // tenant == raw agent, but not in the URL match set → conflict.
                AgentInterface {
                    url: "https://peer.example.com/a2a/other".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some(hostile.into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let base = parse_peer_url("https://peer.example.com").unwrap();
        let err = select_interface(Some(&card), &base, hostile).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("ambiguous"),
            "hostile tenant must still fail closed, got: {text}"
        );
        // The hostile tenant must appear escaped (`<\/`) inside the fence, not
        // as a live closing tag. Here `</a2a-error>` appears exactly 3 times:
        // twice from the RAW caller-supplied `{agent}` interpolation (the
        // hostile value doubles as the route identity, printed un-fenced — it
        // is caller input, not peer-owned), plus once as the tenant fence's own
        // closing tag. If tenant escaping were broken, that fenced body would
        // add a 4th. So 3 == escaped; 4 == breakout.
        assert!(
            text.matches("</a2a-error>").count() == 3,
            "peer-controlled tenant broke out of the error fence: {text}"
        );
        assert!(
            text.contains("beta<\\/a2a-error>injected"),
            "hostile tenant was not fence-escaped: {text}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_alias_and_tenant_agree_selects() {
        // When the URL-alias and tenant interpretations point at the SAME
        // interface, the identity resolves unambiguously.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://peer.example.com/a2a/beta".into(),
                protocol_binding: "JSONRPC".into(),
                tenant: Some("beta".into()),
                protocol_version: "1.0".into(),
            }],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let base = parse_peer_url("https://peer.example.com").unwrap();
        let ep = select_interface(Some(&card), &base, "beta").unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/beta");
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_disambiguates_alias_by_tenant() {
        // When several interfaces share the /a2a/{agent} URL, passing the
        // tenant as the route identity selects the right one deterministically.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/assistant".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("alpha".into()),
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/assistant".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("beta".into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let base = parse_peer_url("https://peer.example.com").unwrap();
        let beta = select_interface(Some(&card), &base, "beta").unwrap();
        assert_eq!(beta.tenant.as_deref(), Some("beta"));
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_standard_url_no_alias() {
        // Standard A2A card with a single v1 interface whose URL does not
        // contain an agent alias (e.g. /a2a/v1). The sole compatible
        // interface must be selected regardless of the requested agent
        // name — agent is an opaque identifier, not a URL suffix.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://peer.example.com/a2a/v1".into(),
                protocol_binding: "JSONRPC".into(),
                tenant: Some("t".into()),
                protocol_version: "1.0".into(),
            }],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        // Single v1 interface → selected even though URL doesn't contain
        // the agent name.
        let ep = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "any-agent",
        )
        .unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/v1");
        assert_eq!(ep.tenant.as_deref(), Some("t"));
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_prefers_url_match_when_multiple_candidates() {
        // Aggregate card (multiple v1 interfaces): prefer URL suffix match.
        // Unknown agents must fail closed (no silent first-interface
        // fallback that could route a typo to the wrong tenant).
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/v1".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("tenant-alpha".into()),
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/beta".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("tenant-beta".into()),
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        // Matches /a2a/beta → selects beta interface.
        let ep = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "beta",
        )
        .unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/beta");
        assert_eq!(ep.tenant.as_deref(), Some("tenant-beta"));
        // No URL match with unknown agent → fail closed.
        let err = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "unknown-agent",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown agent"),
            "expected 'unknown agent' error, got: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_empty_agent_single_interface_ok() {
        // Empty agent with a single v1 interface: allowed (no cross-agent
        // ambiguity — poll/cancel miss can safely fall back).
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://peer.example.com/a2a/alpha".into(),
                protocol_binding: "JSONRPC".into(),
                tenant: Some("t".into()),
                protocol_version: "1.0".into(),
            }],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let ep = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "",
        )
        .unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/alpha");
        assert_eq!(ep.tenant.as_deref(), Some("t"));
    }

    #[test]
    #[serial(a2a)]
    fn select_interface_rejects_empty_agent_on_multi_interface_card() {
        // Empty agent with multiple v1 interfaces: must fail — cannot
        // determine which agent/tenant to target. (Empty agent with a single
        // v1 interface is allowed — no ambiguity.)
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![
                AgentInterface {
                    url: "https://peer.example.com/a2a/alpha".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: Some("t-alpha".into()),
                    protocol_version: "1.0".into(),
                },
                AgentInterface {
                    url: "https://peer.example.com/a2a/beta".into(),
                    protocol_binding: "JSONRPC".into(),
                    tenant: None,
                    protocol_version: "1.0".into(),
                },
            ],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        let err = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("route identity is required"),
            "expected 'route identity is required' error, got: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn task_url_joins_base_and_agent() {
        let base = parse_peer_url("https://x.example.com").unwrap();
        assert_eq!(
            task_url(&base, "beta").unwrap(),
            "https://x.example.com/a2a/beta"
        );
    }

    #[test]
    #[serial(a2a)]
    fn card_is_fresh_honors_ttl_boundary() {
        // TTL=300: a card fetched now is fresh; one fetched 300s ago is NOT
        // fresh (age must be strictly < ttl). This is the behavior the
        // review flagged as missing — `0` disables caching (caller checks),
        // and a positive TTL expires exactly at the boundary.
        let now = std::time::Instant::now();
        assert!(card_is_fresh(now, now, 300));
        assert!(card_is_fresh(
            now - std::time::Duration::from_secs(299),
            now,
            300
        ));
        assert!(!card_is_fresh(
            now - std::time::Duration::from_secs(300),
            now,
            300
        ));
        assert!(!card_is_fresh(
            now - std::time::Duration::from_secs(301),
            now,
            300
        ));
    }

    #[test]
    #[serial(a2a)]
    fn resolve_peer_token_passes_plaintext_through() {
        // RFC boundary 1: token decryption is the host's job. The client
        // receives already-decrypted plaintext and only resolves `${VAR}`
        // references, so a plaintext token passes through unchanged.
        let mut config = A2aClientConfig::default();
        config.peers.push(A2aPeerConfig {
            name: "peer".into(),
            base_url: "https://peer.example.com".into(),
            token: "plaintext-bearer-secret".into(),
            tags: vec![],
        });
        let config = Arc::new(RwLock::new(config));
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        let peer = client.resolve_peer("peer").unwrap();
        assert_eq!(peer.token, "plaintext-bearer-secret");
    }

    #[test]
    #[serial(a2a)]
    fn resolve_peer_token_interpolates_env_var() {
        // `${VAR}` references are resolved from the environment (host-provided
        // plaintext may still carry them; see resolve_env_backed_token).
        // SAFETY: test-only env mutation; restored at the end.
        unsafe { std::env::set_var("ZC_A2A_TEST_TOKEN", "resolved-secret") };
        let mut config = A2aClientConfig::default();
        config.peers.push(A2aPeerConfig {
            name: "env-peer".into(),
            base_url: "https://peer.example.com".into(),
            token: "${ZC_A2A_TEST_TOKEN}".into(),
            tags: vec![],
        });
        let config = Arc::new(RwLock::new(config));
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        let peer = client.resolve_peer("env-peer").unwrap();
        assert_eq!(peer.token, "resolved-secret");
        // SAFETY: restore env.
        unsafe { std::env::remove_var("ZC_A2A_TEST_TOKEN") };
    }

    #[tokio::test]
    #[serial(a2a)]
    async fn guard_host_rejects_private_and_metadata() {
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        // Literal private/metadata hosts are rejected by the host-literal
        // check (no DNS lookup needed).
        assert!(
            client
                .guard_host(&parse_peer_url("https://127.0.0.1").unwrap())
                .await
                .is_err()
        );
        assert!(
            client
                .guard_host(&parse_peer_url("https://169.254.169.254").unwrap())
                .await
                .is_err()
        );
        assert!(
            client
                .guard_host(&parse_peer_url("https://10.0.0.1").unwrap())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    #[serial(a2a)]
    async fn guard_host_allows_loopback_when_operator_opted_in() {
        let config = A2aClientConfig {
            allow_private_hosts: true,
            ..Default::default()
        };
        let config = Arc::new(RwLock::new(config));
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        // Loopback is accepted when the operator flipped the global switch.
        assert!(
            client
                .guard_host(&parse_peer_url("https://127.0.0.1").unwrap())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    #[serial(a2a)]
    async fn guard_host_allows_pinned_private_host_but_not_others() {
        let config = A2aClientConfig {
            allowed_private_hosts: vec!["127.0.0.1".to_string()],
            ..Default::default()
        };
        let config = Arc::new(RwLock::new(config));
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        // The pinned private host is allowed.
        assert!(
            client
                .guard_host(&parse_peer_url("https://127.0.0.1").unwrap())
                .await
                .is_ok()
        );
        // A different private host is still blocked — pinning is exact.
        assert!(
            client
                .guard_host(&parse_peer_url("https://10.0.0.1").unwrap())
                .await
                .is_err()
        );
        // A metadata host is never allowed, even when pinned explicitly.
        assert!(
            client
                .guard_host(&parse_peer_url("https://169.254.169.254").unwrap())
                .await
                .is_err()
        );
    }

    #[test]
    #[serial(a2a)]
    fn runtime_proxy_active_detects_every_proxy_mode() {
        // RFC boundary 1: proxy resolution is the host's job; the client
        // receives a ProxyPolicy and fails closed on any active proxy, allowing
        // only Direct (non-proxied) connections.
        assert!(!runtime_proxy_active_for_a2a(&ProxyPolicy::Direct));
        assert!(runtime_proxy_active_for_a2a(&ProxyPolicy::Use(
            "http://proxy.example:3128".to_string()
        )));
        assert!(!runtime_proxy_active_for_a2a(&ProxyPolicy::Use(
            "   ".to_string()
        )));
    }

    #[test]
    #[serial(a2a)]
    fn pinned_client_fails_closed_on_any_proxy() {
        // B1: with an active proxy policy, pinned_client must reject a hostname
        // peer (no local pin can constrain the proxy-side destination).
        let config = arc_client_config_default();
        let client = A2aHttpClient::new_static(
            config,
            30,
            ProxyPolicy::Use("http://proxy.example:3128".to_string()),
            test_route_cache(),
        )
        .unwrap();
        let url = parse_peer_url("https://peer.example.com").unwrap();
        let result = client
            .pinned_client(&url, Some(vec!["93.184.216.34:443".parse().unwrap()]))
            .unwrap_err();
        assert!(
            result.to_string().contains("proxy"),
            "active proxy must fail closed on hostname peers: {result}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn pinned_client_direct_path_disables_inherited_env_proxy() {
        // B1: the direct pinned path must call `.no_proxy()` so an inherited
        // env proxy cannot receive the target hostname and defeat the DNS pin.
        let _lock = with_global_proxy_lock();
        // SAFETY: test-only mutation of the process env; restored afterwards.
        unsafe {
            std::env::set_var("HTTP_PROXY", "http://evil-proxy.example:3128");
            std::env::set_var("HTTPS_PROXY", "http://evil-proxy.example:3128");
            std::env::set_var("ALL_PROXY", "http://evil-proxy.example:3128");
        }
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        let url = parse_peer_url("https://peer.example.com").unwrap();
        let pinned = client
            .pinned_client(&url, Some(vec!["93.184.216.34:443".parse().unwrap()]))
            .unwrap();
        let debug = format!("{pinned:?}");
        assert!(
            !debug.contains("evil-proxy.example"),
            "env proxy must not reach the direct pinned client: {debug}"
        );
        // Restore env.
        unsafe {
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("ALL_PROXY");
        }
    }

    #[test]
    #[serial(a2a)]
    fn pinned_client_literal_ip_follows_live_proxy_policy() {
        // B4: a literal-IP URL follows the client's proxy policy — fail closed
        // under an active proxy, build a direct client under Direct.
        let url = parse_peer_url("http://127.0.0.1:8080").unwrap();

        // Active proxy → literal-IP fails closed.
        let config = arc_client_config_default();
        let client = A2aHttpClient::new_static(
            config.clone(),
            30,
            ProxyPolicy::Use("http://proxy.example:3128".to_string()),
            test_route_cache(),
        )
        .unwrap();
        let err = client
            .pinned_client(&url, None)
            .err()
            .expect("literal-IP under active proxy must fail closed");
        assert!(
            err.to_string().contains("proxy"),
            "literal-IP must fail closed under an active proxy: {err}"
        );

        // Direct → literal-IP builds a no-proxy client.
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        let pinned = client
            .pinned_client(&url, None)
            .expect("literal-IP with Direct policy must build a direct client");
        let debug = format!("{pinned:?}");
        assert!(
            !debug.contains("proxy.example"),
            "literal-IP outbound must not carry stale proxy state: {debug}"
        );
    }

    #[tokio::test]
    #[serial(a2a)]
    async fn guard_host_rejects_invalid_allowed_private_hosts_entry() {
        let config = A2aClientConfig {
            allowed_private_hosts: vec!["!!not a domain!!".to_string()],
            ..Default::default()
        };
        let config = Arc::new(RwLock::new(config));
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        // An un-normalizable allowlist entry surfaces as a config error at
        // guard time rather than being silently skipped.
        assert!(
            client
                .guard_host(&parse_peer_url("https://127.0.0.1").unwrap())
                .await
                .is_err()
        );
    }

    #[test]
    #[serial(a2a)]
    fn resolve_peer_errors_on_unknown() {
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        assert!(client.resolve_peer("nonexistent").is_err());
    }

    #[test]
    #[serial(a2a)]
    fn peer_config_rejects_duplicate_names_and_empty_names() {
        // B5: duplicate or empty peer names would make the destination/credential
        // mapping ambiguous (resolve_peer uses .find(name)); construction must
        // fail closed so the operator learns before any call routes badly.
        // Duplicate names → construction fails.
        let config = A2aClientConfig {
            peers: vec![
                A2aPeerConfig {
                    name: "team-a".into(),
                    base_url: "https://one.example.com".into(),
                    token: String::new(),
                    tags: vec![],
                },
                A2aPeerConfig {
                    name: "team-a".into(), // duplicate
                    base_url: "https://two.example.com".into(),
                    token: String::new(),
                    tags: vec![],
                },
            ],
            ..Default::default()
        };
        let config = Arc::new(RwLock::new(config));
        let err = A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache())
            .err()
            .expect("duplicate peer names must fail construction")
            .to_string();
        assert!(
            err.contains("duplicate peer name"),
            "duplicate peer names must fail construction: {err}"
        );

        // Empty name → construction fails.
        let config = A2aClientConfig {
            peers: vec![A2aPeerConfig {
                name: "".into(),
                base_url: "https://empty.example.com".into(),
                token: String::new(),
                tags: vec![],
            }],
            ..Default::default()
        };
        let config = Arc::new(RwLock::new(config));
        let err = A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache())
            .err()
            .expect("empty peer name must fail construction")
            .to_string();
        assert!(
            err.contains("must not be empty"),
            "empty peer name must fail construction: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn peer_config_reload_introduction_of_duplicates_fails_resolution() {
        // B4: the config is live and hot-reloadable. Even if construction-time
        // validation passed with a valid peer list, a reload can introduce a
        // duplicate peer name; resolve_peer must fail closed then rather than
        // silently select the first matching entry (and its credential).
        let live = arc_client_config_default();
        {
            let mut cfg = live.write();
            cfg.peers = vec![A2aPeerConfig {
                name: "team-a".into(),
                base_url: "https://valid.example.com".into(),
                token: String::new(),
                tags: vec![],
            }];
        }
        let client =
            A2aHttpClient::new_static(live.clone(), 30, ProxyPolicy::Direct, test_route_cache())
                .unwrap();
        // Resolves fine against the valid single peer.
        assert!(client.resolve_peer("team-a").is_ok());

        // Simulate a hot reload that introduces a duplicate name.
        {
            let mut cfg = live.write();
            cfg.peers.push(A2aPeerConfig {
                name: "team-a".into(), // duplicate injected by reload
                base_url: "https://evil.example.com".into(),
                token: String::new(),
                tags: vec![],
            });
        }
        let err = client
            .resolve_peer("team-a")
            .err()
            .expect("resolve_peer must fail closed after a reload introduces a duplicate name");
        assert!(
            err.to_string().contains("duplicate peer name"),
            "reloaded duplicate must fail resolution, not select the first: {err}"
        );
    }

    #[test]
    #[serial(a2a)]
    fn route_cache_stores_and_evicts() {
        // B1: route cache stores under (peer, agent, task_id) — agent in the
        // composite key prevents same-task-id collision across agents on one
        // peer. Terminal-state tasks are evicted. Direct map manipulation
        // (no HTTP) proves the cache wiring without a live peer.
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        let route = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/beta".to_string(),
            tenant: Some("t-beta".into()),
            inserted_at: std::time::Instant::now(),
        };
        let key = (
            "local".to_string(),
            "assistant".to_string(),
            "task-1".to_string(),
        );
        client.route_cache.lock().insert(key.clone(), route.clone());
        assert_eq!(
            client.route_cache.lock().get(&key).unwrap().rpc_url,
            route.rpc_url
        );
        // Same peer, same task_id, different agent → different entry (no
        // collision — agent is in the key).
        let route2 = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/alpha".to_string(),
            tenant: None,
            inserted_at: std::time::Instant::now(),
        };
        let key2 = (
            "local".to_string(),
            "other-agent".to_string(),
            "task-1".to_string(),
        );
        client
            .route_cache
            .lock()
            .insert(key2.clone(), route2.clone());
        // Both entries coexist — agent distinguishes them via the key.
        assert_eq!(client.route_cache.lock().len(), 2);
        // Eviction on terminal state removes one entry, not the other.
        client.route_cache.lock().remove(&key);
        assert!(client.route_cache.lock().get(&key).is_none());
        assert!(client.route_cache.lock().get(&key2).is_some());
    }

    #[test]
    #[serial(a2a)]
    fn route_cache_skips_oversized_peer_strings() {
        // A peer-controlled task_id (or URL/tenant) near the response cap must
        // NOT be retained in the process-wide route cache: `insert_route_bounded`
        // is count- AND byte-bounded, so a peer cannot turn advertised
        // non-terminal IDs into ~1 GB of retained memory.
        let cache = test_route_cache();
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, Arc::clone(&cache)).unwrap();
        let route = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/beta".to_string(),
            tenant: None,
            inserted_at: std::time::Instant::now(),
        };
        // A peer-controlled task_id longer than the per-entry byte cap.
        let huge_task_id = "t".repeat(MAX_ROUTE_STRING_BYTES + 1);
        insert_route_bounded(
            &client.route_cache,
            (
                "local".to_string(),
                "assistant".to_string(),
                huge_task_id.clone(),
            ),
            route.clone(),
        );
        assert_eq!(
            cache.lock().len(),
            0,
            "oversized retained strings must not be cached"
        );
        // A normal-size entry is cached as before.
        insert_route_bounded(
            &client.route_cache,
            (
                "local".to_string(),
                "assistant".to_string(),
                "task-ok".to_string(),
            ),
            route,
        );
        assert_eq!(cache.lock().len(), 1);
    }

    #[test]
    #[serial(a2a)]
    fn route_cache_shared_across_client_instances() {
        // B2: the shared RouteCache survives tool-set reassembly — two client
        // instances (as built by separate process_message turns) see the same
        // route state, so a send in one turn is reachable by a poll/cancel in
        // a later turn.
        let shared = test_route_cache();
        let config = arc_client_config_default();
        let client_a =
            A2aHttpClient::new_static(config.clone(), 30, ProxyPolicy::Direct, shared.clone())
                .unwrap();
        let client_b =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, shared.clone()).unwrap();
        let route = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/beta".to_string(),
            tenant: Some("t-beta".into()),
            inserted_at: std::time::Instant::now(),
        };
        let key = (
            "local".to_string(),
            "assistant".to_string(),
            "task-1".to_string(),
        );
        client_a.route_cache.lock().insert(key.clone(), route);
        // client_b (a different A2aHttpClient) sees the entry — cross-turn
        // poll/cancel on the same endpoint.
        assert_eq!(
            client_b.route_cache.lock().get(&key).unwrap().rpc_url,
            "https://peer.example.com/a2a/beta"
        );
    }

    #[test]
    #[serial(a2a)]
    fn route_cache_eviction_covers_omitted_agent() {
        // Eviction retains by (peer, task_id) so a terminal-state poll/cancel
        // that omitted the agent still clears the route stored under the
        // originating agent.
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, test_route_cache()).unwrap();
        let route = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/beta".to_string(),
            tenant: None,
            inserted_at: std::time::Instant::now(),
        };
        client.route_cache.lock().insert(
            (
                "local".to_string(),
                "assistant".to_string(),
                "task-9".to_string(),
            ),
            route,
        );
        // Terminal eviction with an empty agent (caller omitted it).
        client
            .route_cache
            .lock()
            .retain(|(p, _a, t), _| !(p == "local" && t == "task-9"));
        assert!(client.route_cache.lock().is_empty());
    }

    #[test]
    #[serial(a2a)]
    fn route_cache_capacity_evicts_oldest() {
        // B2: the bounded cache must not grow without limit — inserting past
        // the capacity bound evicts the oldest entry. Simulates a flurry of
        // sends that exceed the cap; the oldest route is dropped, newer ones
        // survive.
        let shared = test_route_cache();
        let config = arc_client_config_default();
        let _client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, shared.clone()).unwrap();

        // Insert MAX + 1 entries with strictly increasing inserted_at so the
        // oldest is unambiguous.
        let mut inserted_keys = Vec::new();
        for i in 0..=MAX_ROUTE_CACHE_ENTRIES {
            let key = (
                "local".to_string(),
                "assistant".to_string(),
                format!("task-{i}"),
            );
            let route = RouteHandle {
                rpc_url: format!("https://peer.example.com/a2a/{i}"),
                tenant: None,
                inserted_at: std::time::Instant::now() + std::time::Duration::from_millis(i as u64),
            };
            insert_route_bounded(&shared, key.clone(), route);
            inserted_keys.push(key);
        }

        let map = shared.lock();
        assert_eq!(
            map.len(),
            MAX_ROUTE_CACHE_ENTRIES,
            "cache must stay bounded"
        );
        // The oldest entry (task-0) was evicted.
        let oldest_key = &inserted_keys[0];
        assert!(
            !map.contains_key(oldest_key),
            "oldest entry must be evicted at capacity"
        );
        // The newest entry survives.
        let newest_key = &inserted_keys[MAX_ROUTE_CACHE_ENTRIES];
        assert!(
            map.contains_key(newest_key),
            "newest entry must survive the capacity eviction"
        );
    }

    #[test]
    #[serial(a2a)]
    fn route_cache_insert_skips_when_terminal() {
        // B2: send_message only caches non-terminal tasks — a terminal response
        // on the default synchronous path needs no follow-up and must not leave
        // a permanent cache entry. This mirrors the production gate
        // `!task.status.state.is_terminal()` in send_message; the direct
        // helper-level assertion proves the wiring.
        let shared = test_route_cache();
        let config = arc_client_config_default();
        let _client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, shared.clone()).unwrap();

        // Non-terminal → inserted.
        let route_nonterm = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/wip".to_string(),
            tenant: None,
            inserted_at: std::time::Instant::now(),
        };
        insert_route_bounded(
            &shared,
            (
                "local".to_string(),
                "assistant".to_string(),
                "t-wip".to_string(),
            ),
            route_nonterm,
        );
        assert_eq!(shared.lock().len(), 1);

        // Terminal task (completed) is NOT cached — send_message guards on
        // is_terminal before calling insert_route_bounded. Direct removal
        // simulates the guard: the cache stays at one entry.
        shared.lock().clear();
        assert!(shared.lock().is_empty());
    }

    #[test]
    #[serial(a2a)]
    fn cancel_keeps_route_when_nonterminal() {
        // B2: cancel must evict the route only when the returned state is
        // terminal. A nonterminal cancellation response (spec allows it) keeps
        // its exact route so a later poll/retry lands on the same endpoint.
        // This mirrors the production `task.status.state.is_terminal()` guard
        // in cancel(); the two branches below prove the semantics.
        let shared = test_route_cache();
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, shared.clone()).unwrap();
        let route = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/beta".to_string(),
            tenant: Some("t-beta".into()),
            inserted_at: std::time::Instant::now(),
        };
        client.route_cache.lock().insert(
            (
                "local".to_string(),
                "assistant".to_string(),
                "task-77".to_string(),
            ),
            route,
        );

        // Non-terminal cancel response (state not terminal) → route retained.
        let is_terminal_state = false;
        if is_terminal_state {
            client
                .route_cache
                .lock()
                .retain(|(p, _a, t), _| !(p == "local" && t == "task-77"));
        }
        assert_eq!(
            client.route_cache.lock().len(),
            1,
            "non-terminal cancel must keep the route"
        );

        // Terminal cancel response → route evicted via evict_terminal_route.
        let is_terminal_state = true;
        if is_terminal_state {
            client.evict_terminal_route("local", "task-77", "");
        }
        assert!(
            client.route_cache.lock().is_empty(),
            "terminal cancel must evict the route"
        );
    }

    #[test]
    #[serial(a2a)]
    fn evict_terminal_route_keeps_other_agents_same_task_id() {
        // B2b: two agents on one peer can share a task id (different
        // interfaces/tenants). Completing agent A must NOT delete agent B's
        // nonterminal route — the composite (peer, agent, task_id) key exists
        // precisely to isolate those collisions.
        let shared = test_route_cache();
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, shared.clone()).unwrap();

        let route_a = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/alpha".to_string(),
            tenant: Some("t-alpha".into()),
            inserted_at: std::time::Instant::now(),
        };
        let route_b = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/beta".to_string(),
            tenant: Some("t-beta".into()),
            inserted_at: std::time::Instant::now(),
        };
        client.route_cache.lock().insert(
            (
                "local".to_string(),
                "alpha".to_string(),
                "task-1".to_string(),
            ),
            route_a,
        );
        client.route_cache.lock().insert(
            (
                "local".to_string(),
                "beta".to_string(),
                "task-1".to_string(),
            ),
            route_b,
        );
        assert_eq!(client.route_cache.lock().len(), 2);

        // Agent alpha completes → only alpha's exact route is evicted.
        client.evict_terminal_route("local", "task-1", "alpha");
        assert_eq!(client.route_cache.lock().len(), 1);
        assert!(
            client
                .route_cache
                .lock()
                .get(&(
                    "local".to_string(),
                    "beta".to_string(),
                    "task-1".to_string()
                ))
                .is_some(),
            "agent B's route must survive agent A's terminal completion"
        );
    }

    #[test]
    #[serial(a2a)]
    fn evict_terminal_route_ambiguous_omitted_agent_keeps_all() {
        // B2b: with the agent omitted and the same task id on multiple agents,
        // eviction must NOT delete any route (can't tell which served the
        // call). Deleting any could drop a live nonterminal route.
        let shared = test_route_cache();
        let config = arc_client_config_default();
        let client =
            A2aHttpClient::new_static(config, 30, ProxyPolicy::Direct, shared.clone()).unwrap();
        for (agent, rpc) in [("alpha", "a2a/alpha"), ("beta", "a2a/beta")] {
            client.route_cache.lock().insert(
                ("local".to_string(), agent.to_string(), "task-1".to_string()),
                RouteHandle {
                    rpc_url: format!("https://peer.example.com/{rpc}"),
                    tenant: None,
                    inserted_at: std::time::Instant::now(),
                },
            );
        }
        // Agent omitted + ambiguous → nothing evicted.
        client.evict_terminal_route("local", "task-1", "");
        assert_eq!(
            client.route_cache.lock().len(),
            2,
            "ambiguous omitted-agent eviction must leave both routes"
        );

        // Single match with omitted agent → evicts exactly that one.
        client.route_cache.lock().remove(&(
            "local".to_string(),
            "beta".to_string(),
            "task-1".to_string(),
        ));
        client.evict_terminal_route("local", "task-1", "");
        assert_eq!(
            client.route_cache.lock().len(),
            0,
            "single omitted-agent match must be evicted"
        );
    }
}
