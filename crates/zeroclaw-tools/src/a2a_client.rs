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
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};

use zeroclaw_api::a2a_wire::{AgentCard, JsonRpcResponse, Task, rpc_result};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_api::tool_attribution;
use zeroclaw_config::schema::Config;

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
    config: LiveConfig,
    /// Agent Card cache, keyed by peer base_url (derived data, not operator
    /// state). A base_url change is a different key, so an endpoint change
    /// naturally invalidates the prior entry. Populated lazily on first
    /// discover/send; peers whose card is never fetched cost nothing.
    card_cache: Mutex<HashMap<String, AgentCard>>,
}

impl A2aHttpClient {
    /// Build the client. The reqwest client is built once (timeout + no-redirect
    /// policy); peers resolve from live config per call, so nothing about peers
    /// is eagerly validated here.
    pub fn new(config: LiveConfig, request_timeout_secs: u64) -> anyhow::Result<Self> {
        let timeout = if request_timeout_secs == 0 {
            // 0 is documented as "no caching" for the card TTL but is an unsafe
            // default for a request timeout. Fall back to a safe 30s.
            std::time::Duration::from_secs(30)
        } else {
            std::time::Duration::from_secs(request_timeout_secs)
        };
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(std::time::Duration::from_secs(10))
            // No redirects: a peer that 3xx-redirects to an internal host would
            // otherwise be followed into the private network (SSRF).
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            config,
            card_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Resolve a single peer by name from the live config at call time. The
    /// read lock is held only to clone the raw peer fields; token env
    /// interpolation happens after the lock drops so a slow `std::env::var`
    /// can't block config hot-reload writers.
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
            token: resolve_token(&raw_token)?,
        })
    }

    /// SSRF guard: reject private/loopback/link-local/metadata hosts before any
    /// request is issued, mirroring `http_request`'s posture so the outbound
    /// A2A surface reuses the canonical URL/domain/SSRF policy (no duplicated
    /// private-host authority). Two layers,
    /// both via `helpers::domain_guard`:
    /// (1) host-literal check — metadata IPs are always blocked; private/
    ///     loopback/link-local hosts are blocked unless the operator opted in
    ///     via `allow_private_hosts` or pinned the exact host in
    ///     `allowed_private_hosts`;
    /// (2) DNS-resolved IP check — resolves the host and, when private
    ///     resolution is allowed, only blocks cloud-metadata IPs; otherwise
    ///     blocks any IP that lands in private/loopback/link-local/metadata
    ///     space, so a public domain resolving into the private network
    ///     (DNS rebinding) is contained. Literal-IP hosts skip resolution.
    async fn guard_host(&self, base_url: &str) -> anyhow::Result<()> {
        let host = extract_host(base_url)?;
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
                "a2a client: peer base_url host '{host}' is a cloud metadata address and not allowed"
            );
        }

        let private_host = is_private_or_local_host(&host);
        let private_host_explicitly_allowed =
            private_host && crate::helpers::domain_guard::host_matches_allowlist(&host, &allowed);
        if private_host && !private_host_explicitly_allowed && !allow_private {
            anyhow::bail!(
                "a2a client: peer base_url host '{host}' is private/loopback and not allowed (set a2a.client.allow_private_hosts or a2a.client.allowed_private_hosts)"
            );
        }

        // (2) DNS-rebinding defense: resolve the host and validate the resolved
        // IPs. Literal IPs are covered by the checks above (private/metadata
        // cases), and a public literal IP needs no resolution.
        if host.parse::<std::net::IpAddr>().is_err() {
            let port = extract_port(base_url).unwrap_or(443);
            let ips = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|e| {
                    anyhow::Error::msg(format!(
                        "a2a client: failed to resolve peer host '{host}': {e}"
                    ))
                })?
                .map(|addr| addr.ip())
                .collect::<Vec<_>>();
            let private_resolution_allowed = allow_private || private_host_explicitly_allowed;
            if private_resolution_allowed {
                crate::helpers::domain_guard::validate_resolved_ips_exclude_metadata(&host, &ips)?;
            } else {
                crate::helpers::domain_guard::validate_resolved_ips_are_public(&host, &ips)?;
            }
        }
        Ok(())
    }
}

