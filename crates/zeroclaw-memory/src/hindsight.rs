//! Hindsight external memory backend.
//!
//! A first-class [`Memory`] implementation that routes the agent's normal
//! store/recall path to the native Hindsight HTTP API (server-side
//! vectorization + embedding search) instead of the local SQLite/BM25 store.
//!
//! Selection: a per-agent `[agents.<alias>.memory] backend = "hindsight"`
//! (first-class `MemoryBackendKind::Hindsight`). The runtime factory
//! (`create_memory_for_agent`) builds this backend for that agent, so hindsight
//! becomes the agent's built-in memory pipeline (both the automatic per-turn
//! consolidation writes and the `memory_store` / `memory_recall` tools).
//!
//! Per-agent isolation: the bank id derives per agent from the install-wide
//! `[memory.hindsight] bank_template` (`zeroclaw-{agent}` by default), or an
//! explicit per-agent `agents.<alias>.memory.bank_id`. Because each agent gets
//! its own server-namespaced bank, the bank itself is the private-per-agent
//! scope - no local agent_id column is needed.
//!
//! Configuration comes from the typed `[memory.hindsight]` section via the
//! single canonical constructor [`HindsightMemory::from_config`]; the bearer
//! token is resolved from the environment (or an inline non-committed `token`)
//! so no secret lands in a committed config file. Every selection and
//! construction path (per-agent enum, install-wide `memory.backend =
//! "hindsight"` string, CLI/migration, and status) routes through this one
//! typed constructor, which re-validates the config
//! ([`HindsightMemoryConfig::validate_self`]) before building, so no path can
//! reach the refused default endpoint, a plaintext remote, or an invalid bank
//! template. There is no env-only constructor: the typed config is the single
//! source of truth for endpoint, token env, timeout, and bank derivation.
//!
//! Deletion (`forget` / `forget_for_agent`): mapped to the hindsight invalidate
//! endpoint (`PATCH .../memories/{id}` with `state=invalidated`), a soft-delete
//! so a first-class backend never silently declines a removal. The `key` the
//! trait passes is the memory id the read paths surface (`id` and `key` are both
//! set to the server id in `to_entry`). Deletion targets the private bank only -
//! the same bank writes land in.
//!
//! Shared and system tiers (this slice): the typed `[memory.hindsight]`
//! `shared_bank` / `system_bank` fields name two extra banks every agent can
//! READ from (merged into recall + list). The legacy `ZC_HINDSIGHT_SHARED_BANK`
//! / `ZC_HINDSIGHT_SYSTEM_BANK` env vars are bridged into those typed fields at
//! config load so they are validated against every agent's private bank exactly
//! like a TOML value, rather than resolved late at construction against only the
//! constructing agent's own bank. Ordinary writes (`store`, including automatic
//! per-turn consolidation) always land in the per-agent private `bank`, so
//! personal memory stays isolated. The shared/system banks are written ONLY via
//! the explicit [`HindsightMemory::store_to_bank`] path behind the dedicated
//! `shared_memory_store` / `system_memory_store` tools; `shared_memory_store` is
//! per-agent gateable by name, while `system_memory_store` additionally requires
//! an explicit deny-by-default admin grant. This is the native mechanism for a
//! tiered memory model: private per agent + permitted shared/family writes +
//! admin-only system writes, all readable by everyone.
//!
//! Cross-agent collision safety: config load
//! ([`HindsightMemoryConfig::validate_self`] plus the install-wide `Config`
//! validation) rejects any shared/system bank that equals ANY agent's resolved
//! private bank, so one agent's shared tier can never alias another agent's
//! private bank. The per-instance [`resolve_secondary_bank`] drop below is a
//! second, defense-in-depth guard for the single constructing instance.

use super::traits::{Memory, MemoryCategory, MemoryEntry, SharedWritable};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use zeroclaw_config::schema::{
    DEFAULT_HINDSIGHT_TIMEOUT_SECS, DEFAULT_HINDSIGHT_TOP_K, HindsightMemoryConfig,
};

/// Percent-encode a single URL path segment (bank id or server-provided memory
/// id). Encodes everything that is not an unreserved URL character so a bank
/// name or id containing `/`, `?`, `#`, spaces, or other reserved bytes cannot
/// break out of its path segment or inject query/fragment components. Mirrors
/// the repo convention of routing configurable/remote strings through
/// `urlencoding` before interpolating them into a request URL.
fn encode_segment(segment: &str) -> String {
    urlencoding::encode(segment).into_owned()
}

/// Maximum number of characters of a remote error body echoed into an error
/// message. Remote bodies are attacker/operator-influenced and may contain
/// secrets or large payloads, so they are truncated before surfacing.
const MAX_REMOTE_ERROR_BODY: usize = 512;

/// Hard cap on the number of raw bytes READ from a remote ERROR body before it
/// is turned into a bounded snippet. Whitespace collapse can only shrink the
/// text, so reading this many bytes always suffices to fill the
/// [`MAX_REMOTE_ERROR_BODY`]-char snippet while refusing to buffer a
/// gigabyte-sized error stream into memory.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

/// Hard cap on the number of raw bytes READ from a remote SUCCESS body (recall
/// / list / count JSON) before parsing. A well-formed response for a bounded
/// `top_k` stays far under this; a body that exceeds it is treated as a
/// malfunctioning/hostile endpoint and refused rather than materialized, so a
/// server that streams an unbounded body within the request timeout cannot
/// exhaust process memory.
const MAX_REMOTE_BODY_BYTES: usize = 1024 * 1024;

/// Stream a response body chunk-by-chunk, accumulating at most `cap` bytes and
/// STOPPING as soon as the cap is reached. Returns the (capped) bytes plus
/// whether the body was truncated (i.e. more data existed beyond the cap).
///
/// This is the memory-safety boundary for remote reads: unlike
/// `Response::text()` / `Response::json()` in the pinned reqwest, which collect
/// the entire body before decoding, this pulls chunks and bails out at the cap,
/// so a huge or never-ending body streamed within the request timeout can
/// allocate at most `cap` bytes here.
async fn read_body_capped(mut resp: reqwest::Response, cap: usize) -> Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = resp
        .chunk()
        .await
        .context("hindsight response stream failed")?
    {
        if chunk.is_empty() {
            continue;
        }
        let remaining = cap.saturating_sub(buf.len());
        if remaining == 0 {
            // Already at the cap and the server still has more to send: stop
            // reading (dropping `resp` closes the connection) instead of
            // buffering the rest.
            truncated = true;
            break;
        }
        if chunk.len() > remaining {
            buf.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok((buf, truncated))
}

/// Read a failed response's body and reduce it to a bounded, single-line
/// snippet safe to embed in an error message. Streams at most
/// [`MAX_ERROR_BODY_BYTES`] (so a huge error body is never fully buffered),
/// collapses whitespace/newlines, and truncates to [`MAX_REMOTE_ERROR_BODY`]
/// chars so the remote body cannot flood logs, exhaust memory, or smuggle
/// control characters into the error surface.
async fn bounded_error_body(resp: reqwest::Response) -> String {
    let (bytes, over_cap) = read_body_capped(resp, MAX_ERROR_BODY_BYTES)
        .await
        .unwrap_or_default();
    let raw = String::from_utf8_lossy(&bytes);
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if over_cap || collapsed.len() > MAX_REMOTE_ERROR_BODY {
        let mut end = MAX_REMOTE_ERROR_BODY.min(collapsed.len());
        while !collapsed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}… (truncated)", &collapsed[..end])
    } else {
        collapsed
    }
}

/// Read a successful response body under the [`MAX_REMOTE_BODY_BYTES`] cap and
/// deserialize it as `T`. Enforces the byte cap while STREAMING (before any
/// JSON materialization) so a malfunctioning/hostile endpoint cannot exhaust
/// memory with an oversized success body; an over-cap body is refused with
/// `context` rather than parsed.
async fn read_json_capped<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
    context: &'static str,
) -> Result<T> {
    let (bytes, over_cap) = read_body_capped(resp, MAX_REMOTE_BODY_BYTES).await?;
    if over_cap {
        anyhow::bail!(
            "{context}: response body exceeded the {MAX_REMOTE_BODY_BYTES}-byte cap and was refused"
        );
    }
    serde_json::from_slice(&bytes).context(context)
}

/// Build the shared `reqwest::Client` with a per-request timeout so every
/// outbound Hindsight call is bounded. A `timeout_secs` of `0` (which config
/// validation rejects, but the env path could still yield) falls back to the
/// canonical [`DEFAULT_HINDSIGHT_TIMEOUT_SECS`] so the client is never built
/// unbounded. If the builder itself fails (it does not under the pinned TLS
/// features), fall back to a default client rather than panicking.
fn build_client(timeout_secs: u64) -> reqwest::Client {
    let secs = if timeout_secs == 0 {
        DEFAULT_HINDSIGHT_TIMEOUT_SECS
    } else {
        timeout_secs
    };
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(secs))
        .build()
        .unwrap_or_default()
}

/// Hindsight-backed memory store bound to a single private bank, with optional
/// shared and system banks that are read-merged and written only via the
/// explicit [`HindsightMemory::store_to_bank`] tool path.
pub struct HindsightMemory {
    alias: String,
    base_url: String,
    bank: String,
    /// Optional shared/family bank merged into recall/list as READ-ONLY.
    /// Ordinary writes never touch it; the `shared_memory_store` tool writes it
    /// via `store_to_bank`.
    shared_bank: Option<String>,
    /// Optional system bank merged into recall/list as READ-ONLY. Ordinary
    /// writes never touch it; the `system_memory_store` tool writes it via
    /// `store_to_bank`.
    system_bank: Option<String>,
    token: String,
    default_top_k: usize,
    client: reqwest::Client,
}

impl std::fmt::Debug for HindsightMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HindsightMemory")
            .field("alias", &self.alias)
            .field("base_url", &self.base_url)
            .field("bank", &self.bank)
            .field("shared_bank", &self.shared_bank)
            .field("system_bank", &self.system_bank)
            .field("default_top_k", &self.default_top_k)
            .finish_non_exhaustive()
    }
}

