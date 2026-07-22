//! A2A outbound client (caller role): the `a2a_*` tools that delegate tasks
//! to remote A2A-compliant agents.
//!
//! Lives in `zeroclaw-tools` as a sibling to `channel_room` / `http_request` /
//! `git_forge`, so runtime tool registration depends only on `zeroclaw-tools`
//! (the gateway stays the inbound/server edge). The A2A wire types are shared
//! with the inbound surface via [`zeroclaw_api::a2a_wire`].
//!
//! Per the A2ATool RFC: no copied peer
//! `Vec` is stored in the client or tool handle. Peer definitions, credentials,
//! and security policy resolve from canonical live `Config` at call time. The
//! Agent Card cache is derived data keyed by endpoint, invalidated when the
//! endpoint changes.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::TryStreamExt;
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};

use zeroclaw_api::a2a_wire::{
    AgentCard, AgentInterface, JsonRpcResponse, Role, SendMessageParams, SendMessageResponse, Task,
    rpc_result,
};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_api::tool_attribution;
use zeroclaw_config::schema::Config;

#[cfg(test)]
use zeroclaw_config::multi_agent::A2aClientPeerConfig;
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

use crate::helpers::domain_guard::{is_cloud_metadata_ip, is_private_or_local_host};

/// A2A protocol version sent on every request (spec §3.2 `A2A-Version` header).
const A2A_VERSION: &str = "1.0";

/// Live config handle: shared `Arc<RwLock<Config>>` so the client reads the
/// canonical, hot-reloadable config at call time rather than a startup snapshot.
type LiveConfig = Arc<RwLock<Config>>;

/// One peer resolved at call time from `[a2a.client.peers]`. The token is the
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
    http: reqwest::Client,
    /// Request timeout applied to every outbound call (mirrors the shared
    /// `http` builder); retained as a plain `Duration` so the per-request
    /// pinned client can re-apply it without needing a getter on
    /// `reqwest::Client`.
    request_timeout: std::time::Duration,
    config: LiveConfig,
    /// Config file parent dir (the zeroclaw data dir), used to locate the
    /// `SecretStore` for decrypting encrypted peer tokens. `None` when the
    /// client is built without a config path (tests) — encrypted tokens then
    /// fail clearly rather than silently using ciphertext.
    zeroclaw_dir: Option<std::path::PathBuf>,
    /// Whether secret encryption is enabled at runtime; passed to
    /// `SecretStore::new` when decrypting peer tokens.
    secrets_encrypt: bool,
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
    route_cache: Mutex<HashMap<(String, String, String), RouteHandle>>,
}