/// Resolve a `${VAR}` token placeholder to its env value, or return the
/// literal. Empty (no `${}`) means anonymous peer (no Authorization header).
/// Mirrors http_request's `env_secret_reference` grammar without pulling that
/// private function across the module boundary.
fn resolve_token(raw: &str) -> anyhow::Result<String> {
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

/// Extract the host portion from a `http(s)://host[:port]/...` URL without
/// pulling in a URL crate; peer base_urls are simple operator strings.
fn extract_host(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| anyhow::Error::msg("a2a client: base_url must be http:// or https://"))?;
    let host = rest.split('/').next().unwrap_or(rest);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        anyhow::bail!("a2a client: base_url has empty host");
    }
    Ok(host.to_string())
}

/// Extract the port for a `http(s)://host[:port]/...` URL: an explicit port
/// if present, otherwise the scheme default (80 / 443). Used to drive
/// `tokio::net::lookup_host` for the DNS-rebinding SSRF check.
fn extract_port(url: &str) -> anyhow::Result<u16> {
    let (scheme, rest) = url
        .strip_prefix("https://")
        .map(|r| ("https", r))
        .or_else(|| url.strip_prefix("http://").map(|r| ("http", r)))
        .ok_or_else(|| anyhow::Error::msg("a2a client: base_url must be http:// or https://"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if let Some(port) = authority
        .rsplit_once(':')
        .and_then(|(_, port_str)| port_str.parse::<u16>().ok())
    {
        return Ok(port);
    }
    Ok(if scheme == "https" { 443 } else { 80 })
}

/// Task endpoint under a peer base: `<base>/a2a/{agent}`.
fn task_url(base_url: &str, agent: &str) -> String {
    format!("{base_url}/a2a/{agent}")
}

/// Resolve the JSON-RPC endpoint URL for a peer from its Agent Card's
/// `supportedInterfaces` (spec §4.4 transport selection). Picks the first
/// interface whose `protocolBinding` is JSON-RPC (case-insensitive); falls
/// back to the conventional `<base>/a2a/{agent}` path when the card is
/// absent or carries no JSON-RPC interface. MVP supports only the
/// JSON-RPC/REST transport binding; other bindings are a follow-up.
fn rpc_endpoint_url(card: Option<&AgentCard>, base_url: &str, agent: &str) -> String {
    if let Some(iface) = card.and_then(|c| {
        c.supported_interfaces
            .iter()
            .find(|i| i.protocol_binding.eq_ignore_ascii_case("jsonrpc"))
    }) {
        return iface.url.clone();
    }
    task_url(base_url, agent)
}

// ── JSON-RPC call methods (caller role) ────────────────────────────

impl A2aHttpClient {
    /// `message/send` (spec §3.1.1): delegate a task to a peer agent and block
    /// for the returned `Task`. Non-terminal states are returned as-is for the
    /// tool layer to poll/cancel.
    pub async fn send_message(
        &self,
        peer: &str,
        agent: &str,
        message: &str,
    ) -> anyhow::Result<Task> {
        let peer_ref = self.resolve_peer(peer)?;
        self.guard_host(&peer_ref.base_url).await?;
        // Transport selection (spec §4.4): read supportedInterfaces from the
        // peer's cached card, prefer a JSON-RPC binding URL, else fall back to
        // the conventional /a2a/{agent} path. Card fetch is cached by endpoint.
        let card = self.card_for_endpoint(peer).await.ok();
        let url = rpc_endpoint_url(card.as_ref(), &peer_ref.base_url, agent);
        let params = json!({
            "message": {
                "role": "user",
                "parts": [ { "kind": "text", "text": message } ],
            }
        });
        let resp: JsonRpcResponse<Task> = self
            .post_jsonrpc(&url, "message/send", params, &peer_ref)
            .await?;
        rpc_result(resp)
    }

    /// `tasks/get` (spec §3.1.3): retrieve the current state and artifacts of
    /// an in-flight task.
    pub async fn get_task(&self, peer: &str, task_id: &str) -> anyhow::Result<Task> {
        let peer_ref = self.resolve_peer(peer)?;
        self.guard_host(&peer_ref.base_url).await?;
        let card = self.card_for_endpoint(peer).await.ok();
        let url = rpc_endpoint_url(card.as_ref(), &peer_ref.base_url, "");
        let params = json!({ "id": task_id });
        let resp: JsonRpcResponse<Task> = self
            .post_jsonrpc(&url, "tasks/get", params, &peer_ref)
            .await?;
        rpc_result(resp)
    }

    /// `tasks/cancel` (spec §3.1.5): request cancellation of an in-flight task.
    pub async fn cancel(&self, peer: &str, task_id: &str) -> anyhow::Result<Task> {
        let peer_ref = self.resolve_peer(peer)?;
        self.guard_host(&peer_ref.base_url).await?;
        let card = self.card_for_endpoint(peer).await.ok();
        let url = rpc_endpoint_url(card.as_ref(), &peer_ref.base_url, "");
        let params = json!({ "id": task_id });
        let resp: JsonRpcResponse<Task> = self
            .post_jsonrpc(&url, "tasks/cancel", params, &peer_ref)
            .await?;
        rpc_result(resp)
    }

    /// `GET /.well-known/agent-card.json` (spec §14.3 discovery
    /// surface): fetch a peer's public Agent Card from the origin root. The
    /// well-known card is unauthenticated. Cached by base_url (derived data):
    /// a repeat discover/send reuses the cached card; an endpoint change is
    /// a different cache key, so the prior entry is naturally invalidated.
    pub async fn get_card(&self, peer: &str) -> anyhow::Result<AgentCard> {
        let peer_ref = self.resolve_peer(peer)?;
        self.guard_host(&peer_ref.base_url).await?;
        // Cache hit: clone and return without a network round-trip.
        if let Some(card) = self.card_cache.lock().get(&peer_ref.base_url).cloned() {
            return Ok(card);
        }
        let card = self.fetch_card(&peer_ref.base_url).await?;
        self.card_cache
            .lock()
            .insert(peer_ref.base_url.clone(), card.clone());
        Ok(card)
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
    async fn fetch_card(&self, base_url: &str) -> anyhow::Result<AgentCard> {
        let spec_url = format!("{base_url}/.well-known/agent-card.json");
        // A 2xx body that fails JSON decode is the ZeroClaw-inbound case (the
        // spec root path 404s to a gateway SPA / HTML today), so try spec-first
        // and fall back only on an HTTP-level miss or a decode failure.
        if let Ok(card) = self.try_card_url(&spec_url).await {
            return Ok(card);
        }
        let catalog_url = format!("{base_url}/.well-known/agents-card.json");
        self.try_card_url(&catalog_url).await.map_err(|e| {
            anyhow::Error::msg(format!(
                "a2a client: could not fetch Agent Card from peer '{base_url}' \
                 (tried spec path {spec_url} then ZeroClaw catalog {catalog_url}): {e}"
            ))
        })
    }

    /// GET a card URL and decode it as `AgentCard`. Returns an error when the
    /// endpoint is missing (non-success status) or the body is not a valid
    /// Agent Card JSON — both signal "try the fallback path".
    async fn try_card_url(&self, url: &str) -> anyhow::Result<AgentCard> {
        let resp = self
            .http
            .get(url)
            .header("A2A-Version", A2A_VERSION)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("HTTP {status}");
        }
        Ok(resp.json::<AgentCard>().await?)
    }

    /// Resolve a peer's Agent Card from cache if present, else fetch it. Used
    /// by `send_message` to read `supportedInterfaces` for transport selection
    /// without a redundant fetch when `a2a_discover` already populated it.
    async fn card_for_endpoint(&self, peer: &str) -> anyhow::Result<AgentCard> {
        self.get_card(peer).await
    }

    /// POST a JSON-RPC 2.0 request envelope and deserialize the response.
    /// Bearer token from the resolved peer is attached when non-empty.
    async fn post_jsonrpc<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        method: &str,
        params: Value,
        peer: &ResolvedPeer,
    ) -> anyhow::Result<T> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let mut req = self
            .http
            .post(url)
            .header("A2A-Version", A2A_VERSION)
            .json(&body);
        if !peer.token.is_empty() {
            req = req.bearer_auth(&peer.token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("a2a client: peer returned HTTP {status}: {text}");
        }
        Ok(resp.json::<T>().await?)
    }
}

// ── Tool surface (4 independent tools) ─────────────────────────────

/// `a2a_discover` — list available peer agents and their capabilities.
pub struct A2aDiscoverTool {
    client: Arc<A2aHttpClient>,
}
/// `a2a_send` — delegate a task to a peer agent (blocking on the result).
pub struct A2aSendTool {
    client: Arc<A2aHttpClient>,
}
/// `a2a_get_task` — retrieve the state/artifacts of an in-flight task.
pub struct A2aGetTaskTool {
    client: Arc<A2aHttpClient>,
}
/// `a2a_cancel` — cancel an in-flight task.
pub struct A2aCancelTool {
    client: Arc<A2aHttpClient>,
}

impl A2aDiscoverTool {
    pub fn new(client: Arc<A2aHttpClient>) -> Self {
        Self { client }
    }
}
impl A2aSendTool {
    pub fn new(client: Arc<A2aHttpClient>) -> Self {
        Self { client }
    }
}
impl A2aGetTaskTool {
    pub fn new(client: Arc<A2aHttpClient>) -> Self {
        Self { client }
    }
}
impl A2aCancelTool {
    pub fn new(client: Arc<A2aHttpClient>) -> Self {
        Self { client }
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

/// Render a peer Task into the structured `ToolOutput::json` the agent sees.
fn task_to_output(task: &Task) -> ToolResult {
    let data = json!({
        "task_id": task.id,
        "state": task.status.state,
        "context_id": task.context_id,
        "artifacts": task.artifacts.iter().map(|a| json!({
            "artifact_id": a.artifact_id,
            "text": a.parts.iter().filter(|p| p.kind == "text").map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n"),
        })).collect::<Vec<_>>(),
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
                Ok(ToolResult::ok(ToolOutput::json(json!({
                    "peer": peer,
                    "name": card.name,
                    "description": card.description,
                    "version": card.version,
                    "skills": card.skills,
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
         Returns a Task with a task_id, state, and artifacts (the peer's reply). \
         If the state is non-terminal (working/input-required), poll with \
         a2a_get_task or cancel with a2a_cancel."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "peer": { "type": "string", "description": "Configured peer name to send the task to." },
                "agent": { "type": "string", "description": "Target agent alias on the peer (the {alias} in /a2a/{alias})." },
                "message": { "type": "string", "description": "The task prompt to send to the peer agent." }
            },
            "required": ["peer", "agent", "message"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let peer = require_str(&args, "peer")?;
        let agent = require_str(&args, "agent")?;
        let message = require_str(&args, "message")?;
        let task = self.client.send_message(&peer, &agent, &message).await?;
        Ok(task_to_output(&task))
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
                "task_id": { "type": "string", "description": "The task id returned by a2a_send." }
            },
            "required": ["peer", "task_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let peer = require_str(&args, "peer")?;
        let task_id = require_str(&args, "task_id")?;
        let task = self.client.get_task(&peer, &task_id).await?;
        Ok(task_to_output(&task))
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
                "task_id": { "type": "string", "description": "The task id to cancel." }
            },
            "required": ["peer", "task_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let peer = require_str(&args, "peer")?;
        let task_id = require_str(&args, "task_id")?;
        let task = self.client.cancel(&peer, &task_id).await?;
        Ok(task_to_output(&task))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::a2a_wire::{AgentCard, AgentInterface};

    #[test]
    fn resolve_token_passes_literal_through() {
        assert_eq!(resolve_token("abc123").unwrap(), "abc123");
        assert_eq!(resolve_token("").unwrap(), "");
    }

    #[test]
    fn resolve_token_interpolates_env_var() {
        unsafe {
            std::env::set_var("ZC_TEST_A2A_TOKEN", "secret-value");
        }
        assert_eq!(
            resolve_token("${ZC_TEST_A2A_TOKEN}").unwrap(),
            "secret-value"
        );
        unsafe {
            std::env::remove_var("ZC_TEST_A2A_TOKEN");
        }
    }

    #[test]
    fn resolve_token_rejects_missing_env_var() {
        assert!(resolve_token("${ZC_TEST_DEFINITELY_MISSING_TOKEN}").is_err());
    }

    #[test]
    fn resolve_token_rejects_empty_and_bad_names() {
        assert!(resolve_token("${}").is_err());
        assert!(resolve_token("${bad-name}").is_err());
    }

    #[test]
    fn extract_host_strips_scheme_port_and_path() {
        assert_eq!(
            extract_host("https://team.example.com").unwrap(),
            "team.example.com"
        );
        assert_eq!(
            extract_host("https://team.example.com/a2a/x").unwrap(),
            "team.example.com"
        );
        assert_eq!(extract_host("http://1.2.3.4:8080").unwrap(), "1.2.3.4");
    }

    #[test]
    fn extract_host_rejects_non_http() {
        assert!(extract_host("ftp://team.example.com").is_err());
        assert!(extract_host("team.example.com").is_err());
    }

    #[test]
    fn extract_port_explicit_and_default() {
        assert_eq!(extract_port("https://x.example.com:8443/a").unwrap(), 8443);
        assert_eq!(extract_port("http://x.example.com:8080").unwrap(), 8080);
        assert_eq!(extract_port("https://x.example.com").unwrap(), 443);
        assert_eq!(extract_port("http://x.example.com").unwrap(), 80);
        assert!(extract_port("ftp://x.example.com").is_err());
    }

    #[test]
    fn rpc_endpoint_url_prefers_jsonrpc_interface() {
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://peer.example.com/rpc".into(),
                protocol_binding: "JSONRPC".into(),
                protocol_version: "1.0".into(),
            }],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        assert_eq!(
            rpc_endpoint_url(Some(&card), "https://peer.example.com", "beta"),
            "https://peer.example.com/rpc"
        );
    }

    #[test]
    fn rpc_endpoint_url_falls_back_when_no_jsonrpc() {
        // No card -> conventional path.
        assert_eq!(
            rpc_endpoint_url(None, "https://peer.example.com", "beta"),
            "https://peer.example.com/a2a/beta"
        );
        // Card with no JSON-RPC interface -> fallback too.
        let card = AgentCard {
            name: "p".into(),
            description: "d".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://peer.example.com/grpc".into(),
                protocol_binding: "GRPC".into(),
                protocol_version: "1.0".into(),
            }],
            version: "1.0".into(),
            capabilities: Default::default(),
            default_input_modes: vec![],
            default_output_modes: vec![],
            skills: vec![],
        };
        assert_eq!(
            rpc_endpoint_url(Some(&card), "https://peer.example.com", "beta"),
            "https://peer.example.com/a2a/beta"
        );
    }

    #[test]
    fn task_url_joins_base_and_agent() {
        assert_eq!(
            task_url("https://x.example.com", "beta"),
            "https://x.example.com/a2a/beta"
        );
    }

    #[tokio::test]
    async fn guard_host_rejects_private_and_metadata() {
        let config = Arc::new(RwLock::new(Config::default()));
        let client = A2aHttpClient::new(config, 30).unwrap();
        // Literal private/metadata hosts are rejected by the host-literal
        // check (no DNS lookup needed).
        assert!(client.guard_host("https://127.0.0.1").await.is_err());
        assert!(client.guard_host("https://169.254.169.254").await.is_err());
        assert!(client.guard_host("https://10.0.0.1").await.is_err());
    }

    #[tokio::test]
    async fn guard_host_allows_loopback_when_operator_opted_in() {
        let mut config = Config::default();
        config.a2a.client.allow_private_hosts = true;
        let config = Arc::new(RwLock::new(config));
        let client = A2aHttpClient::new(config, 30).unwrap();
        // Loopback is accepted when the operator flipped the global switch.
        assert!(client.guard_host("https://127.0.0.1").await.is_ok());
    }

    #[tokio::test]
    async fn guard_host_allows_pinned_private_host_but_not_others() {
        let mut config = Config::default();
        config.a2a.client.allowed_private_hosts = vec!["127.0.0.1".to_string()];
        let config = Arc::new(RwLock::new(config));
        let client = A2aHttpClient::new(config, 30).unwrap();
        // The pinned private host is allowed.
        assert!(client.guard_host("https://127.0.0.1").await.is_ok());
        // A different private host is still blocked — pinning is exact.
        assert!(client.guard_host("https://10.0.0.1").await.is_err());
        // A metadata host is never allowed, even when pinned explicitly.
        assert!(client.guard_host("https://169.254.169.254").await.is_err());
    }

    #[tokio::test]
    async fn guard_host_rejects_invalid_allowed_private_hosts_entry() {
        let mut config = Config::default();
        config.a2a.client.allowed_private_hosts = vec!["!!not a domain!!".to_string()];
        let config = Arc::new(RwLock::new(config));
        let client = A2aHttpClient::new(config, 30).unwrap();
        // An un-normalizable allowlist entry surfaces as a config error at
        // guard time rather than being silently skipped.
        assert!(client.guard_host("https://127.0.0.1").await.is_err());
    }

    #[test]
    fn resolve_peer_errors_on_unknown() {
        let config = Arc::new(RwLock::new(Config::default()));
        let client = A2aHttpClient::new(config, 30).unwrap();
        assert!(client.resolve_peer("nonexistent").is_err());
    }
}