/// Resolve a secondary (shared/system) bank from typed config, dropping it when
/// empty or when it collides with an already-taken bank (private or, for system,
/// the shared bank).
///
/// The tier banks are TYPED-CONFIG ONLY here: the legacy
/// `ZC_HINDSIGHT_SHARED_BANK` / `ZC_HINDSIGHT_SYSTEM_BANK` env vars are bridged
/// into `memory.hindsight.shared_bank` / `system_bank` at config load
/// (`env_overrides::apply_hindsight_tier_bank_env_bridge`), so by the time the
/// backend constructs, an env-provided tier bank has already been validated by
/// `Config::validate` against EVERY agent's private bank - not just this one.
/// Resolving the env var here again would reintroduce the cross-agent leak this
/// closed, so it is deliberately not read. The per-instance collision drop below
/// remains as defense-in-depth for the single constructing instance.
fn resolve_secondary_bank(configured: Option<&str>, taken: &[&str]) -> Option<String> {
    configured
        .map(str::to_string)
        .filter(|b| !b.is_empty() && !taken.contains(&b.as_str()))
}

impl HindsightMemory {
    /// Build a hindsight backend for `agent_alias` from the typed
    /// `[memory.hindsight]` config plus a per-agent `bank_id` override.
    ///
    /// This is the SINGLE canonical constructor: per-agent selection, the
    /// install-wide `memory.backend = "hindsight"` string path, CLI/migration,
    /// and status all reach the backend through here. It re-runs
    /// [`HindsightMemoryConfig::validate_self`] so an invalid endpoint (the
    /// refused third-party default, a plaintext remote) or bank template cannot
    /// be reached even when a caller skipped the per-agent config-load check.
    ///
    /// The bearer token is resolved from the environment variable named by
    /// `cfg.token_env` first (the recommended path, keeping secrets out of the
    /// committed config), then from an inline `cfg.token` as an escape hatch
    /// for non-committed local configs.
    pub fn from_config(
        cfg: &HindsightMemoryConfig,
        agent_alias: &str,
        bank_override: &str,
    ) -> Result<Self> {
        // Re-validate the typed config on EVERY construction path. The per-agent
        // enum triggers `Config::validate` -> `validate_self`, but the
        // install-wide string path and CLI/status construction do not, so the
        // trust boundary (refused default endpoint, plaintext remote, invalid
        // bank template) is enforced here rather than trusting the caller.
        if let Err(msg) = cfg.validate_self() {
            anyhow::bail!("memory backend 'hindsight' configuration is invalid: {msg}");
        }
        let token = std::env::var(&cfg.token_env)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                cfg.token
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .with_context(|| {
                format!(
                    "memory backend 'hindsight' requires a bearer token: set env {} \
                     (or an inline [memory.hindsight] token in a non-committed local config)",
                    cfg.token_env
                )
            })?;
        let base_url = cfg.base_url.trim().trim_end_matches('/').to_string();
        let bank = cfg.bank_for(agent_alias, bank_override);
        // Shared/system banks come from typed config only (the legacy env vars
        // are bridged into that typed config at load, so they are already
        // validated against every agent's private bank). A bank colliding with
        // the private bank (or, for system, the shared bank) is dropped so
        // private writes can never leak into a shared tier. The authoritative
        // cross-agent collision check runs at config load (`validate_self` +
        // install-wide `Config` validation); this per-instance drop is
        // defense-in-depth for the constructing instance.
        let shared_bank = resolve_secondary_bank(cfg.shared_bank_configured(), &[bank.as_str()]);
        let system_bank = resolve_secondary_bank(
            cfg.system_bank_configured(),
            &[bank.as_str(), shared_bank.as_deref().unwrap_or_default()],
        );
        let default_top_k = if cfg.top_k == 0 {
            DEFAULT_HINDSIGHT_TOP_K
        } else {
            cfg.top_k
        };

        Ok(Self {
            alias: agent_alias.to_string(),
            base_url,
            bank,
            shared_bank,
            system_bank,
            token,
            default_top_k,
            client: build_client(cfg.timeout_secs),
        })
    }

    /// The resolved private bank id (server namespaces it further, e.g.
    /// `u6--<bank>`). Writes always go here.
    #[must_use]
    pub fn bank(&self) -> &str {
        &self.bank
    }

    /// The optional shared/family bank merged into recall/list and written by
    /// the `shared_memory_store` tool.
    #[must_use]
    pub fn shared_bank(&self) -> Option<&str> {
        self.shared_bank.as_deref()
    }

    /// The optional system bank merged into recall/list and written by the
    /// `system_memory_store` tool.
    #[must_use]
    pub fn system_bank(&self) -> Option<&str> {
        self.system_bank.as_deref()
    }

    /// Construct a `HindsightMemory` directly, for tests in this and dependent
    /// crates (e.g. the shared/system write tools). Bypasses env/config
    /// resolution so tests can point at a mock server with explicit banks.
    #[doc(hidden)]
    #[must_use]
    pub fn for_test(
        alias: &str,
        base_url: &str,
        bank: &str,
        shared_bank: Option<&str>,
        system_bank: Option<&str>,
        token: &str,
    ) -> Self {
        Self {
            alias: alias.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            bank: bank.to_string(),
            shared_bank: shared_bank.map(str::to_string),
            system_bank: system_bank.map(str::to_string),
            token: token.to_string(),
            default_top_k: DEFAULT_HINDSIGHT_TOP_K,
            client: build_client(DEFAULT_HINDSIGHT_TIMEOUT_SECS),
        }
    }

    /// URL of the private bank's memories collection (retain/write path). The
    /// bank is percent-encoded as a path segment so a configurable override
    /// containing reserved URL bytes (`/`, `?`, `#`, space) cannot write to a
    /// different path than recall/list read from (which already encode).
    fn memories_url(&self) -> String {
        self.memories_url_for(&self.bank)
    }

    /// URL of a named bank's memories collection, with the bank percent-encoded
    /// as a single path segment. The write path uses this so it encodes the
    /// bank identically to the recall/list read paths.
    fn memories_url_for(&self, bank: &str) -> String {
        format!(
            "{}/v1/default/banks/{}/memories",
            self.base_url,
            encode_segment(bank)
        )
    }

    /// URL of a single memory item, used by the invalidate (soft-delete) PATCH.
    /// Both the bank name and the server-provided memory id are percent-encoded
    /// as path segments so a value containing reserved URL bytes cannot break
    /// out of its segment.
    fn memory_item_url_for(&self, bank: &str, id: &str) -> String {
        format!(
            "{}/v1/default/banks/{}/memories/{}",
            self.base_url,
            encode_segment(bank),
            encode_segment(id)
        )
    }

    /// Soft-delete (invalidate) a memory item in `bank` by id via
    /// `PATCH .../memories/{id}` with `{"state":"invalidated"}`. Returns
    /// `Ok(true)` when the server accepted the invalidation, `Ok(false)` for a
    /// `404` (already gone / unknown id) so retention/hygiene degrade
    /// gracefully. Other non-success statuses surface as an error; an empty id
    /// is a no-op.
    async fn invalidate_in_bank(&self, bank: &str, id: &str) -> Result<bool> {
        if id.trim().is_empty() {
            return Ok(false);
        }
        let resp = self
            .client
            .patch(self.memory_item_url_for(bank, id))
            .bearer_auth(&self.token)
            .json(&InvalidateBody {
                state: "invalidated",
            })
            .send()
            .await
            .context("hindsight invalidate request failed")?;
        let status = resp.status();
        if status.is_success() {
            return Ok(true);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let body = bounded_error_body(resp).await;
        anyhow::bail!("hindsight invalidate returned HTTP {status}: {body}");
    }

    fn recall_url_for(&self, bank: &str) -> String {
        format!(
            "{}/v1/default/banks/{}/memories/recall",
            self.base_url,
            encode_segment(bank)
        )
    }

    fn list_url_for(&self, bank: &str) -> String {
        format!(
            "{}/v1/default/banks/{}/memories/list",
            self.base_url,
            encode_segment(bank)
        )
    }

    /// Recall against a single named bank.
    async fn recall_bank(&self, bank: &str, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let body = RecallBody { query, limit };
        let resp = self
            .client
            .post(self.recall_url_for(bank))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .context("hindsight recall request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = bounded_error_body(resp).await;
            anyhow::bail!("hindsight recall returned HTTP {status}: {body}");
        }
        let parsed: RecallResponse =
            read_json_capped(resp, "hindsight recall returned unparseable JSON").await?;
        Ok(parsed
            .results
            .into_iter()
            .map(|r| {
                let score = r.scores.as_ref().and_then(final_score);
                Self::to_entry(r.id, r.text, r.context, r.mentioned_at, &r.tags, score)
            })
            .collect())
    }

    /// Explicit retain into a NAMED bank, used by the shared/system write tools.
    ///
    /// Unlike [`Memory::store`] (which always targets the private `self.bank`),
    /// this posts to `bank` and stamps the item with the writer alias plus a
    /// `tier:<tier>` tag for auditability. `tier` is a short marker such as
    /// `"shared"` or `"system"` describing which tool wrote it. Empty content is
    /// a no-op (mirrors `store`). This is the ONLY path that writes a
    /// non-private bank; automatic per-turn consolidation never calls it.
    pub async fn store_to_bank(
        &self,
        bank: &str,
        key: &str,
        content: &str,
        category: MemoryCategory,
        tier: &str,
    ) -> Result<()> {
        if content.trim().is_empty() {
            return Ok(());
        }
        let context_owned = if key.trim().is_empty() {
            category.to_string()
        } else {
            key.to_string()
        };
        let mut tags = Self::tags_for(&category);
        tags.push(format!("author:{}", self.alias));
        if !tier.trim().is_empty() {
            tags.push(format!("tier:{tier}"));
        }
        let body = RetainBody {
            items: vec![RetainItem {
                content,
                context: Some(context_owned.as_str()),
                tags,
            }],
            is_async: false,
        };
        let resp = self
            .client
            .post(self.memories_url_for(bank))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .context("hindsight shared/system retain request failed")?;
        let status = resp.status();
        if !status.is_success() {
            // Bound and single-line the remote body exactly like the private
            // retain/recall/list paths so a large or multiline shared/system
            // error body cannot flood model-visible output or logs.
            let body = bounded_error_body(resp).await;
            anyhow::bail!("hindsight shared/system retain returned HTTP {status}: {body}");
        }
        Ok(())
    }

    /// List a single named bank.
    async fn list_bank(&self, bank: &str) -> Result<Vec<MemoryEntry>> {
        let resp = self
            .client
            .get(self.list_url_for(bank))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("hindsight list request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = bounded_error_body(resp).await;
            anyhow::bail!("hindsight list returned HTTP {status}: {body}");
        }
        let parsed: ListResponse =
            read_json_capped(resp, "hindsight list returned unparseable JSON").await?;
        Ok(parsed
            .items
            .into_iter()
            .map(|i| Self::to_entry(i.id, i.text, i.context, i.mentioned_at, &i.tags, None))
            .collect())
    }
}

// ── Wire types (validated against the live tokengate hindsight API) ──

#[derive(serde::Serialize)]
struct RetainItem<'a> {
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<&'a str>,
    tags: Vec<String>,
}