impl A2aHttpClient {
    /// Build the client. The reqwest client is built once (timeout + no-redirect
    /// policy); peers resolve from live config per call, so nothing about peers
    /// is eagerly validated here. `zeroclaw_dir` + `secrets_encrypt` enable
    /// decrypting encrypted peer tokens via the canonical `SecretStore` (the
    /// same path `http_request` uses).
    pub fn new(
        config: LiveConfig,
        request_timeout_secs: u64,
        zeroclaw_dir: Option<std::path::PathBuf>,
        secrets_encrypt: bool,
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
        let builder = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(std::time::Duration::from_secs(10))
            // No redirects: a peer that 3xx-redirects to an internal host would
            // otherwise be followed into the private network (SSRF).
            .redirect(reqwest::redirect::Policy::none());
        // Apply the runtime proxy policy so literal-IP peers (which reuse this
        // shared client via `pinned_client`'s no-resolution branch) honor the
        // same operator proxy scope as other ZeroClaw-managed HTTP traffic.
        let builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.a2a");
        let http = builder.build()?;
        Ok(Self {
            http,
            request_timeout: timeout,
            config,
            zeroclaw_dir,
            secrets_encrypt,
            card_cache: Mutex::new(HashMap::new()),
            route_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Resolve a single peer by name from the live config at call time. The
    /// read lock is held only to clone the raw peer fields; token resolution
    /// (decrypt + env interpolation) happens after the lock drops so a slow
    /// `SecretStore`/`std::env::var` can't block config hot-reload writers.
    fn resolve_peer(&self, peer: &str) -> anyhow::Result<ResolvedPeer> {
        let (base_url, raw_token) = {
            let config = self.config.read();
            let p = config
                .a2a
                .client
                .peers
                .iter()
                .find(|p| p.name == peer)
                .ok_or_else(|| anyhow::Error::msg(format!("a2a client: unknown peer '{peer}'")))?;
            (
                p.base_url.trim_end_matches('/').to_string(),
                p.token.clone(),
            )
        };
        Ok(ResolvedPeer {
            base_url,
            token: self.resolve_peer_token(&raw_token)?,
        })
    }

    /// Resolve a peer's raw token field to the credential value sent as
    /// `Authorization: Bearer`. Mirrors `http_request`'s canonical secret
    /// path (`http_request.rs:278-295`): an encrypted value (per
    /// `SecretStore::is_encrypted`) is decrypted via the `SecretStore` rooted
    /// at the zeroclaw dir; the resulting literal (or a plain literal) is then
    /// passed through the `${VAR}` env resolver so an env-backed token is
    /// resolved at call time. Empty resolves to empty (anonymous peer, no
    /// Authorization header).
    fn resolve_peer_token(&self, raw: &str) -> anyhow::Result<String> {
        if raw.is_empty() {
            return Ok(String::new());
        }
        // Step 1: decrypt if the value is encrypted ciphertext (mirrors
        // http_request.rs:278-289).
        let literal = if zeroclaw_config::secrets::SecretStore::is_encrypted(raw) {
            let Some(dir) = self.zeroclaw_dir.as_deref() else {
                anyhow::bail!(
                    "a2a client: peer token is encrypted but the client has no zeroclaw dir \
                     (config path) to locate the SecretStore"
                );
            };
            let store = zeroclaw_config::secrets::SecretStore::new(dir, self.secrets_encrypt);
            let plaintext = store.decrypt(raw)?;
            if plaintext.is_empty() {
                anyhow::bail!("a2a client: peer token is empty after decryption");
            }
            plaintext
        } else {
            raw.to_string()
        };
        // Step 2: resolve `${VAR}` env references on the (possibly decrypted)
        // literal, mirroring http_request's resolve_env_backed_auth_secret.
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
            let config = self.config.read();
            let client = &config.a2a.client;
            (
                client.allow_private_hosts,
                client.allowed_private_hosts.clone(),
            )
        };
        // Normalize once per call; invalid entries surface as a config error
        // rather than a silent skip (mirrors `http_request` construction).
        let allowed = crate::helpers::domain_guard::normalize_allowed_domains(
            allowed_private_hosts,
            "a2a.client.allowed_private_hosts",
        )?;

        // (1) Cloud-metadata hosts are always blocked, even when private hosts
        // are allowed — a peer must never reach the IMDS endpoint.
        if host
            .parse::<std::net::IpAddr>()
            .is_ok_and(is_cloud_metadata_ip)
        {
            anyhow::bail!(
                "a2a client: peer URL host '{host}' is a cloud metadata address and not allowed"
            );
        }

        let private_host = is_private_or_local_host(&host);
        let private_host_explicitly_allowed =
            private_host && crate::helpers::domain_guard::host_matches_allowlist(&host, &allowed);
        if private_host && !private_host_explicitly_allowed && !allow_private {
            anyhow::bail!(
                "a2a client: peer URL host '{host}' is private/loopback and not allowed (set a2a.client.allow_private_hosts or a2a.client.allowed_private_hosts)"
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
                    anyhow::Error::msg(format!(
                        "a2a client: failed to resolve peer host '{host}': {e}"
                    ))
                })?
                .collect::<Vec<_>>();
            let ips = resolved.iter().map(|a| a.ip()).collect::<Vec<_>>();
            let private_resolution_allowed = allow_private || private_host_explicitly_allowed;
            if private_resolution_allowed {
                crate::helpers::domain_guard::validate_resolved_ips_exclude_metadata(&host, &ips)?;
            } else {
                crate::helpers::domain_guard::validate_resolved_ips_are_public(&host, &ips)?;
            }
            return Ok(Some(resolved));
        }
        Ok(None)
    }

    /// Build a per-request reqwest client that pins the validated DNS
    /// resolution for `url`, so the actual connection cannot be rebound to a
    /// different (private/metadata) IP after the SSRF check passed. Mirrors
    /// `http_request`'s `resolve_to_addrs` posture. For a literal-IP host (no
    /// resolution), the shared `self.http` client is returned — the literal
    /// IP is already pinned by construction.
    fn pinned_client(
        &self,
        url: &reqwest::Url,
        resolved: Option<Vec<std::net::SocketAddr>>,
    ) -> anyhow::Result<reqwest::Client> {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::Error::msg("a2a client: URL must include a host"))?;
        let Some(addrs) = resolved else {
            // Literal IP (or no resolution): shared client is safe.
            return Ok(self.http.clone());
        };
        // Clone the shared builder settings (timeout, no-redirect, runtime
        // proxy) and add the IP pin. The runtime proxy builder mirrors
        // http_request (http_request.rs:354-359) so A2A traffic honors the
        // same operator proxy scope as other ZeroClaw-managed HTTP, while
        // the IP pin still closes the DNS-rebinding gap. A new client per
        // call is the price of pinning the specific endpoint's resolved
        // addresses.
        let builder = reqwest::Client::builder()
            .timeout(self.request_timeout)
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none());
        let builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.a2a");
        Ok(builder.resolve_to_addrs(host, &addrs).build()?)
    }
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
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| anyhow::Error::msg(format!("a2a client: invalid URL '{url}': {e}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        anyhow::bail!(
            "a2a client: URL must be http:// or https:// (got '{}')",
            parsed.scheme()
        );
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
    match url.port() {
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
fn message_to_task(message: zeroclaw_api::a2a_wire::Message) -> (Task, bool) {
    let msg_id = message.message_id.clone();
    let text = message
        .parts
        .iter()
        .filter_map(|p| p.as_text().map(|s| s.to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    let artifact = zeroclaw_api::a2a_wire::Artifact {
        // Derive from the peer-provided messageId, not a random UUID.
        artifact_id: format!("{msg_id}-artifact"),
        name: None,
        description: None,
        parts: vec![zeroclaw_api::a2a_wire::Part::text_str(text)],
    };
    let task = Task {
        // Reuse the peer-provided messageId — not an invented UUID.
        id: msg_id,
        context_id: message.context_id.clone().unwrap_or_default(),
        status: zeroclaw_api::a2a_wire::TaskStatus {
            state: zeroclaw_api::a2a_wire::TaskState::TaskStateCompleted,
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
/// to repeat the call on the same endpoint.
#[derive(Clone)]
struct RouteHandle {
    rpc_url: String,
    tenant: Option<String>,
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
/// - **multiple candidates**: prefer one whose URL targets the requested
///   agent (trailing `/a2a/{agent}` segment, the ZeroClaw catalog
///   convention); if no URL matches, still select the first candidate
///   (the caller's agent was an opaque identifier, not a URL suffix) —
///   multi-tenant peers distinguish agents via `tenant`, not URL;
/// - if no card (card-less peer), fall back to `<base>/a2a/{agent}` (MVP
///   card-less shape — the peer hasn't served discovery yet).
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
        // Single compatible interface: select regardless of URL shape. A
        // standard A2A card uses arbitrary URLs (e.g. /a2a/v1) with no
        // agent alias — agent is an opaque identifier, not a URL suffix.
        if v1_candidates.len() == 1 {
            return Ok(endpoint_from(v1_candidates[0]));
        }
        // Multiple v1 candidates (aggregate card, e.g. ZeroClaw catalog).
        // Try to match the agent as a URL suffix first (ZeroClaw
        // convention: each alias is a distinct /a2a/{alias} interface).
        // If no URL match, still select the first candidate — agent is an
        // opaque identifier and multi-tenant peers use `tenant`, not URL
        // suffix, to distinguish agents.
        if !agent.is_empty() {
            let agent_segment = format!("/a2a/{}", percent_encode_path_segment(agent));
            if let Some(iface) = v1_candidates
                .iter()
                .find(|i| i.url.ends_with(&agent_segment))
            {
                return Ok(endpoint_from(iface));
            }
        }
        // agent empty or no URL match: take the first v1 candidate.
        return Ok(endpoint_from(v1_candidates[0]));
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
        // Transport selection (spec §5.2): read supportedInterfaces from the
        // peer's cached card, matching the requested agent; else fall back to
        // the conventional /a2a/{agent} path. Card fetch is cached by endpoint.
        let card = self
            .card_for_endpoint_with(&peer_ref, &base_http)
            .await
            .ok();
        let endpoint = select_interface(card.as_ref(), &base, agent)?;
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
            message: zeroclaw_api::a2a_wire::Message {
                message_id: uuid_v4(),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                role: Role::RoleUser,
                parts: vec![zeroclaw_api::a2a_wire::Part::text_str(message)],
                metadata: None,
                extensions: vec![],
                reference_task_ids: vec![],
            },
            configuration: Some(zeroclaw_api::a2a_wire::SendMessageConfiguration {
                accepted_output_modes: vec![],
                history_length: None,
                return_immediately,
            }),
            metadata: None,
        };
        let resp: JsonRpcResponse<SendMessageResponse> = self
            .post_jsonrpc(
                &http,
                &rpc,
                "SendMessage",
                serde_json::to_value(&params)?,
                &peer_ref,
                &base,
            )
            .await?;
        let payload = rpc_result(resp)?;
        // Unwrap the oneof: a Task branch returns the task for the tool layer
        // (poll/cancel on non-terminal states); a Message branch is surfaced
        // as a synthetic completed Task carrying the reply text as an artifact,
        // with `was_task_branch=false` so the tool output preserves the
        // response distinction.
        let (task, was_task_branch) = match payload {
            SendMessageResponse::Task { task } => (task, true),
            SendMessageResponse::Message { message } => message_to_task(message),
        };
        // Cache the route keyed by (peer, task_id) so a later GetTask/CancelTask
        // reuses this exact endpoint (peer/agent/rpc_url/tenant) instead of
        // re-selecting an interface that could route to a different agent. The
        // composite key prevents task-id collisions across peers. Only the
        // Task branch carries a real, peer-assigned task_id; Message replies
        // are synchronous (no poll needed), so they are not cached.
        if was_task_branch {
            let route = RouteHandle {
                rpc_url: endpoint.url.clone(),
                tenant: endpoint.tenant.clone(),
            };
            self.route_cache.lock().insert(
                (peer.to_string(), agent.to_string(), task.id.clone()),
                route,
            );
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
        let resp: JsonRpcResponse<Task> = self
            .post_jsonrpc(&http, &rpc, "GetTask", params, &peer_ref, &base)
            .await?;
        let task = rpc_result(resp)?;
        // Evict the route cache entry when the task reaches a terminal
        // state (no further poll/cancel will reference it).
        if task.status.state.is_terminal() {
            self.route_cache.lock().remove(&(
                peer.to_string(),
                agent.unwrap_or("").to_string(),
                task_id.to_string(),
            ));
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
        let resp: JsonRpcResponse<Task> = self
            .post_jsonrpc(&http, &rpc, "CancelTask", params, &peer_ref, &base)
            .await?;
        let task = rpc_result(resp)?;
        // Cancel always reaches a terminal (canceled) state; evict the route.
        self.route_cache.lock().remove(&(
            peer.to_string(),
            agent.unwrap_or("").to_string(),
            task_id.to_string(),
        ));
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
        // Route cache hit: lookup by (peer, agent, task_id) so a same-task-id
        // collision across agents on one peer cannot overwrite another agent's
        // route. The agent in the key ensures poll/cancel reach the creating
        // endpoint, not the first-matching entry.
        if !agent.is_empty()
            && let Some(route) = self
                .route_cache
                .lock()
                .get(&(peer.to_string(), agent.to_string(), task_id.to_string()))
                .cloned()
        {
            let rpc = parse_peer_url(&route.rpc_url)?;
            return Ok((rpc, route.tenant, base, peer_ref, base_http));
        }
        // Miss (or no agent provided): fetch the card to determine whether a
        // safe fallback exists.
        let card = self
            .card_for_endpoint_with(&peer_ref, &base_http)
            .await
            .ok();
        // When agent is non-empty, delegate to select_interface which now
        // requires an exact agent match (rejects unknown agents on aggregate
        // cards). An empty agent means the caller doesn't know which agent
        // created the task (poll/cancel miss); only allow single-interface
        // cards for that case.
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
        self.card_for_endpoint_with(&peer_ref, &http).await
    }

    /// Fetch a peer's Agent Card, spec-first with a ZeroClaw-inbound fallback.
    ///
    /// Tries the spec §14.3 well-known root path `/.well-known/agent-card.json`
    /// first — the single-agent root card every standard A2A server serves,
    /// and the discovery card a spec-compliant ZeroClaw inbound serves once
    /// the discovery-card-on-spec-root design is fully landed. When that path
    /// is absent (HTTP 404, or a 2xx body that isn't valid JSON — e.g. a
    /// gateway SPA fallback), falls back to `/.well-known/agents-card.json`,
    /// the aggregate catalog card the ZeroClaw inbound serves today (the
    /// inbound moved discovery off the spec root path onto the plural catalog
    /// path; the planned design rejects a separate catalog object type and
    /// puts the discovery card back on the spec root).
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
    ) -> anyhow::Result<AgentCard> {
        let spec_url = format!("{base_url}/.well-known/agent-card.json");
        // A 2xx body that fails JSON decode is the ZeroClaw-inbound case (the
        // spec root path 404s to a gateway SPA / HTML today), so try spec-first
        // and fall back only on an HTTP-level miss or a decode failure.
        if let Ok(card) = self.try_card_url(http, &spec_url).await {
            return Ok(card);
        }
        let catalog_url = format!("{base_url}/.well-known/agents-card.json");
        self.try_card_url(http, &catalog_url).await.map_err(|e| {
            anyhow::Error::msg(format!(
                "a2a client: could not fetch Agent Card from peer '{base_url}' \
                 (tried spec path {spec_url} then ZeroClaw catalog {catalog_url}): {e}"
            ))
        })
    }

    /// GET a card URL and decode it as `AgentCard`. Returns an error when the
    /// endpoint is missing (non-success status) or the body is not a valid
    /// Agent Card JSON — both signal "try the fallback path". The caller
    /// passes the reqwest client pinned to the validated base_url resolution.
    async fn try_card_url(&self, http: &reqwest::Client, url: &str) -> anyhow::Result<AgentCard> {
        let resp = http
            .get(url)
            .header("A2A-Version", A2A_VERSION)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("HTTP {status}");
        }
        // Bounded read before deserialization (mirrors post_jsonrpc).
        let max_bytes = self.limited_response_bytes();
        let body = Self::read_limited_bytes(resp, max_bytes).await?;
        Ok(serde_json::from_slice::<AgentCard>(&body)?)
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
    ) -> anyhow::Result<AgentCard> {
        let ttl_secs = self.config.read().a2a.client.card_cache_ttl_secs;
        if ttl_secs > 0 {
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
        }
        let card = self.fetch_card(http, &peer.base_url).await?;
        if ttl_secs > 0 {
            self.card_cache.lock().insert(
                peer.base_url.clone(),
                (card.clone(), std::time::Instant::now()),
            );
        }
        Ok(card)
    }

    /// POST a JSON-RPC 2.0 request envelope and deserialize the response. The
    /// caller passes the reqwest client (pinned to the validated DNS
    /// resolution) and the canonical RPC URL. The call is bound to the
    /// authorized peer origin (`base`): a peer's Agent Card can advertise an
    /// arbitrary RPC URL, so the card must not redirect the task — with or
    /// without a bearer token — to an attacker origin or downgrade to
    /// plaintext http. Mismatched origins fail clearly rather than send (DNS
    /// safety is not credential authorization). The bearer token (when
    /// non-empty, `${VAR}`-resolved, never logged) is attached only after the
    /// origin check passes.
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
            anyhow::bail!(
                "a2a client: peer advertised RPC URL '{}' whose origin does not match the \
                 authorized peer origin '{}'; refusing to send (set the peer's RPC interface \
                 to the same origin as [a2a.client.peers] base_url)",
                rpc,
                authorized_origin
            );
        }
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
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
        let resp = req.send().await?;
        let status = resp.status();
        let max_bytes = self.limited_response_bytes();
        if !status.is_success() {
            // Limit error-body reads too: a compromised peer could stream a
            // huge error body to exhaust memory.
            let bytes = Self::read_limited_bytes(resp, max_bytes).await?;
            let text = String::from_utf8_lossy(&bytes).to_string();
            anyhow::bail!("a2a client: peer returned HTTP {status}: {text}");
        }
        // Bounded body read before deserialization: a malicious peer cannot
        // force unbounded `json()` parsing of a multi-GB body.
        let bytes = Self::read_limited_bytes(resp, max_bytes).await?;
        Ok(serde_json::from_slice::<T>(&bytes)?)
    }

    /// Live-read `a2a.client.max_response_bytes` from config (0 = unlimited).
    /// Read per call so a hot-reloaded limit takes effect immediately.
    fn limited_response_bytes(&self) -> Option<usize> {
        let max = self.config.read().a2a.client.max_response_bytes;
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
            buf.extend_from_slice(&chunk);
            if buf.len() > max {
                anyhow::bail!(
                    "a2a client: peer response exceeded max_response_bytes ({max} bytes); rejected before deserialization"
                );
            }
        }
        Ok(buf)
    }
}

// ── Tool surface (4 independent tools) ─────────────────────────────

/// `a2a_discover` — list available peer agents and their capabilities.
pub struct A2aDiscoverTool {
    client: Arc<A2aHttpClient>,
    security: Arc<SecurityPolicy>,
}
/// `a2a_send` — delegate a task to a peer agent (blocking on the result).
pub struct A2aSendTool {
    client: Arc<A2aHttpClient>,
    security: Arc<SecurityPolicy>,
}
/// `a2a_get_task` — retrieve the state/artifacts of an in-flight task.
pub struct A2aGetTaskTool {
    client: Arc<A2aHttpClient>,
    security: Arc<SecurityPolicy>,
}
/// `a2a_cancel` — cancel an in-flight task.
pub struct A2aCancelTool {
    client: Arc<A2aHttpClient>,
    security: Arc<SecurityPolicy>,
}

impl A2aDiscoverTool {
    pub fn new(client: Arc<A2aHttpClient>, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}
impl A2aSendTool {
    pub fn new(client: Arc<A2aHttpClient>, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}
impl A2aGetTaskTool {
    pub fn new(client: Arc<A2aHttpClient>, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}
impl A2aCancelTool {
    pub fn new(client: Arc<A2aHttpClient>, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}

tool_attribution!(A2aDiscoverTool, zeroclaw_api::attribution::ToolKind::A2a);
tool_attribution!(A2aSendTool, zeroclaw_api::attribution::ToolKind::A2a);
tool_attribution!(A2aGetTaskTool, zeroclaw_api::attribution::ToolKind::A2a);
tool_attribution!(A2aCancelTool, zeroclaw_api::attribution::ToolKind::A2a);

/// Extract a required string argument, or surface a clear error naming the
/// missing field.
fn require_str(args: &Value, field: &str) -> anyhow::Result<String> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::Error::msg(format!("a2a: missing required argument '{field}'")))
}

/// Render a peer response into the structured `ToolOutput::json` the agent
/// sees. Peer artifact text is fenced as `untrusted-external` (cross-agent
/// prompt-injection surface: the reply comes from an agent under someone
/// else's control). The `text` field carries the fenced block so the model
/// sees the provenance marker, mirroring mcp_context's `trust` convention.
/// `was_task_branch` distinguishes a genuine A2A `Task` response from a
/// `Message` reply folded into a synthetic task — the model sees the
/// distinction via a `response_type` field. `agent` is carried in the output
/// so the model can pass it to follow-up get_task/cancel calls for correct
/// route lookup.
fn task_to_output(task: &Task, was_task_branch: bool, agent: &str) -> ToolResult {
    let artifacts: Vec<_> = task
        .artifacts
        .iter()
        .map(|a| {
            let raw_text: String = a
                .parts
                .iter()
                .filter_map(|p| p.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            let fenced =
                format!("<a2a-artifact trust=\"untrusted-external\">\n{raw_text}\n</a2a-artifact>");
            json!({
                "artifact_id": a.artifact_id,
                "text": fenced,
            })
        })
        .collect();
    let data = json!({
        "task_id": task.id,
        "state": task.status.state,
        "context_id": task.context_id,
        "response_type": if was_task_branch { "task" } else { "message" },
        "agent": agent,
        "artifacts": artifacts,
    });
    ToolResult::ok(ToolOutput::json(data))
}

#[async_trait]
impl Tool for A2aDiscoverTool {
    fn name(&self) -> &str {
        "a2a_discover"
    }
    fn description(&self) -> &str {
        "List available remote A2A peer agents and their advertised capabilities. \
         Call with no peer to list all configured peers, or a specific peer to fetch \
         its Agent Card (name, description, skills). Use before a2a_send to find the \
         right peer and agent for a task."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "peer": { "type": "string", "description": "Peer name to fetch the Agent Card for. Omit to list all configured peers." },
                "filter_tags": { "type": "array", "items": { "type": "string" }, "description": "Optional tags to filter peers by (e.g. [\"production\"])." }
            }
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        // Security gate: discovery is a Read operation (fetches the peer's
        // Agent Card), but still pass the shared policy boundary so a
        // read-only autonomy session is handled consistently.
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "a2a_discover")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }
        let filter_tags: Vec<String> = args
            .get("filter_tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        match args.get("peer").and_then(|v| v.as_str()) {
            Some(peer) => {
                let card = self.client.get_card(peer).await?;
                // Peer card text (name/description/skills/version/interfaces)
                // is attacker-authored by construction and shown to the model
                // so it can choose a delegate — a cross-agent prompt-injection
                // surface. Every field that originates from the remote card
                // (including card name, version, skill ids and tags, interface
                // URLs and tenants, capabilities) is fenced as
                // untrusted-external. Configured local fields (peer name) stay
                // outside the fence. Mirroring mcp_context's convention.
                let fenced_name = format!(
                    "<a2a-card trust=\"untrusted-external\">{}</a2a-card>",
                    card.name
                );
                let fenced_desc = format!(
                    "<a2a-card trust=\"untrusted-external\">\n{}\n</a2a-card>",
                    card.description
                );
                let fenced_version = format!(
                    "<a2a-card trust=\"untrusted-external\">{}</a2a-card>",
                    card.version
                );
                let fenced_skills: Vec<_> = card
                    .skills
                    .iter()
                    .map(|s| {
                        json!({
                            "id": format!("<a2a-skill trust=\"untrusted-external\">{}</a2a-skill>", s.id),
                            "name": format!("<a2a-skill trust=\"untrusted-external\">{}</a2a-skill>", s.name),
                            "description": format!("<a2a-skill trust=\"untrusted-external\">{}</a2a-skill>", s.description),
                            "tags": s.tags.iter().map(|t| format!("<a2a-skill trust=\"untrusted-external\">{t}</a2a-skill>")).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                let fenced_interfaces: Vec<_> = card
                    .supported_interfaces
                    .iter()
                    .map(|i| {
                        json!({
                            "url": format!("<a2a-interface trust=\"untrusted-external\">{}</a2a-interface>", i.url),
                            "protocol_binding": format!("<a2a-interface trust=\"untrusted-external\">{}</a2a-interface>", i.protocol_binding),
                            "protocol_version": format!("<a2a-interface trust=\"untrusted-external\">{}</a2a-interface>", i.protocol_version),
                            "tenant": i.tenant.as_ref().map(|t| format!("<a2a-interface trust=\"untrusted-external\">{t}</a2a-interface>")),
                        })
                    })
                    .collect();
                Ok(ToolResult::ok(ToolOutput::json(json!({
                    "peer": peer,
                    "name": fenced_name,
                    "description": fenced_desc,
                    "version": fenced_version,
                    "capabilities": card.capabilities,
                    "supported_interfaces": fenced_interfaces,
                    "skills": fenced_skills,
                }))))
            }
            None => {
                let config = self.client.config.read();
                let peers: Vec<_> = config
                    .a2a
                    .client
                    .peers
                    .iter()
                    .filter(|p| {
                        filter_tags.is_empty()
                            || filter_tags.iter().all(|t| p.tags.iter().any(|pt| pt == t))
                    })
                    .map(|p| json!({ "name": p.name, "base_url": p.base_url, "tags": p.tags }))
                    .collect();
                Ok(ToolResult::ok(ToolOutput::json(json!({ "peers": peers }))))
            }
        }
    }
}

#[async_trait]
impl Tool for A2aSendTool {
    fn name(&self) -> &str {
        "a2a_send"
    }
    fn description(&self) -> &str {
        "Delegate a task to a remote A2A peer agent and wait for the result. \
         Returns a Task with a task_id, state, and artifacts (the peer's reply, \
         fenced as untrusted-external). If the state is non-terminal \
         (working/input-required), poll with a2a_get_task or cancel with \
         a2a_cancel. The message is sent as-is. This is an Act operation that \
         requires approval by default (not in auto_approve) unless the operator \
         explicitly opts in via risk_profiles.<name>.auto_approve."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "peer": { "type": "string", "description": "Configured peer name to send the task to." },
                "agent": { "type": "string", "description": "Target agent alias on the peer (the {alias} in /a2a/{alias})." },
                "message": { "type": "string", "description": "The task prompt to send to the peer agent." },
                "return_immediately": { "type": "boolean", "description": "Default false (block for a terminal state). Set true to return immediately with a non-terminal (working/input-required) task for polling." },
                "context_id": { "type": "string", "description": "Optional context ID for multi-turn continuation (from a prior send's response)." },
                "task_id": { "type": "string", "description": "Optional task ID for continuing an existing task (e.g. after INPUT_REQUIRED)." }
            },
            "required": ["peer", "agent", "message"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        // Security gate: send is an Act operation (mutates peer state by
        // creating a task). Deny before any network I/O in read-only autonomy.
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "a2a_send")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }
        let peer = require_str(&args, "peer")?;
        let agent = require_str(&args, "agent")?;
        let message = require_str(&args, "message")?;
        let return_immediately = args
            .get("return_immediately")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let context_id = args
            .get("context_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let (task, was_task_branch) = self
            .client
            .send_message(
                &peer,
                &agent,
                &message,
                context_id,
                task_id,
                return_immediately,
            )
            .await?;
        Ok(task_to_output(&task, was_task_branch, &agent))
    }
}

#[async_trait]
impl Tool for A2aGetTaskTool {
    fn name(&self) -> &str {
        "a2a_get_task"
    }
    fn description(&self) -> &str {
        "Retrieve the current state and artifacts of an in-flight A2A task on a \
         peer. Use to poll a task that a2a_send returned in a non-terminal state \
         (working/input-required)."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "peer": { "type": "string", "description": "Configured peer name hosting the task." },
                "task_id": { "type": "string", "description": "The task id returned by a2a_send." },
                "agent": { "type": "string", "description": "Optional agent alias that created the task (from a2a_send). Helps route the poll to the correct interface on aggregate cards." }
            },
            "required": ["peer", "task_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        // Security gate: get_task is a Read (polls peer state, no mutation).
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "a2a_get_task")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }
        let peer = require_str(&args, "peer")?;
        let task_id = require_str(&args, "task_id")?;
        let agent = args.get("agent").and_then(|v| v.as_str()).unwrap_or("");
        let task = self
            .client
            .get_task(&peer, &task_id, args.get("agent").and_then(|v| v.as_str()))
            .await?;
        Ok(task_to_output(&task, true, agent))
    }
}

#[async_trait]
impl Tool for A2aCancelTool {
    fn name(&self) -> &str {
        "a2a_cancel"
    }
    fn description(&self) -> &str {
        "Cancel an in-flight A2A task on a peer. Returns the updated Task \
         (typically state=canceled, though the spec does not guarantee it)."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "peer": { "type": "string", "description": "Configured peer name hosting the task." },
                "task_id": { "type": "string", "description": "The task id to cancel." },
                "agent": { "type": "string", "description": "Optional agent alias that created the task (from a2a_send). Helps route the cancel to the correct interface on aggregate cards." }
            },
            "required": ["peer", "task_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        // Security gate: cancel is an Act (mutates peer task state).
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "a2a_cancel")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }
        let peer = require_str(&args, "peer")?;
        let task_id = require_str(&args, "task_id")?;
        let agent = args.get("agent").and_then(|v| v.as_str()).unwrap_or("");
        let task = self
            .client
            .cancel(&peer, &task_id, args.get("agent").and_then(|v| v.as_str()))
            .await?;
        Ok(task_to_output(&task, true, agent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::a2a_wire::{AgentCard, AgentInterface};

    #[test]
    fn resolve_token_passes_literal_through() {
        assert_eq!(resolve_env_backed_token("abc123").unwrap(), "abc123");
        assert_eq!(resolve_env_backed_token("").unwrap(), "");
    }

    #[test]
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
    fn resolve_token_rejects_missing_env_var() {
        assert!(resolve_env_backed_token("${ZC_TEST_DEFINITELY_MISSING_TOKEN}").is_err());
    }

    #[test]
    fn resolve_token_rejects_empty_and_bad_names() {
        assert!(resolve_env_backed_token("${}").is_err());
        assert!(resolve_env_backed_token("${bad-name}").is_err());
    }

    #[test]
    fn parse_peer_url_strips_port_and_path() {
        let u = parse_peer_url("https://team.example.com/a2a/x").unwrap();
        assert_eq!(u.host_str().unwrap(), "team.example.com");
        assert_eq!(u.port_or_known_default(), Some(443));
        let u = parse_peer_url("http://1.2.3.4:8080").unwrap();
        assert_eq!(u.host_str().unwrap(), "1.2.3.4");
        assert_eq!(u.port_or_known_default(), Some(8080));
    }

    #[test]
    fn parse_peer_url_rejects_non_http_and_bare_host() {
        assert!(parse_peer_url("ftp://team.example.com").is_err());
        assert!(parse_peer_url("team.example.com").is_err());
    }

    #[test]
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
    }

    #[test]
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
    fn select_interface_falls_back_to_first_when_unknown_agent_on_aggregate_card() {
        // Aggregate card (multiple v1 interfaces): requesting an agent that
        // matches no URL suffix falls back to the first v1 candidate. A2A
        // does not require agent names in URLs; multi-tenant peers use
        // `tenant` to distinguish agents.
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
        let ep = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "unknown-agent",
        )
        .unwrap();
        // Falls back to the first v1 candidate.
        assert_eq!(ep.url, "https://peer.example.com/a2a/alpha");
        assert_eq!(ep.tenant.as_deref(), Some("tenant-alpha"));
    }

    #[test]
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
    fn select_interface_prefers_url_match_when_multiple_candidates() {
        // Aggregate card (multiple v1 interfaces): prefer URL suffix match,
        // but if no match exists, still select the first candidate — agent
        // is opaque and multi-tenant peers use tenant, not URL suffix.
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
        // No URL match with unknown agent → first candidate (standard
        // multi-tenant peer, distinguishes via tenant).
        let ep = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "unknown-agent",
        )
        .unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/v1");
        assert_eq!(ep.tenant.as_deref(), Some("tenant-alpha"));
    }

    #[test]
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
    fn select_interface_empty_agent_picks_first() {
        // Empty agent with multiple v1 interfaces: selects the first
        // candidate (agent is opaque; multi-tenant peers use tenant).
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
        let ep = select_interface(
            Some(&card),
            &parse_peer_url("https://peer.example.com").unwrap(),
            "",
        )
        .unwrap();
        assert_eq!(ep.url, "https://peer.example.com/a2a/alpha");
        assert_eq!(ep.tenant.as_deref(), Some("t-alpha"));
    }

    #[test]
    fn task_url_joins_base_and_agent() {
        let base = parse_peer_url("https://x.example.com").unwrap();
        assert_eq!(
            task_url(&base, "beta").unwrap(),
            "https://x.example.com/a2a/beta"
        );
    }

    #[test]
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
    fn resolve_peer_token_decrypts_encrypted_value() {
        // The review-flagged secret boundary: a peer token stored as
        // SecretStore-encrypted ciphertext must be decrypted via the
        // canonical SecretStore (same path http_request uses), not treated
        // as a literal. This test proves the encrypted path resolves.
        let tmp = std::env::temp_dir().join(format!(
            "zc-a2a-token-enc-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let store = zeroclaw_config::secrets::SecretStore::new(&tmp, true);
        let encrypted = store.encrypt("plaintext-bearer-secret").unwrap();

        let mut config = Config::default();
        config.a2a.client.peers.push(A2aClientPeerConfig {
            name: "enc-peer".into(),
            base_url: "https://peer.example.com".into(),
            token: encrypted,
            tags: vec![],
        });
        let config = Arc::new(RwLock::new(config));
        let client = A2aHttpClient::new(config, 30, Some(tmp.clone()), true).unwrap();
        let peer = client.resolve_peer("enc-peer").unwrap();
        assert_eq!(peer.token, "plaintext-bearer-secret");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn resolve_peer_token_fails_encrypted_without_zeroclaw_dir() {
        // An encrypted token with no zeroclaw dir (no config path) must fail
        // clearly rather than emit ciphertext as the Authorization header.
        let store = zeroclaw_config::secrets::SecretStore::new(&std::env::temp_dir(), true);
        let encrypted = store.encrypt("secret").unwrap();
        let mut config = Config::default();
        config.a2a.client.peers.push(A2aClientPeerConfig {
            name: "enc-peer".into(),
            base_url: "https://peer.example.com".into(),
            token: encrypted,
            tags: vec![],
        });
        let config = Arc::new(RwLock::new(config));
        // No zeroclaw dir: encrypted resolution must error.
        let client = A2aHttpClient::new(config, 30, None, true).unwrap();
        assert!(client.resolve_peer("enc-peer").is_err());
    }

    #[tokio::test]
    async fn guard_host_rejects_private_and_metadata() {
        let config = Arc::new(RwLock::new(Config::default()));
        let client = A2aHttpClient::new(config, 30, None, false).unwrap();
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
    async fn guard_host_allows_loopback_when_operator_opted_in() {
        let mut config = Config::default();
        config.a2a.client.allow_private_hosts = true;
        let config = Arc::new(RwLock::new(config));
        let client = A2aHttpClient::new(config, 30, None, false).unwrap();
        // Loopback is accepted when the operator flipped the global switch.
        assert!(
            client
                .guard_host(&parse_peer_url("https://127.0.0.1").unwrap())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn guard_host_allows_pinned_private_host_but_not_others() {
        let mut config = Config::default();
        config.a2a.client.allowed_private_hosts = vec!["127.0.0.1".to_string()];
        let config = Arc::new(RwLock::new(config));
        let client = A2aHttpClient::new(config, 30, None, false).unwrap();
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

    #[tokio::test]
    async fn guard_host_rejects_invalid_allowed_private_hosts_entry() {
        let mut config = Config::default();
        config.a2a.client.allowed_private_hosts = vec!["!!not a domain!!".to_string()];
        let config = Arc::new(RwLock::new(config));
        let client = A2aHttpClient::new(config, 30, None, false).unwrap();
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
    fn resolve_peer_errors_on_unknown() {
        let config = Arc::new(RwLock::new(Config::default()));
        let client = A2aHttpClient::new(config, 30, None, false).unwrap();
        assert!(client.resolve_peer("nonexistent").is_err());
    }

    #[tokio::test]
    async fn a2a_send_denied_in_readonly_autonomy() {
        // B3: send is an Act operation; a read-only autonomy session must
        // deny it before any network I/O (no peer contact attempted).
        use zeroclaw_config::autonomy::AutonomyLevel;
        let config = Arc::new(RwLock::new(Config::default()));
        let client = Arc::new(A2aHttpClient::new(config, 30, None, false).unwrap());
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = A2aSendTool::new(Arc::clone(&client), security);
        let result = tool
            .execute(json!({"peer": "local", "agent": "assistant", "message": "hi"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
        // No route entry cached (deny happened before send_message).
        assert!(client.route_cache.lock().is_empty());
    }

    #[tokio::test]
    async fn a2a_discover_allowed_in_readonly_autonomy() {
        // B3: discover is a Read operation; it passes the policy boundary
        // even in read-only autonomy. The call fails later on unknown peer
        // (an Err, not a policy denial) — proving the Read classification
        // let it through the gate. A denial would have returned an Ok
        // ToolResult with success=false and a "read-only" error.
        use zeroclaw_config::autonomy::AutonomyLevel;
        let config = Arc::new(RwLock::new(Config::default()));
        let client = Arc::new(A2aHttpClient::new(config, 30, None, false).unwrap());
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = A2aDiscoverTool::new(Arc::clone(&client), security);
        // No peer configured -> get_card returns Err (unknown peer), NOT an
        // Ok(ToolResult) with a read-only denial. That Err proves the policy
        // gate let the Read operation through.
        let outcome = tool.execute(json!({"peer": "nonexistent"})).await;
        assert!(
            outcome.is_err(),
            "discover should reach get_card (Read allowed), not be denied"
        );
        let err_msg = outcome.unwrap_err().to_string();
        assert!(
            !err_msg.contains("read-only"),
            "discover (Read) must not be denied by read-only autonomy: {err_msg}"
        );
    }

    #[test]
    fn route_cache_stores_and_evicts() {
        // B1: route cache stores under (peer, agent, task_id) — agent in the
        // composite key prevents same-task-id collision across agents on one
        // peer. Terminal-state tasks are evicted. Direct map manipulation
        // (no HTTP) proves the cache wiring without a live peer.
        let config = Arc::new(RwLock::new(Config::default()));
        let client = A2aHttpClient::new(config, 30, None, false).unwrap();
        let route = RouteHandle {
            rpc_url: "https://peer.example.com/a2a/beta".to_string(),
            tenant: Some("t-beta".into()),
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

    // ── E2E HTTP-boundary tests (wiremock) ───────────────────────────

    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::autonomy::AutonomyLevel;

    /// Build a `Config` wired to `MockServer` with one peer (no auth).
    fn test_config(server: &MockServer) -> Config {
        let mut config = Config::default();
        config.secrets.encrypt = false;
        let mut client_cfg = config.a2a.client;
        client_cfg.enabled = true;
        client_cfg.request_timeout_secs = 10;
        client_cfg.card_cache_ttl_secs = 0; // disable cache so card is always fetched
        client_cfg.allow_private_hosts = true; // mock server lives on localhost
        client_cfg.max_response_bytes = 0; // unlimited for test bodies
        client_cfg.peers.push(A2aClientPeerConfig {
            name: "local".into(),
            base_url: server.uri(),
            token: String::new(),
            tags: vec![],
        });
        config.a2a.client = client_cfg;
        config
    }

    /// Mount a standard single-agent card (`supportedInterfaces` with one v1
    /// JSON-RPC entry for "assistant") so `select_interface` picks it.
    async fn mount_card(server: &MockServer) {
        let card = json!({
            "name": "test-peer",
            "description": "A test peer",
            "version": "1.0",
            "supportedInterfaces": [{
                "url": format!("{}/a2a/assistant", server.uri()),
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0"
            }],
            "capabilities": {},
            "defaultInputModes": [],
            "defaultOutputModes": [],
            "skills": [{
                "id": "s1",
                "name": "test-skill",
                "description": "a test skill"
            }]
        });
        Mock::given(method("GET"))
            .and(path("/.well-known/agent-card.json"))
            .and(header("A2A-Version", "1.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(card))
            .expect(1..)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn e2e_send_nonterminal_poll_terminal() {
        // Nonterminal send → poll → terminal: `SendMessage` returns WORKING,
        // `GetTask` returns COMPLETED. Proves the route cache persists across
        // the lifecycle and the poll lands on the right endpoint.
        let server = MockServer::start().await;
        mount_card(&server).await;

        // SendMessage → WORKING (non-terminal)
        Mock::given(method("POST"))
            .and(path("/a2a/assistant"))
            .and(body_string_contains("SendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "task": {
                        "id": "task-42",
                        "contextId": "ctx-1",
                        "status": {
                            "state": "TASK_STATE_WORKING",
                            "timestamp": "2025-01-01T00:00:00Z"
                        },
                        "artifacts": [],
                        "history": []
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        // GetTask → COMPLETED (terminal)
        Mock::given(method("POST"))
            .and(path("/a2a/assistant"))
            .and(body_string_contains("GetTask"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "id": "task-42",
                    "contextId": "ctx-1",
                    "status": {
                        "state": "TASK_STATE_COMPLETED",
                        "timestamp": "2025-01-01T00:00:01Z"
                    },
                    "artifacts": [{
                        "artifactId": "a1",
                        "parts": [{"text": "all done"}]
                    }],
                    "history": []
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let config = Arc::new(RwLock::new(test_config(&server)));
        let client = Arc::new(A2aHttpClient::new(config, 10, None, false).unwrap());
        let security = Arc::new(SecurityPolicy::default());

        // 1. Send: should return WORKING.
        let send_tool = A2aSendTool::new(Arc::clone(&client), Arc::clone(&security));
        let result = send_tool
            .execute(json!({"peer": "local", "agent": "assistant", "message": "do the thing"}))
            .await
            .unwrap();
        assert!(result.success, "send failed: {:?}", result.error);
        let data = result.output.data().expect("output must have data");
        assert_eq!(data["task_id"], "task-42");
        assert_eq!(data["state"], "TASK_STATE_WORKING");
        assert_eq!(data["response_type"], "task");
        // Route cache must have the entry.
        assert_eq!(client.route_cache.lock().len(), 1);

        // 2. Poll: should return COMPLETED.
        let poll_tool = A2aGetTaskTool::new(Arc::clone(&client), Arc::clone(&security));
        let result = poll_tool
            .execute(json!({"peer": "local", "task_id": "task-42", "agent": "assistant"}))
            .await
            .unwrap();
        assert!(result.success, "poll failed: {:?}", result.error);
        let data = result.output.data().expect("output must have data");
        assert_eq!(data["task_id"], "task-42");
        assert_eq!(data["state"], "TASK_STATE_COMPLETED");
        // Terminal state evicts the route.
        assert!(client.route_cache.lock().is_empty());
    }

    #[tokio::test]
    async fn e2e_cancel_in_flight_task() {
        // Send → cancel: `SendMessage` returns WORKING, `CancelTask` returns
        // CANCELED. Proves cancellation reaches the correct endpoint and
        // evicts the route on terminal state.
        let server = MockServer::start().await;
        mount_card(&server).await;

        Mock::given(method("POST"))
            .and(path("/a2a/assistant"))
            .and(body_string_contains("SendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "task": {
                        "id": "task-99",
                        "contextId": "ctx-99",
                        "status": {
                            "state": "TASK_STATE_WORKING",
                            "timestamp": "2025-01-01T00:00:00Z"
                        },
                        "artifacts": [],
                        "history": []
                    }
                }
            })))
            .expect(1..)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/a2a/assistant"))
            .and(body_string_contains("CancelTask"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "id": "task-99",
                    "contextId": "ctx-99",
                    "status": {
                        "state": "TASK_STATE_CANCELED",
                        "timestamp": "2025-01-01T00:00:01Z"
                    },
                    "artifacts": [],
                    "history": []
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let config = Arc::new(RwLock::new(test_config(&server)));
        let client = Arc::new(A2aHttpClient::new(config, 10, None, false).unwrap());
        let security = Arc::new(SecurityPolicy::default());

        let send_tool = A2aSendTool::new(Arc::clone(&client), Arc::clone(&security));
        let result = send_tool
            .execute(json!({"peer": "local", "agent": "assistant", "message": "cancel-me"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(client.route_cache.lock().len(), 1);

        let cancel_tool = A2aCancelTool::new(Arc::clone(&client), Arc::clone(&security));
        let result = cancel_tool
            .execute(json!({"peer": "local", "task_id": "task-99", "agent": "assistant"}))
            .await
            .unwrap();
        assert!(result.success, "cancel failed: {:?}", result.error);
        let data = result.output.data().expect("output must have data");
        assert_eq!(data["state"], "TASK_STATE_CANCELED");
        // Cancel is terminal — route evicted.
        assert!(client.route_cache.lock().is_empty());
    }

    #[tokio::test]
    async fn e2e_a2a_send_never_contacts_peer_when_readonly() {
        // B3 HTTP-level proof: a read-only autonomy session must deny
        // `a2a_send` before any network I/O. The mock server expects zero
        // requests — if a request arrives, the test fails.
        let server = MockServer::start().await;
        // Don't mount a card — autonomy denial happens before card fetch.
        // The card mock with expect(0) ensures no HTTP request hits the wire.

        // Mount a SendMessage mock with expect(0) — any POST to the RPC
        // endpoint is a test failure.
        Mock::given(method("POST"))
            .and(path("/a2a/assistant"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let config = Arc::new(RwLock::new(test_config(&server)));
        let client = Arc::new(A2aHttpClient::new(config, 10, None, false).unwrap());
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = A2aSendTool::new(Arc::clone(&client), security);
        let result = tool
            .execute(json!({"peer": "local", "agent": "assistant", "message": "hi"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn e2e_a2a_cancel_never_contacts_peer_when_readonly() {
        // B3 HTTP-level proof: `a2a_cancel` in read-only autonomy must be
        // denied before any network I/O.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/a2a/assistant"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let config = Arc::new(RwLock::new(test_config(&server)));
        let client = Arc::new(A2aHttpClient::new(config, 10, None, false).unwrap());
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = A2aCancelTool::new(Arc::clone(&client), security);
        let result = tool
            .execute(json!({"peer": "local", "task_id": "task-1"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn e2e_discover_fetches_card_and_fences_untrusted_fields() {
        // `a2a_discover` does a real HTTP fetch, receives a card with hostile
        // text in every remote-controlled field, and fences each one as
        // untrusted-external.
        let server = MockServer::start().await;

        // Mount card with untrusted text in all remote fields.
        let card = json!({
            "name": "<script>alert(1)</script>",
            "description": "benign",
            "version": "1.0",
            "supportedInterfaces": [{
                "url": format!("{}/a2a/assistant", server.uri()),
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0"
            }],
            "capabilities": {},
            "defaultInputModes": [],
            "defaultOutputModes": [],
            "skills": [{
                "id": "<img src=x onerror=alert(1)>",
                "name": "<script>evil</script>",
                "description": "a skill",
                "tags": ["<iframe>"]
            }]
        });
        // The fetch_card flow tries well-known/agent-card.json first
        // (with A2A-Version header) then falls back to agents-card.json.
        // Mount on both paths so the spec-first path works.
        for card_path in &[
            "/.well-known/agent-card.json",
            "/.well-known/agents-card.json",
        ] {
            Mock::given(method("GET"))
                .and(path(*card_path))
                .respond_with(ResponseTemplate::new(200).set_body_json(card.clone()))
                .expect(0..)
                .mount(&server)
                .await;
        }

        let config = Arc::new(RwLock::new(test_config(&server)));
        let client = Arc::new(A2aHttpClient::new(config, 10, None, false).unwrap());
        let security = Arc::new(SecurityPolicy::default());
        let tool = A2aDiscoverTool::new(Arc::clone(&client), security);

        let result = tool.execute(json!({"peer": "local"})).await.unwrap();
        assert!(result.success, "discover failed: {:?}", result.error);
        let output = result.output.data().expect("output must have data");

        // All the card-level strings must be fenced.
        let name = output["name"].as_str().unwrap();
        assert!(
            name.contains("untrusted-external"),
            "card name not fenced: {name}"
        );
        assert!(name.contains("<script>"), "card name text lost");

        let version = output["version"].as_str().unwrap();
        assert!(version.contains("untrusted-external"), "version not fenced");

        // Interface fields must be fenced.
        let iface = &output["supported_interfaces"][0];
        assert!(
            iface["url"]
                .as_str()
                .unwrap()
                .contains("untrusted-external"),
            "interface url not fenced"
        );
        assert!(
            iface["protocol_binding"]
                .as_str()
                .unwrap()
                .contains("untrusted-external"),
            "protocol_binding not fenced"
        );

        // Skill id, name, tags must be fenced.
        let skill = &output["skills"][0];
        assert!(
            skill["id"].as_str().unwrap().contains("untrusted-external"),
            "skill id not fenced"
        );
        assert!(
            skill["name"]
                .as_str()
                .unwrap()
                .contains("untrusted-external"),
            "skill name not fenced"
        );
        let tags = skill["tags"].as_array().unwrap();
        assert!(
            tags[0].as_str().unwrap().contains("untrusted-external"),
            "skill tag not fenced"
        );
    }

    #[tokio::test]
    async fn e2e_response_size_limit_streaming_reject() {
        // A peer returns a body larger than max_response_bytes; the client
        // must reject it before buffering the full body (streaming limit).
        let server = MockServer::start().await;
        mount_card(&server).await;

        // Set max_response_bytes to 10 so a 1KB RPC body is rejected.
        let mut config = test_config(&server);
        config.a2a.client.max_response_bytes = 10;
        let config = Arc::new(RwLock::new(config));
        let client = Arc::new(A2aHttpClient::new(config, 10, None, false).unwrap());
        let security = Arc::new(SecurityPolicy::default());

        Mock::given(method("POST"))
            .and(path("/a2a/assistant"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a".repeat(1024)))
            .expect(1)
            .mount(&server)
            .await;

        let tool = A2aSendTool::new(Arc::clone(&client), security);
        let result = tool
            .execute(json!({"peer": "local", "agent": "assistant", "message": "hi"}))
            .await;
        assert!(
            result.is_err(),
            "oversized response must be rejected (expect Err, not Ok)"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("max_response_bytes"),
            "error must mention max_response_bytes: {err}"
        );
    }
}