#[derive(serde::Serialize)]
struct RetainBody<'a> {
    items: Vec<RetainItem<'a>>,
    #[serde(rename = "async")]
    is_async: bool,
}

#[derive(serde::Serialize)]
struct RecallBody<'a> {
    query: &'a str,
    limit: usize,
}

/// Body of the invalidate (soft-delete) PATCH: sets the item's lifecycle state.
#[derive(serde::Serialize)]
struct InvalidateBody<'a> {
    state: &'a str,
}

// The recall score object's primary field is literally "final" (a Rust
// keyword), so read it out of the raw JSON value rather than deriving a struct.
fn final_score(v: &serde_json::Value) -> Option<f64> {
    v.get("final").and_then(serde_json::Value::as_f64)
}

#[derive(Deserialize)]
struct RecallResult {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    mentioned_at: Option<String>,
    /// The retain-time tags (`["zeroclaw", <category>]`, plus optional
    /// `author:`/`tier:` meta tags on shared/system writes). Used to decode the
    /// row's real `MemoryCategory` so the dedup gates can see it.
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    scores: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RecallResponse {
    #[serde(default)]
    results: Vec<RecallResult>,
}

#[derive(Deserialize)]
struct ListItem {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    mentioned_at: Option<String>,
    /// Same retain-time tags the recall path exposes; decoded into the entry's
    /// `MemoryCategory` (see [`RecallResult::tags`]).
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct ListResponse {
    #[serde(default)]
    items: Vec<ListItem>,
    #[serde(default)]
    total: Option<u64>,
}

/// A caller-supplied `[since, until]` recall window, parsed once into typed
/// RFC 3339 instants at the read boundary.
///
/// Hindsight has no server-side time predicate, so the driver enforces the
/// window client-side. Parsing the CALLER'S bounds up front (rather than
/// per-row, lexically) means an invalid `since`/`until` is a clear, typed
/// error instead of a silent byte comparison, and every kept/dropped decision
/// is made against real instants. Empty/whitespace bounds are treated as
/// absent, matching the established backends. Mirrors the typed RFC 3339
/// validation the SQLite/Lucid/Markdown backends already apply.
#[derive(Clone, Copy, Default)]
struct TimeWindow {
    since: Option<DateTime<FixedOffset>>,
    until: Option<DateTime<FixedOffset>>,
}

impl TimeWindow {
    /// Parse and validate the caller-supplied bounds. An empty/whitespace bound
    /// is absent; a non-empty bound that is not RFC 3339 is a hard error; and
    /// `since` must not be after `until`.
    fn parse(since: Option<&str>, until: Option<&str>) -> Result<Self> {
        let since = Self::parse_bound("since", since)?;
        let until = Self::parse_bound("until", until)?;
        if let (Some(s), Some(u)) = (since, until)
            && s > u
        {
            anyhow::bail!("'since' must not be after 'until'");
        }
        Ok(Self { since, until })
    }

    fn parse_bound(field: &str, raw: Option<&str>) -> Result<Option<DateTime<FixedOffset>>> {
        raw.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                DateTime::parse_from_rfc3339(s)
                    .with_context(|| format!("invalid '{field}' date (expected RFC 3339): {s:?}"))
            })
            .transpose()
    }

    /// Whether either bound is present. When neither is, the window imposes no
    /// constraint and undatable rows are kept.
    fn is_bounded(&self) -> bool {
        self.since.is_some() || self.until.is_some()
    }
}

impl HindsightMemory {
    /// Categories whose rows are durable global knowledge when they carry no
    /// session binding: a `core`/`daily` fact with no session is long-term
    /// knowledge meant to be recallable from any session, not a per-session
    /// artifact. Mirrors the SQLite backend's single source of truth so the
    /// remote read boundary enforces the same session semantics.
    const DURABLE_GLOBAL_CATEGORIES: [MemoryCategory; 2] =
        [MemoryCategory::Core, MemoryCategory::Daily];

    /// When a recall carries a session/time discriminator the remote cannot
    /// evaluate itself, over-fetch this multiple of the caller's limit as the
    /// FIRST page before filtering client-side. Subsequent pages double the
    /// fetch until the window is filled or the ceiling is hit. Mirrors the
    /// scoped-memory wrapper's over-fetch pattern.
    const RECALL_SCOPED_OVERFETCH: usize = 4;

    /// Hard ceiling on the per-bank fetch limit while paging a scoped recall to
    /// fill its window. Bounds the escalation so a window that can never be
    /// filled (e.g. the bank holds far more foreign-session/out-of-window rows
    /// than the limit) still terminates after a bounded number of doublings
    /// instead of walking the entire bank. The response byte cap
    /// ([`MAX_REMOTE_BODY_BYTES`]) is the second backstop.
    const RECALL_SCOPED_FETCH_CEILING: usize = 512;

    fn tags_for(category: &MemoryCategory) -> Vec<String> {
        vec!["zeroclaw".to_string(), category.to_string()]
    }

    /// Retain-time tags for a write: the `["zeroclaw", <category>]` markers
    /// plus a `session:<id>` discriminator when the write is session-scoped.
    /// Round-tripping the session through a tag (the same channel the category
    /// already uses) lets the remote read boundary re-derive and ENFORCE the
    /// originating session, so conversation memory written under one session
    /// cannot be recalled into another. `category_from_tags` ignores the
    /// `key:value` session tag, so category decode is unaffected.
    fn tags_for_write(category: &MemoryCategory, session_id: Option<&str>) -> Vec<String> {
        let mut tags = Self::tags_for(category);
        if let Some(sid) = session_id.map(str::trim).filter(|s| !s.is_empty()) {
            tags.push(format!("session:{sid}"));
        }
        tags
    }

    /// Decode the originating session id a write stamped via
    /// [`Self::tags_for_write`] (`session:<id>`), so recalled/listed entries
    /// carry the real session instead of `None`. Returns `None` when the row
    /// has no session tag (a durable-global or legacy row).
    fn session_from_tags(tags: &[String]) -> Option<String> {
        tags.iter().find_map(|t| {
            t.trim()
                .strip_prefix("session:")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    }

    /// Whether a row is durable global knowledge (a `core`/`daily` row with no
    /// session binding), and therefore recallable from any session.
    fn is_durable_global(category: &MemoryCategory) -> bool {
        Self::DURABLE_GLOBAL_CATEGORIES.contains(category)
    }

    /// Whether `entry` passes the session gate for a recall/list scoped to
    /// `filter_session`. Mirrors the SQLite backend's rule: with a session
    /// filter, keep rows bound to that exact session PLUS durable-global
    /// `core`/`daily` rows that carry no session; without a filter, keep
    /// everything. This is the privacy boundary: a session-scoped
    /// conversation row from another session is dropped.
    fn passes_session_gate(entry: &MemoryEntry, filter_session: Option<&str>) -> bool {
        match filter_session {
            None => true,
            Some(sid) => {
                entry.session_id.as_deref() == Some(sid)
                    || (entry.session_id.is_none() && Self::is_durable_global(&entry.category))
            }
        }
    }

    /// Whether `entry.timestamp` falls within the inclusive `[since, until]`
    /// window carried by `bounds`.
    ///
    /// Fails CLOSED: whenever a bound is present, a row whose timestamp is
    /// empty or not parseable as RFC 3339 is EXCLUDED, because it cannot be
    /// shown to satisfy a window the remote could not enforce server-side.
    /// Only when NO bound is supplied is an undatable row kept - there is no
    /// window to fall outside of, so the caller asked for everything. Parseable
    /// timestamps compare as instants against the (already validated) typed
    /// bounds; there is no lexical fallback, so a malformed date can never
    /// slip through a byte comparison.
    fn passes_time_range(entry: &MemoryEntry, bounds: &TimeWindow) -> bool {
        if !bounds.is_bounded() {
            return true;
        }
        let Ok(ts) = DateTime::parse_from_rfc3339(entry.timestamp.trim()) else {
            // A bound is present but this row carries no parseable instant:
            // it cannot be placed inside the window, so exclude it.
            return false;
        };
        if let Some(since) = bounds.since
            && ts < since
        {
            return false;
        }
        if let Some(until) = bounds.until
            && ts > until
        {
            return false;
        }
        true
    }

    /// Apply the session and time discriminators to a fetched result set at
    /// the read boundary, then truncate to `limit`. The remote returned rows
    /// ranked by relevance (recall) or recency (list); this preserves that
    /// order while dropping rows that fall outside the requested session or
    /// `[since, until]` window, enforcing the same scoping the local backends
    /// apply server-side in SQL.
    fn filter_scoped(
        entries: Vec<MemoryEntry>,
        session_id: Option<&str>,
        window: &TimeWindow,
        limit: usize,
    ) -> Vec<MemoryEntry> {
        entries
            .into_iter()
            .filter(|e| Self::passes_session_gate(e, session_id))
            .filter(|e| Self::passes_time_range(e, window))
            .take(limit)
            .collect()
    }

    /// Decode a row's real [`MemoryCategory`] from the tags the driver itself
    /// wrote via [`Self::tags_for`] (`["zeroclaw", <category>]`, plus optional
    /// `author:`/`tier:` meta tags on shared/system writes). This is the reverse
    /// of `tags_for`: it MUST round-trip the exact strings `MemoryCategory`'s
    /// `Display` emits (`core`/`daily`/`conversation`, else the custom name).
    ///
    /// Skips the fixed `zeroclaw` marker and any `key:value` meta tag (e.g.
    /// `author:and`, `tier:shared`), then takes the first remaining tag as the
    /// category. Falls back to `Core` when no category tag is present, matching
    /// the historical behavior for untagged rows.
    fn category_from_tags(tags: &[String]) -> MemoryCategory {
        tags.iter()
            .map(|t| t.trim())
            .find(|t| !t.is_empty() && *t != "zeroclaw" && !t.contains(':'))
            .map_or(MemoryCategory::Core, |t| match t {
                "core" => MemoryCategory::Core,
                "daily" => MemoryCategory::Daily,
                "conversation" => MemoryCategory::Conversation,
                other => MemoryCategory::Custom(other.to_string()),
            })
    }

    fn to_entry(
        id: Option<String>,
        text: Option<String>,
        context: Option<String>,
        mentioned_at: Option<String>,
        tags: &[String],
        score: Option<f64>,
    ) -> MemoryEntry {
        MemoryEntry {
            id: id.clone().unwrap_or_default(),
            key: id.unwrap_or_default(),
            content: text.unwrap_or_default(),
            category: Self::category_from_tags(tags),
            timestamp: mentioned_at.unwrap_or_default(),
            // Recover the originating session the write stamped into the tags
            // (`session:<id>`), so a materialized entry carries the real
            // session instead of `None`. Durable-global/legacy rows have no
            // session tag and stay `None`.
            session_id: Self::session_from_tags(tags),
            score,
            namespace: context.unwrap_or_else(|| "default".to_string()),
            importance: None,
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }
    }
}

#[async_trait]
impl Memory for HindsightMemory {
    fn name(&self) -> &str {
        "hindsight"
    }

    fn as_shared_writable(&self) -> Option<&dyn SharedWritable> {
        Some(self)
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        // Skip empty and auto-save bookkeeping keys' empty content.
        if content.trim().is_empty() {
            return Ok(());
        }
        let context_owned = if key.trim().is_empty() {
            category.to_string()
        } else {
            key.to_string()
        };
        // Round-trip the originating session through the tags so the remote
        // read boundary can re-derive and enforce it: conversation memory
        // written under one session must not be recallable from another.
        let body = RetainBody {
            items: vec![RetainItem {
                content,
                context: Some(context_owned.as_str()),
                tags: Self::tags_for_write(&category, session_id),
            }],
            is_async: false,
        };
        let resp = self
            .client
            .post(self.memories_url())
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .context("hindsight retain request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = bounded_error_body(resp).await;
            anyhow::bail!("hindsight retain returned HTTP {status}: {body}");
        }
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let effective_limit = if limit == 0 {
            self.default_top_k
        } else {
            limit
        };
        // Validate the caller-supplied window ONCE, up front: an invalid
        // `since`/`until` is a clear typed error here, not a silent per-row
        // lexical comparison deeper in the filter.
        let window = TimeWindow::parse(since, until)?;
        let normalized = super::traits::normalize_recent_recall_query(query);
        // Hindsight recall needs a query; for recent/empty queries fall back to
        // list, which applies the same session gate.
        if normalized.trim().is_empty() {
            return self
                .list(None, session_id)
                .await
                .map(|entries| Self::filter_scoped(entries, session_id, &window, effective_limit));
        }
        // Hindsight has no server-side session/time predicate, so ENFORCE the
        // discriminators client-side at the read boundary. Without this, a
        // conversation row from session A could surface in session B's prompt -
        // a live privacy boundary crossing.
        let scoped = session_id.is_some() || window.is_bounded();
        // Recall this agent's own private bank first and apply the FULL scope:
        // the session gate (drop foreign-session conversation rows, keep durable
        // global core/daily) plus the time window. When scoped, PAGE to fill the
        // window: hindsight ranks by relevance, so a window dominated by
        // foreign-session or out-of-window rows could otherwise fill a single
        // over-fetch and hide valid rows ranked lower. The recall API is
        // limit-only (no cursor/offset), so the only paging it admits is to
        // request a larger limit and re-filter: escalate until the window is
        // filled, the remote is exhausted (fewer rows than asked, so no more),
        // or a hard ceiling is reached. The response byte cap
        // ([`MAX_REMOTE_BODY_BYTES`]) is the second backstop on fetch size.
        let mut entries = if !scoped {
            let raw = self
                .recall_bank(&self.bank, normalized, effective_limit)
                .await?;
            Self::filter_scoped(raw, session_id, &window, effective_limit)
        } else {
            let ceiling = Self::RECALL_SCOPED_FETCH_CEILING.max(effective_limit);
            let mut fetch_limit = effective_limit
                .saturating_mul(Self::RECALL_SCOPED_OVERFETCH)
                .min(ceiling);
            loop {
                let raw = self
                    .recall_bank(&self.bank, normalized, fetch_limit)
                    .await?;
                let exhausted = raw.len() < fetch_limit;
                let filtered = Self::filter_scoped(raw, session_id, &window, effective_limit);
                if filtered.len() >= effective_limit || exhausted || fetch_limit >= ceiling {
                    break filtered;
                }
                fetch_limit = fetch_limit.saturating_mul(2).min(ceiling);
            }
        };
        // Shared/system read-only banks, if set, are merged in (never written
        // through this path; both tiers are readable by every agent). These are
        // CROSS-AGENT tiers: their rows are written via `store_to_bank`, which
        // stamps no `session:<id>` tag, so they are inherently session-less like
        // durable-global core/daily. The session gate therefore does NOT apply
        // to them - a shared/system row must stay visible regardless of the
        // caller's session; only the time window (which is a genuine recency
        // bound, not a privacy boundary) still filters them.
        let extra_fetch = if scoped {
            effective_limit.saturating_mul(Self::RECALL_SCOPED_OVERFETCH)
        } else {
            effective_limit
        };
        let mut merged_any = false;
        for extra in [self.shared_bank.as_deref(), self.system_bank.as_deref()]
            .into_iter()
            .flatten()
        {
            let extra_entries = self.recall_bank(extra, normalized, extra_fetch).await?;
            entries.extend(
                extra_entries
                    .into_iter()
                    .filter(|e| Self::passes_time_range(e, &window))
                    .take(effective_limit),
            );
            merged_any = true;
        }
        if merged_any {
            // Highest score first, then keep the top slice.
            entries.sort_by(|a, b| {
                b.score
                    .unwrap_or(f64::MIN)
                    .partial_cmp(&a.score.unwrap_or(f64::MIN))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            entries.truncate(effective_limit);
        }
        Ok(entries)
    }

    async fn get(&self, _key: &str) -> Result<Option<MemoryEntry>> {
        // Hindsight has no key-addressed get; recall/list are the read paths.
        Ok(None)
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Hindsight's list endpoint has no server-side category/session
        // predicate, so enforce the discriminators client-side at the read
        // boundary.
        //
        // Private bank: keep only the requested category (when given) and apply
        // the session gate (exact session match plus durable-global core/daily).
        let mut entries: Vec<MemoryEntry> = self
            .list_bank(&self.bank)
            .await?
            .into_iter()
            .filter(|e| category.is_none_or(|c| &e.category == c))
            .filter(|e| Self::passes_session_gate(e, session_id))
            .collect();
        // Shared/system read-only tiers (if set) are merged in. These are
        // CROSS-AGENT, session-less tiers (writes stamp no `session:<id>`), so
        // the session gate does NOT apply - a shared/system row stays visible
        // regardless of the caller's session. The category filter still applies
        // (it is a content classifier, not a privacy boundary).
        for extra in [self.shared_bank.as_deref(), self.system_bank.as_deref()]
            .into_iter()
            .flatten()
        {
            entries.extend(
                self.list_bank(extra)
                    .await?
                    .into_iter()
                    .filter(|e| category.is_none_or(|c| &e.category == c)),
            );
        }
        Ok(entries)
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        // Hindsight has no key-addressed delete: `key` is the memory id the
        // read paths surface (both `id` and `key` on a `MemoryEntry` are set to
        // the server id in `to_entry`). Soft-delete it in the private bank via
        // the invalidate PATCH so a first-class backend never silently declines
        // a removal. A `404` maps to `Ok(false)` so hygiene degrades
        // gracefully.
        self.invalidate_in_bank(&self.bank, key).await
    }

    async fn forget_for_agent(&self, key: &str, _agent_id: &str) -> Result<bool> {
        // The bank is the per-agent scope, so agent_id is redundant here: the
        // private bank already isolates this agent's rows. Forget by id in the
        // private bank, same as `forget`.
        self.invalidate_in_bank(&self.bank, key).await
    }

    async fn count(&self) -> Result<usize> {
        let resp = self
            .client
            .get(self.list_url_for(&self.bank))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("hindsight count request failed")?;
        if !resp.status().is_success() {
            return Ok(0);
        }
        // Cap the count body too: a hostile list endpoint must not exhaust
        // memory here either. A parse/over-cap failure degrades to 0.
        let parsed: ListResponse =
            read_json_capped(resp, "hindsight count returned unparseable JSON")
                .await
                .unwrap_or(ListResponse {
                    items: Vec::new(),
                    total: None,
                });
        Ok(parsed.total.map_or(parsed.items.len(), |t| {
            usize::try_from(t).unwrap_or(usize::MAX)
        }))
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/version", self.base_url);
        match self.client.get(url).bearer_auth(&self.token).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> Result<()> {
        // The bank is the per-agent scope, so agent_id stamping is a no-op here.
        self.store(key, content, category, session_id).await
    }

    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Private-only foundation: each agent's bank IS its isolation boundary,
        // and config load rejects any cross-agent private-bank collision (see
        // `Config::validate`), so distinct agents can never resolve to the same
        // bank. There is therefore no cross-agent read to gate here and the
        // allowlist is intentionally not consulted; cross-agent shared/system
        // reads (and their authorization) are introduced by the tiers slice.
        self.recall(query, limit, session_id, since, until).await
    }
}

impl ::zeroclaw_api::attribution::Attributable for HindsightMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(
            ::zeroclaw_api::attribution::MemoryKind::Hindsight,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl SharedWritable for HindsightMemory {
    fn shared_bank(&self) -> Option<&str> {
        self.shared_bank.as_deref()
    }

    fn system_bank(&self) -> Option<&str> {
        self.system_bank.as_deref()
    }

    async fn store_to_shared(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
    ) -> Result<()> {
        let bank = self
            .shared_bank
            .as_deref()
            .context("no shared bank configured")?;
        self.store_to_bank(bank, key, content, category, "shared")
            .await
    }

    async fn store_to_system(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
    ) -> Result<()> {
        let bank = self
            .system_bank
            .as_deref()
            .context("no system bank configured")?;
        self.store_to_bank(bank, key, content, category, "system")
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A HindsightMemory pointed at a mock server with a fixed token/bank and
    /// no shared bank, so tests exercise the store/recall/list HTTP mapping
    /// without any environment or live network.
    fn memory_for(base_url: &str, bank: &str) -> HindsightMemory {
        HindsightMemory {
            alias: "tester".to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            bank: bank.to_string(),
            shared_bank: None,
            system_bank: None,
            token: "test-token".to_string(),
            default_top_k: DEFAULT_HINDSIGHT_TOP_K,
            client: build_client(DEFAULT_HINDSIGHT_TIMEOUT_SECS),
        }
    }

    #[test]
    fn bank_for_prefers_override_then_template() {
        let cfg = HindsightMemoryConfig {
            bank_template: "zeroclaw-{agent}".to_string(),
            ..HindsightMemoryConfig::default()
        };
        // No override -> template with {agent} substituted.
        assert_eq!(cfg.bank_for("clawdia", ""), "zeroclaw-clawdia");
        assert_eq!(cfg.bank_for("clawdia", "   "), "zeroclaw-clawdia");
        // Explicit override wins verbatim (trimmed).
        assert_eq!(cfg.bank_for("clawdia", " team-shared "), "team-shared");
    }

    #[test]
    fn from_config_reads_token_env_then_bank_template() {
        // A unique env var name avoids cross-test interference.
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_A";
        // SAFETY: single-threaded test; set + read within this test only.
        unsafe { std::env::set_var(env_name, "env-token-123") };
        let cfg = HindsightMemoryConfig {
            base_url: "https://example.test/hs/".to_string(),
            bank_template: "zeroclaw-{agent}".to_string(),
            token_env: env_name.to_string(),
            ..HindsightMemoryConfig::default()
        };
        let mem = HindsightMemory::from_config(&cfg, "scout", "").expect("construct");
        assert_eq!(mem.bank(), "zeroclaw-scout");
        assert_eq!(mem.token, "env-token-123");
        // Trailing slash on base_url is trimmed for clean URL joins.
        assert_eq!(mem.base_url, "https://example.test/hs");
        unsafe { std::env::remove_var(env_name) };
    }

    #[test]
    fn from_config_falls_back_to_inline_token_when_env_absent() {
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_ABSENT";
        unsafe { std::env::remove_var(env_name) };
        let cfg = HindsightMemoryConfig {
            base_url: "https://memory.example.com/hs".to_string(),
            token_env: env_name.to_string(),
            token: Some("inline-token-xyz".to_string()),
            ..HindsightMemoryConfig::default()
        };
        let mem = HindsightMemory::from_config(&cfg, "scout", "pinned-bank").expect("construct");
        assert_eq!(mem.token, "inline-token-xyz");
        assert_eq!(mem.bank(), "pinned-bank");
    }

    #[test]
    fn from_config_rejects_refused_default_endpoint() {
        // The single canonical constructor re-validates the typed config, so
        // the refused third-party default endpoint cannot be reached even on a
        // path (CLI/install-wide/status) that skipped `Config::validate`. A
        // token is present so the failure is unambiguously the endpoint.
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_DEFAULT_EP";
        unsafe { std::env::set_var(env_name, "tok") };
        let cfg = HindsightMemoryConfig {
            // Default base_url is the refused third-party endpoint.
            token_env: env_name.to_string(),
            ..HindsightMemoryConfig::default()
        };
        let err = HindsightMemory::from_config(&cfg, "scout", "").unwrap_err();
        assert!(
            err.to_string().contains("operator-owned"),
            "constructor must refuse the default endpoint: {err}"
        );
        unsafe { std::env::remove_var(env_name) };
    }

    #[test]
    fn from_config_rejects_plaintext_remote_endpoint() {
        // Plaintext http:// to a remote host is refused by the constructor's
        // re-validation on every path.
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_PLAINTEXT";
        unsafe { std::env::set_var(env_name, "tok") };
        let cfg = HindsightMemoryConfig {
            base_url: "http://memory.example.com/hs".to_string(),
            token_env: env_name.to_string(),
            ..HindsightMemoryConfig::default()
        };
        let err = HindsightMemory::from_config(&cfg, "scout", "").unwrap_err();
        assert!(
            err.to_string().contains("https"),
            "constructor must refuse a plaintext remote endpoint: {err}"
        );
        unsafe { std::env::remove_var(env_name) };
    }

    #[test]
    fn from_config_errors_without_any_token() {
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_MISSING";
        unsafe { std::env::remove_var(env_name) };
        let cfg = HindsightMemoryConfig {
            base_url: "https://memory.example.com/hs".to_string(),
            token_env: env_name.to_string(),
            token: None,
            ..HindsightMemoryConfig::default()
        };
        let err = HindsightMemory::from_config(&cfg, "scout", "").unwrap_err();
        assert!(
            err.to_string().contains(env_name),
            "error should name the missing env var: {err}"
        );
    }

    #[tokio::test]
    async fn store_posts_retain_payload_to_bank() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_partial_json(json!({
                "items": [{ "content": "PURPLE-OTTER-42", "context": "fact" }],
                "async": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.store("fact", "PURPLE-OTTER-42", MemoryCategory::Core, None)
            .await
            .expect("store should succeed against the mock retain endpoint");
    }

    #[tokio::test]
    async fn recall_maps_results_to_entries_with_final_score() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_partial_json(json!({ "query": "otter", "limit": 3 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {
                        "id": "m1",
                        "text": "PURPLE-OTTER-42",
                        "context": "fact",
                        "mentioned_at": "2026-07-10T00:00:00Z",
                        "scores": { "final": 0.87 }
                    }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall("otter", 3, None, None, None)
            .await
            .expect("recall should succeed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "PURPLE-OTTER-42");
        assert_eq!(hits[0].id, "m1");
        assert_eq!(hits[0].namespace, "fact");
        assert!((hits[0].score.unwrap() - 0.87).abs() < 1e-9);
    }

    #[tokio::test]
    async fn empty_query_recall_falls_back_to_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "a", "text": "first", "context": "c1" },
                    { "id": "b", "text": "second", "context": "c2" }
                ],
                "total": 2
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        // "*" normalizes to the empty/recent query, which lists instead of recalling.
        let hits = mem.recall("*", 10, None, None, None).await.expect("list");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].content, "first");
    }

    #[tokio::test]
    async fn store_surfaces_http_error_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let err = mem
            .store("k", "v", MemoryCategory::Core, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("500"),
            "error should carry the HTTP status: {err}"
        );
    }

    #[tokio::test]
    async fn empty_content_store_is_a_noop() {
        // No mock mounted: if store tried to hit the network it would fail.
        let mem = memory_for("http://127.0.0.1:1", "zeroclaw-test");
        mem.store("k", "   ", MemoryCategory::Core, None)
            .await
            .expect("empty content should short-circuit without any request");
    }

    #[test]
    fn category_from_tags_decodes_zeroclaw_category() {
        // Round-trip: whatever tags_for writes, category_from_tags must decode.
        // Order is not guaranteed by the server, and the fixed "zeroclaw" marker
        // must be ignored.
        for cat in [
            MemoryCategory::Core,
            MemoryCategory::Daily,
            MemoryCategory::Conversation,
            MemoryCategory::Custom("project".to_string()),
        ] {
            let tags = HindsightMemory::tags_for(&cat);
            assert_eq!(HindsightMemory::category_from_tags(&tags), cat);
            // Reversed order still decodes the same category.
            let mut rev = tags.clone();
            rev.reverse();
            assert_eq!(HindsightMemory::category_from_tags(&rev), cat);
        }
    }

    #[test]
    fn category_from_tags_ignores_meta_and_falls_back_to_core() {
        // Shared/system writes append author:/tier: meta tags; those must be
        // skipped so the real category tag wins.
        assert_eq!(
            HindsightMemory::category_from_tags(&[
                "zeroclaw".into(),
                "daily".into(),
                "author:and".into(),
                "tier:shared".into(),
            ]),
            MemoryCategory::Daily
        );
        // No category tag present -> Core fallback (historical behavior).
        assert_eq!(
            HindsightMemory::category_from_tags(&["zeroclaw".into()]),
            MemoryCategory::Core
        );
        assert_eq!(
            HindsightMemory::category_from_tags(&[]),
            MemoryCategory::Core
        );
    }

    #[tokio::test]
    async fn recall_decodes_category_from_tags() {
        // Regression for the dedup bug: a recalled row tagged "daily" must
        // decode to MemoryCategory::Daily (and "core"/"conversation" likewise),
        // so the downstream dedup gates can see the real category instead of
        // every row reading back as Core.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": "d1", "text": "daily summary", "tags": ["daily", "zeroclaw"],
                      "scores": { "final": 0.9 } },
                    { "id": "c1", "text": "core fact", "tags": ["zeroclaw", "core"],
                      "scores": { "final": 0.8 } },
                    { "id": "v1", "text": "chat bit", "tags": ["conversation", "zeroclaw"],
                      "scores": { "final": 0.7 } },
                    { "id": "u1", "text": "untagged", "scores": { "final": 0.6 } }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall("anything", 10, None, None, None)
            .await
            .expect("recall should succeed");
        let by_id = |id: &str| {
            hits.iter()
                .find(|h| h.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
                .category
                .clone()
        };
        assert_eq!(by_id("d1"), MemoryCategory::Daily);
        assert_eq!(by_id("c1"), MemoryCategory::Core);
        assert_eq!(by_id("v1"), MemoryCategory::Conversation);
        // Untagged rows keep the historical Core fallback.
        assert_eq!(by_id("u1"), MemoryCategory::Core);
    }

    #[tokio::test]
    async fn list_decodes_category_from_tags() {
        // The list path (used by empty/recent-query recall) must decode tags too.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "d1", "text": "daily summary", "tags": ["daily", "zeroclaw"] },
                    { "id": "c1", "text": "core fact", "tags": ["zeroclaw", "core"] }
                ],
                "total": 2
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let items = mem.list(None, None).await.expect("list should succeed");
        assert_eq!(items[0].category, MemoryCategory::Daily);
        assert_eq!(items[1].category, MemoryCategory::Core);
    }

    /// A memory pointed at `base_url` whose client carries a very short
    /// timeout, so a delayed/never-responding mock trips the deadline quickly
    /// instead of blocking the test for the production default.
    fn memory_with_short_timeout(base_url: &str, bank: &str) -> HindsightMemory {
        HindsightMemory {
            alias: "tester".to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            bank: bank.to_string(),
            shared_bank: None,
            system_bank: None,
            token: "test-token".to_string(),
            default_top_k: DEFAULT_HINDSIGHT_TOP_K,
            // 1s is comfortably above the mock's response latency floor yet far
            // below the ~30s artificial delay, so the deadline is what fires.
            client: build_client(1),
        }
    }

    #[tokio::test]
    async fn recall_times_out_against_a_stalled_server() {
        // A read path (recall) against a server that never responds in time must
        // surface a typed timeout error, not hang the caller. wiremock delays
        // the response past the client deadline.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "results": [] }))
                    .set_delay(std::time::Duration::from_secs(30)),
            )
            .mount(&server)
            .await;

        let mem = memory_with_short_timeout(&server.uri(), "zeroclaw-test");
        let err = mem
            .recall("otter", 3, None, None, None)
            .await
            .expect_err("a stalled recall must return a timeout error, not hang");
        // The underlying reqwest error must be a timeout (reqwest reports it via
        // `is_timeout()` on the chained source).
        assert!(
            err.chain().any(|cause| {
                cause
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout)
            }),
            "expected a reqwest timeout in the error chain, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn store_times_out_against_a_stalled_server() {
        // The write path (store) must be bounded too: a never-responding retain
        // endpoint returns a typed timeout error instead of parking the turn.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "ok": true }))
                    .set_delay(std::time::Duration::from_secs(30)),
            )
            .mount(&server)
            .await;

        let mem = memory_with_short_timeout(&server.uri(), "zeroclaw-test");
        let err = mem
            .store("fact", "PURPLE-OTTER-42", MemoryCategory::Core, None)
            .await
            .expect_err("a stalled store must return a timeout error, not hang");
        assert!(
            err.chain().any(|cause| {
                cause
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout)
            }),
            "expected a reqwest timeout in the error chain, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn urls_percent_encode_bank_and_id_segments() {
        // A bank name and memory id with reserved URL bytes must be encoded as
        // path segments so they cannot break out into extra path/query parts.
        let mem = memory_for("https://host.test", "team/space room");
        let recall = mem.recall_url_for("team/space room");
        assert!(
            recall.contains("team%2Fspace%20room") || recall.contains("team%2Fspace+room"),
            "bank segment must be percent-encoded: {recall}"
        );
        assert!(
            !recall.contains("banks/team/space"),
            "raw slash must not leak into the path: {recall}"
        );
        // The WRITE path (retain) must encode the bank identically, so a
        // configurable override cannot POST to a different path than reads.
        let write = mem.memories_url_for("team/space room");
        assert!(
            write.contains("team%2Fspace%20room") || write.contains("team%2Fspace+room"),
            "write-path bank segment must be percent-encoded: {write}"
        );
        assert!(
            !write.contains("banks/team/space"),
            "raw slash must not leak into the write path: {write}"
        );
        // The invalidate PATCH url must encode both the bank and the memory id.
        let item = mem.memory_item_url_for("bank", "id/with?reserved#chars");
        assert!(
            !item.contains("id/with?reserved#chars"),
            "id segment must be percent-encoded: {item}"
        );
        assert!(
            item.contains("id%2Fwith%3Freserved%23chars"),
            "id reserved bytes must be encoded: {item}"
        );
    }

    #[tokio::test]
    async fn store_encodes_bank_on_the_write_path() {
        // Regression: a bank override with reserved bytes must POST to the
        // encoded path (same as recall/list read), not a raw-interpolated one.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/team%2Fspace/memories"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "team/space");
        mem.store("fact", "PURPLE-OTTER-42", MemoryCategory::Core, None)
            .await
            .expect("store must hit the percent-encoded bank path");
    }

    #[tokio::test]
    async fn forget_issues_invalidate_patch_to_private_bank() {
        // forget(id) must PATCH .../memories/{id} on the PRIVATE bank with
        // {"state":"invalidated"} and map a 2xx to Ok(true).
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/mem-123"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_partial_json(json!({ "state": "invalidated" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(
            mem.forget("mem-123").await.expect("forget should succeed"),
            "a 2xx invalidate must report the row removed"
        );
    }

    #[tokio::test]
    async fn forget_maps_404_to_false() {
        // An unknown/already-gone id returns 404 -> Ok(false), so hygiene
        // degrades gracefully instead of erroring.
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(!mem.forget("missing").await.expect("404 must not error"));
    }

    #[tokio::test]
    async fn forget_surfaces_server_error() {
        // A 5xx is a real failure and must surface as an error (not a silent
        // false), so the caller can retry/log.
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/boom"))
            .respond_with(ResponseTemplate::new(500).set_body_string("kaboom"))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let err = mem.forget("boom").await.unwrap_err();
        assert!(
            err.to_string().contains("500"),
            "5xx must surface the status: {err}"
        );
    }

    #[tokio::test]
    async fn forget_empty_id_is_a_noop() {
        // No mock mounted: an empty id must not fire a request.
        let mem = memory_for("http://127.0.0.1:1", "zeroclaw-test");
        assert!(!mem.forget("   ").await.expect("empty id short-circuits"));
    }

    #[tokio::test]
    async fn forget_for_agent_targets_private_bank_by_id() {
        // forget_for_agent ignores agent_id (the bank is the per-agent scope)
        // and invalidates by id in the private bank, same as forget.
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/mem-9"))
            .and(body_partial_json(json!({ "state": "invalidated" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(
            mem.forget_for_agent("mem-9", "any-agent")
                .await
                .expect("forget_for_agent should succeed")
        );
    }

    #[tokio::test]
    async fn recall_error_body_is_bounded_and_single_line() {
        // A large multi-line remote error body must be collapsed to one line
        // and truncated so it cannot flood logs or smuggle control chars into
        // the surfaced error.
        let server = MockServer::start().await;
        let huge = format!("line-one\nline-two\n{}", "X".repeat(4000));
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(500).set_body_string(huge))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let err = mem
            .recall("otter", 3, None, None, None)
            .await
            .expect_err("a 500 must surface as an error");
        let msg = err.to_string();
        assert!(msg.contains("truncated"), "body must be truncated: {msg}");
        assert!(!msg.contains('\n'), "body must be single-line: {msg:?}");
        // The bounded snippet plus the fixed prefix stay comfortably small.
        assert!(
            msg.len() < 700,
            "error message must be bounded: {}",
            msg.len()
        );
    }

    // ── B1: session / category / time scoping at the read boundary ──

    #[test]
    fn write_tags_round_trip_the_originating_session() {
        // A session-scoped write stamps `session:<id>` alongside the category
        // marker; a session-less write does not. The session tag must not
        // disturb category decode.
        let scoped = HindsightMemory::tags_for_write(&MemoryCategory::Conversation, Some("sess-A"));
        assert!(
            scoped.iter().any(|t| t == "session:sess-A"),
            "session-scoped write must stamp the session tag: {scoped:?}"
        );
        assert_eq!(
            HindsightMemory::session_from_tags(&scoped).as_deref(),
            Some("sess-A")
        );
        assert_eq!(
            HindsightMemory::category_from_tags(&scoped),
            MemoryCategory::Conversation,
            "session tag must not disturb category decode"
        );
        // Blank/absent session -> no tag, decodes back to None.
        let unscoped = HindsightMemory::tags_for_write(&MemoryCategory::Core, None);
        assert!(unscoped.iter().all(|t| !t.starts_with("session:")));
        assert_eq!(HindsightMemory::session_from_tags(&unscoped), None);
        let blank = HindsightMemory::tags_for_write(&MemoryCategory::Core, Some("  "));
        assert!(blank.iter().all(|t| !t.starts_with("session:")));
    }

    #[tokio::test]
    async fn store_persists_session_discriminator_in_tags() {
        // Production autosave writes Conversation memory with the active
        // session; that session must be persisted through the retain payload
        // tags so the read boundary can later enforce it.
        let server = MockServer::start().await;
        // Match on the serialized payload: the session must ride the tags as
        // `session:conv-A` (array elements aren't partial-matched element-wise
        // by body_partial_json, so assert on the raw JSON string instead).
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .and(body_string_contains("session:conv-A"))
            .and(body_string_contains("SECRET-IN-A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.store(
            "k",
            "SECRET-IN-A",
            MemoryCategory::Conversation,
            Some("conv-A"),
        )
        .await
        .expect("session-scoped store must post the session tag");
    }

    #[tokio::test]
    async fn materialized_entries_carry_real_session_not_none() {
        // A recalled row whose tags carry the originating session must
        // materialize with that session_id, not None.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": "m1", "text": "hi", "tags": ["zeroclaw", "conversation", "session:conv-A"],
                      "scores": { "final": 0.9 } }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall("hi", 5, Some("conv-A"), None, None)
            .await
            .expect("recall should succeed");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].session_id.as_deref(),
            Some("conv-A"),
            "materialized entry must carry the real session, not None"
        );
    }

    /// PRODUCTION-BOUNDARY regression: conversation memory written under one
    /// session must NEVER be recalled into another session's prompt. Memory
    /// injection passes the current session to `recall` specifically to keep
    /// conversation entries scoped; Hindsight must honor that at the remote
    /// read boundary. The mock bank returns BOTH sessions' rows (Hindsight has
    /// no server-side session predicate), so the driver itself must drop the
    /// foreign-session conversation row.
    #[tokio::test]
    async fn conversation_memory_cannot_cross_sessions() {
        let server = MockServer::start().await;
        // The remote bank holds a conversation row from session A, a
        // conversation row from session B, and a durable-global core fact with
        // no session. A recall scoped to session B returns everything (no
        // server-side filter), so the driver must enforce the session gate.
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": "a", "text": "SECRET-FROM-SESSION-A",
                      "tags": ["zeroclaw", "conversation", "session:conv-A"],
                      "scores": { "final": 0.99 } },
                    { "id": "b", "text": "hello-from-session-B",
                      "tags": ["zeroclaw", "conversation", "session:conv-B"],
                      "scores": { "final": 0.98 } },
                    { "id": "g", "text": "durable-global-fact",
                      "tags": ["zeroclaw", "core"],
                      "scores": { "final": 0.50 } }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall("anything", 10, Some("conv-B"), None, None)
            .await
            .expect("recall should succeed");

        let contents: Vec<&str> = hits.iter().map(|h| h.content.as_str()).collect();
        assert!(
            !contents.contains(&"SECRET-FROM-SESSION-A"),
            "session A's conversation row must NOT cross into session B: {contents:?}"
        );
        assert!(
            contents.contains(&"hello-from-session-B"),
            "session B's own conversation row must be recallable: {contents:?}"
        );
        assert!(
            contents.contains(&"durable-global-fact"),
            "durable-global core facts stay recallable across sessions: {contents:?}"
        );
    }

    #[tokio::test]
    async fn list_enforces_session_and_category_filters() {
        // Hindsight's list endpoint has no server-side predicate; the driver
        // must apply both the category and session filters client-side.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "a", "text": "conv A", "tags": ["zeroclaw", "conversation", "session:s-A"] },
                    { "id": "b", "text": "conv B", "tags": ["zeroclaw", "conversation", "session:s-B"] },
                    { "id": "g", "text": "core fact", "tags": ["zeroclaw", "core"] }
                ],
                "total": 3
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        // Session filter: only session B's row plus the durable-global core row.
        let scoped = mem
            .list(None, Some("s-B"))
            .await
            .expect("list should succeed");
        let scoped_ids: Vec<&str> = scoped.iter().map(|e| e.id.as_str()).collect();
        assert!(
            scoped_ids.contains(&"b"),
            "own session row kept: {scoped_ids:?}"
        );
        assert!(
            scoped_ids.contains(&"g"),
            "durable-global row kept: {scoped_ids:?}"
        );
        assert!(
            !scoped_ids.contains(&"a"),
            "foreign session row dropped: {scoped_ids:?}"
        );
        // Category filter narrows to conversation rows only.
        let convo = mem
            .list(Some(&MemoryCategory::Conversation), None)
            .await
            .expect("list should succeed");
        assert!(
            convo
                .iter()
                .all(|e| e.category == MemoryCategory::Conversation)
        );
        assert_eq!(convo.len(), 2);
    }

    #[tokio::test]
    async fn recall_enforces_time_window() {
        // A recall bounded by [since, until] must drop rows whose timestamp
        // falls outside the window, even though Hindsight returns them all.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": "old", "text": "too old", "mentioned_at": "2026-01-01T00:00:00Z",
                      "tags": ["zeroclaw", "core"], "scores": { "final": 0.9 } },
                    { "id": "mid", "text": "in window", "mentioned_at": "2026-06-15T00:00:00Z",
                      "tags": ["zeroclaw", "core"], "scores": { "final": 0.8 } },
                    { "id": "new", "text": "too new", "mentioned_at": "2026-12-31T00:00:00Z",
                      "tags": ["zeroclaw", "core"], "scores": { "final": 0.7 } }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall(
                "anything",
                10,
                None,
                Some("2026-06-01T00:00:00Z"),
                Some("2026-07-01T00:00:00Z"),
            )
            .await
            .expect("recall should succeed");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["mid"], "only the in-window row survives: {ids:?}");
    }

    /// Fail-closed regression: a row with a MISSING (empty) remote timestamp
    /// must be EXCLUDED when a bound is present - it cannot be shown to satisfy
    /// a window the remote could not enforce. (With no bound it would be kept;
    /// that is `recall_keeps_undated_rows_when_unbounded` below.)
    #[tokio::test]
    async fn recall_excludes_undated_row_when_bound_present() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": "dated", "text": "in window", "mentioned_at": "2026-06-15T00:00:00Z",
                      "tags": ["zeroclaw", "core"], "scores": { "final": 0.9 } },
                    { "id": "undated", "text": "no timestamp",
                      "tags": ["zeroclaw", "core"], "scores": { "final": 0.8 } }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall(
                "anything",
                10,
                None,
                Some("2026-06-01T00:00:00Z"),
                Some("2026-07-01T00:00:00Z"),
            )
            .await
            .expect("recall should succeed");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["dated"],
            "an undated row must fail closed under a bound: {ids:?}"
        );
    }

    /// Fail-closed regression: a row with a MALFORMED (non-RFC-3339) remote
    /// timestamp must be EXCLUDED when a bound is present - a byte comparison
    /// must never let `"not-a-real-date"` survive a window it cannot be placed
    /// in.
    #[tokio::test]
    async fn recall_excludes_malformed_timestamp_row_when_bound_present() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": "dated", "text": "in window", "mentioned_at": "2026-06-15T00:00:00Z",
                      "tags": ["zeroclaw", "core"], "scores": { "final": 0.9 } },
                    { "id": "garbage", "text": "bad date", "mentioned_at": "not-a-real-date",
                      "tags": ["zeroclaw", "core"], "scores": { "final": 0.8 } }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall(
                "anything",
                10,
                None,
                Some("2026-06-01T00:00:00Z"),
                Some("2026-07-01T00:00:00Z"),
            )
            .await
            .expect("recall should succeed");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["dated"],
            "a malformed-timestamp row must fail closed under a bound: {ids:?}"
        );
    }

    /// A malformed CALLER bound is a clear error, not a silent lexical
    /// comparison: the caller asked for a window the driver cannot honor, so
    /// recall must refuse rather than quietly return unfiltered rows.
    #[tokio::test]
    async fn recall_rejects_invalid_caller_bound() {
        let server = MockServer::start().await;
        // No recall route is mounted: a correct implementation rejects the bad
        // bound BEFORE any HTTP call, so the mock is never hit.
        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let err = mem
            .recall("anything", 10, None, Some("yesterday"), None)
            .await
            .expect_err("an unparseable caller bound must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("since") && msg.contains("RFC 3339"),
            "error should name the invalid bound and expected format: {msg}"
        );
    }

    /// Unbounded recall keeps an undated row: with no window there is nothing
    /// for it to fall outside of, so fail-closed exclusion must NOT apply.
    #[tokio::test]
    async fn recall_keeps_undated_rows_when_unbounded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": "undated", "text": "no timestamp",
                      "tags": ["zeroclaw", "core"], "scores": { "final": 0.8 } }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall("anything", 10, None, None, None)
            .await
            .expect("recall should succeed");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["undated"],
            "an undated row is kept when no bound is supplied: {ids:?}"
        );
    }

    /// WARNING regression: scoped recall PAGES to fill the window. A valid
    /// own-session row ranked BELOW a full first over-fetch page of
    /// foreign-session rows must still surface: the driver escalates the fetch
    /// (limit-only API, so a larger limit is the only paging shape) until the
    /// requested limit is satisfied. The mock answers the first page
    /// (`limit == 4`, the over-fetch of `limit 1`) with four foreign-session
    /// rows, and the escalated page (`limit == 8`) with the valid row appended,
    /// proving the second fetch happened and filled the window.
    #[tokio::test]
    async fn scoped_recall_pages_to_fill_window() {
        let server = MockServer::start().await;
        let foreign: Vec<serde_json::Value> = (0..4)
            .map(|i| {
                json!({
                    "id": format!("f{i}"), "text": "foreign",
                    "tags": ["zeroclaw", "conversation", "session:other"],
                    "scores": { "final": 0.9 - f64::from(i) * 0.01 }
                })
            })
            .collect();
        // First page (over-fetch of limit 1 -> 4): only foreign-session rows.
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .and(body_partial_json(json!({ "limit": 4 })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "results": foreign.clone() })),
            )
            .mount(&server)
            .await;
        // Escalated page (doubled to 8): the valid own-session row appears,
        // ranked below the four foreign rows.
        let mut with_valid = foreign;
        with_valid.push(json!({
            "id": "mine", "text": "valid own-session row",
            "tags": ["zeroclaw", "conversation", "session:mine"],
            "scores": { "final": 0.5 }
        }));
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .and(body_partial_json(json!({ "limit": 8 })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "results": with_valid })),
            )
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let hits = mem
            .recall("anything", 1, Some("mine"), None, None)
            .await
            .expect("recall should succeed");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["mine"],
            "paging must surface the valid row ranked below the first page: {ids:?}"
        );
    }

    // ── B2: byte-cap remote bodies while STREAMING, before materialization ──

    /// Core regression: `read_body_capped` must STOP reading at the cap. Proven
    /// by asserting on BYTES CONSUMED (the returned buffer length) rather than
    /// any final decoded string: a body far larger than the cap yields exactly
    /// `cap` bytes and the `truncated` flag, so at most `cap` bytes are ever
    /// accumulated regardless of how much the server streams.
    #[tokio::test]
    async fn read_body_capped_stops_at_the_cap() {
        let server = MockServer::start().await;
        // The server offers 2 MiB; the cap is 64 KiB. If reading did not stop
        // at the cap, the buffer would balloon to the full body.
        let cap = 64 * 1024;
        let huge = "X".repeat(2 * 1024 * 1024);
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_string(huge))
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .get(format!("{}/big", server.uri()))
            .send()
            .await
            .expect("request should succeed");
        let (bytes, truncated) = read_body_capped(resp, cap)
            .await
            .expect("capped read should succeed");
        assert!(truncated, "an over-cap body must report truncation");
        assert_eq!(
            bytes.len(),
            cap,
            "reading must STOP at the cap: consumed {} bytes, cap {cap}",
            bytes.len()
        );
    }

    #[tokio::test]
    async fn read_body_capped_returns_full_small_body() {
        // A body under the cap is returned whole and not marked truncated.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/small"))
            .respond_with(ResponseTemplate::new(200).set_body_string("tiny-body"))
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .get(format!("{}/small", server.uri()))
            .send()
            .await
            .expect("request should succeed");
        let (bytes, truncated) = read_body_capped(resp, MAX_REMOTE_BODY_BYTES)
            .await
            .expect("capped read should succeed");
        assert!(!truncated, "a small body must not be marked truncated");
        assert_eq!(bytes, b"tiny-body");
    }

    /// The SUCCESS JSON path must refuse an oversized body BEFORE parsing it,
    /// so a malfunctioning endpoint that streams a huge (even valid-looking)
    /// JSON body within the timeout cannot materialize it into memory. The
    /// oversized body here is > MAX_REMOTE_BODY_BYTES; recall must error with
    /// the cap message instead of decoding it.
    #[tokio::test]
    async fn recall_refuses_oversized_success_body_before_parsing() {
        let server = MockServer::start().await;
        // A syntactically valid JSON object padded far past the 1 MiB cap. If
        // the cap were applied only after buffering, this whole body would be
        // read and parsed; instead the streaming cap trips first.
        let padding = "A".repeat(MAX_REMOTE_BODY_BYTES + 512 * 1024);
        let body = format!(r#"{{"results": [], "junk": "{padding}"}}"#);
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let err = mem
            .recall("otter", 3, None, None, None)
            .await
            .expect_err("an oversized success body must be refused, not parsed");
        let msg = err.to_string();
        assert!(
            msg.contains("cap") && msg.contains("refused"),
            "oversized body must be refused at the streaming cap: {msg}"
        );
    }

    /// The ERROR path must also stop reading at its (smaller) cap: a huge error
    /// body is bounded without buffering the whole stream. Proven via the
    /// helper's byte accounting plus the surfaced snippet staying bounded.
    #[tokio::test]
    async fn error_body_is_capped_while_streaming() {
        let server = MockServer::start().await;
        // 4 MiB error body, far beyond the 8 KiB error cap.
        let huge = "E".repeat(4 * 1024 * 1024);
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(500).set_body_string(huge))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let err = mem
            .recall("otter", 3, None, None, None)
            .await
            .expect_err("a 500 must surface as an error");
        let msg = err.to_string();
        assert!(
            msg.contains("truncated"),
            "error body must be truncated: {msg}"
        );
        // The surfaced message stays tiny (bounded snippet), proving the 4 MiB
        // body was not materialized into the error string.
        assert!(
            msg.len() < 700,
            "error message must stay bounded regardless of body size: {}",
            msg.len()
        );
    }

    // ── Shared/system tier writes + read-merge ──

    /// A memory with explicit private + shared + system banks for the
    /// shared-write and read-merge tests.
    fn memory_with_tiers(
        base_url: &str,
        shared: Option<&str>,
        system: Option<&str>,
    ) -> HindsightMemory {
        HindsightMemory::for_test(
            "and",
            base_url,
            "zeroclaw-and",
            shared,
            system,
            "test-token",
        )
    }

    #[tokio::test]
    async fn store_to_bank_posts_to_named_bank_not_private() {
        let server = MockServer::start().await;
        // The named (shared) bank must receive the POST.
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-house/memories"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_partial_json(json!({
                "items": [{ "content": "trash Tuesday", "context": "trash_day" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;

        let mem = memory_with_tiers(&server.uri(), Some("zeroclaw-house"), None);
        mem.store_to_bank(
            "zeroclaw-house",
            "trash_day",
            "trash Tuesday",
            MemoryCategory::Core,
            "shared",
        )
        .await
        .expect("store_to_bank should hit the named bank");
    }

    #[tokio::test]
    async fn store_to_bank_tags_author_and_tier() {
        let server = MockServer::start().await;
        // Assert the retained item carries author:<alias> and tier:<tier> tags.
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-house/memories"))
            .and(body_partial_json(json!({
                "items": [{ "tags": ["zeroclaw", "core", "author:and", "tier:shared"] }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;

        let mem = memory_with_tiers(&server.uri(), Some("zeroclaw-house"), None);
        mem.store_to_bank("zeroclaw-house", "k", "v", MemoryCategory::Core, "shared")
            .await
            .expect("author + tier tags must be present");
    }

    #[tokio::test]
    async fn store_to_bank_empty_content_is_noop() {
        // No mock mounted: an empty write must not fire a request.
        let mem = memory_with_tiers("http://127.0.0.1:1", Some("zeroclaw-house"), None);
        mem.store_to_bank("zeroclaw-house", "k", "   ", MemoryCategory::Core, "shared")
            .await
            .expect("empty content should short-circuit");
    }

    #[tokio::test]
    async fn store_to_bank_error_body_is_bounded_and_single_line() {
        // The shared/system write path must bound a large multiline remote error
        // body exactly like the private retain/recall/list paths, so a failing
        // shared/system write cannot flood model-visible output or logs.
        let server = MockServer::start().await;
        let huge = format!("err-line-one\nerr-line-two\n{}", "Y".repeat(4000));
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-house/memories"))
            .respond_with(ResponseTemplate::new(500).set_body_string(huge))
            .mount(&server)
            .await;

        let mem = memory_with_tiers(&server.uri(), Some("zeroclaw-house"), None);
        let err = mem
            .store_to_bank("zeroclaw-house", "k", "v", MemoryCategory::Core, "shared")
            .await
            .expect_err("a 500 on the shared write must surface as an error");
        let msg = err.to_string();
        assert!(msg.contains("truncated"), "body must be truncated: {msg}");
        assert!(!msg.contains('\n'), "body must be single-line: {msg:?}");
        assert!(
            msg.len() < 700,
            "error message must be bounded: {}",
            msg.len()
        );
    }

    #[tokio::test]
    async fn store_still_targets_private_bank() {
        // Regression: the ordinary store path is unchanged and hits the
        // PRIVATE bank, never the shared one, even when a shared bank is set.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-and/memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;

        let mem = memory_with_tiers(&server.uri(), Some("zeroclaw-house"), None);
        mem.store("k", "private note", MemoryCategory::Core, None)
            .await
            .expect("store must target the private bank");
    }

    #[tokio::test]
    async fn shared_writable_store_to_shared_uses_configured_bank() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-house/memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;

        let mem = memory_with_tiers(
            &server.uri(),
            Some("zeroclaw-house"),
            Some("zeroclaw-system"),
        );
        // Exercise the SharedWritable trait surface the tools use.
        assert_eq!(mem.shared_bank(), Some("zeroclaw-house"));
        assert_eq!(mem.system_bank(), Some("zeroclaw-system"));
        SharedWritable::store_to_shared(&mem, "k", "v", MemoryCategory::Core)
            .await
            .expect("store_to_shared should hit the shared bank");
    }

    #[tokio::test]
    async fn store_to_shared_without_bank_errors() {
        let mem = memory_with_tiers("http://127.0.0.1:1", None, None);
        assert!(mem.shared_bank().is_none());
        let err = SharedWritable::store_to_shared(&mem, "k", "v", MemoryCategory::Core)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no shared bank configured"));
    }

    #[tokio::test]
    async fn recall_merges_system_bank_read_only() {
        let server = MockServer::start().await;
        // Private, shared, and system banks each answer recall; all three merge.
        for (bank, text) in [
            ("zeroclaw-and", "private-hit"),
            ("zeroclaw-house", "shared-hit"),
            ("zeroclaw-system", "system-hit"),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("/v1/default/banks/{bank}/memories/recall")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "results": [{ "id": bank, "text": text, "scores": { "final": 0.5 } }]
                })))
                .mount(&server)
                .await;
        }

        let mem = memory_with_tiers(
            &server.uri(),
            Some("zeroclaw-house"),
            Some("zeroclaw-system"),
        );
        let hits = mem
            .recall("anything", 10, None, None, None)
            .await
            .expect("recall merges all tiers");
        let texts: Vec<&str> = hits.iter().map(|h| h.content.as_str()).collect();
        assert!(texts.contains(&"private-hit"));
        assert!(texts.contains(&"shared-hit"));
        assert!(texts.contains(&"system-hit"));
    }
}
