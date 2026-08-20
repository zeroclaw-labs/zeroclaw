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
//! Deletion (`forget` / `forget_for_agent`): the `Memory` contract removes by
//! the SAME logical key `store` accepts (mirroring every other backend, e.g.
//! `SqliteMemory::forget` deletes `WHERE key = ?`). Hindsight stores that
//! caller key as `context` and assigns its OWN opaque item id server-side, so
//! `forget` cannot simply treat the caller's key as the id: it first resolves
//! `context == key` to the matching item id(s) via a list scan
//! ([`HindsightMemory::resolve_context_to_ids`]), then invalidates each
//! resolved id via `PATCH .../memories/{id}` with `state=invalidated` (a
//! soft-delete). `to_entry` surfaces the caller's original key on
//! `MemoryEntry::key` (from `context`) and the server id separately on
//! `MemoryEntry::id`, so a `store`/`recall`/`forget` round trip works with the
//! same key throughout. Hindsight v0.8.4 only allows curating (invalidating)
//! `world`/`experience` facts, not derived `observation` rows; a `forget` that
//! resolves to an `observation` item returns a clear error instead of silently
//! failing or bypassing the backend's own contract. Deletion targets the
//! private bank only - the same bank writes land in.
//!
//! Recall type filter: `recall_types` restricts recall to selected Hindsight
//! fact types (`experience`, `observation`, `world`); it is sent as the recall
//! body's `types` array and applied on BOTH the query and the recent/empty-query
//! (`list`) paths. Empty = no filter (all types). The effective value comes
//! solely from the typed `[memory.hindsight] recall_types` field: the generic
//! `ZEROCLAW_memory__hindsight__recall_types` override and the legacy short form
//! `ZC_HINDSIGHT_RECALL_TYPES` are both resolved into that typed field during
//! config loading (the legacy name is bridged in
//! `zeroclaw_config::env_overrides`, validated by the shared
//! `HindsightMemoryConfig::normalize_recall_types` so an invalid fact type is a
//! startup error), so this constructor never re-reads the environment and there
//! is a single observable source of truth visible to config inspection.
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
//!
//! Retention scope (IMPORTANT): explicit deletion (`forget` / `forget_for_agent`,
//! documented above) covers a caller that holds a memory id. *Automatic*
//! time-based retention is NOT wired to this backend: [`crate::hygiene::run_if_due`]
//! prunes the local SQLite/markdown stores under the workspace directory
//! directly and never routes expiry through the [`Memory`] trait, so an
//! expired Hindsight Daily item is not auto-invalidated remotely.
//! Backend-neutral automatic retention (routing hygiene expiry through
//! `forget`) is deliberately out of scope here and tracked as follow-up work;
//! do not claim automatic remote retention for this driver until that wiring
//! lands.

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

/// Whether `segment` is the reserved single- or double-dot path segment
/// (`.` or `..`). `urlencoding::encode` passes `.` through unchanged (it is an
/// unreserved URL byte), but HTTP clients and servers normalize `.`/`..` path
/// segments during URL resolution. An id of exactly `.` or `..` sent as the
/// final path segment of an authenticated PATCH could therefore be resolved to
/// a different resource than `.../memories/{id}` (e.g. the bank collection
/// itself, or its parent). Reject the id outright rather than relying on
/// percent-encoding to protect a byte it deliberately leaves untouched.
fn is_reserved_dot_segment(segment: &str) -> bool {
    segment == "." || segment == ".."
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
    /// Optional recall-side fact-type filter. When non-empty, each recall body
    /// carries a `types` array (Hindsight fact types: `experience`,
    /// `observation`, `world`) so the server returns only those record types.
    /// Empty (default) sends nothing, keeping the recall body byte-identical to
    /// the historical `{query, limit}` shape.
    recall_types: Vec<String>,
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
            .field("recall_types", &self.recall_types)
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

        // Recall type filter: the effective value comes ONLY from the typed
        // config field. Empty means "no filter" (all types). The legacy
        // `ZC_HINDSIGHT_RECALL_TYPES` override is NOT re-read here: it is bridged
        // into `cfg.recall_types` during config loading
        // (`env_overrides::apply_env_overrides`) alongside the generic
        // `ZEROCLAW_memory__hindsight__recall_types` form, so there is exactly
        // ONE observable source of truth (typed `Config`, visible to config
        // inspection and drift) instead of a second env read that could silently
        // disagree with reported config.
        let recall_types = cfg.recall_types.clone();

        Ok(Self {
            alias: agent_alias.to_string(),
            base_url,
            bank,
            shared_bank,
            system_bank,
            token,
            default_top_k,
            recall_types,
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
            recall_types: Vec::new(),
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
    /// is a no-op. A literal `.`/`..` id segment is refused: `urlencoding`
    /// leaves those bytes unchanged, but URL/path normalization collapses them,
    /// so an unencoded dot-segment id could re-route this authenticated PATCH
    /// away from the intended `.../memories/{id}` resource.
    async fn invalidate_in_bank(&self, bank: &str, id: &str) -> Result<bool> {
        if id.trim().is_empty() {
            return Ok(false);
        }
        if is_reserved_dot_segment(id.trim()) {
            anyhow::bail!(
                "hindsight invalidate refused: id {id:?} is a reserved '.'/'..' path segment"
            );
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

    /// `Memory::forget` / `forget_for_agent` entry point: resolve the
    /// caller-facing logical `key` (the `context` `store` wrote) to the
    /// matching Hindsight item id(s) in `bank`, then invalidate each. An empty
    /// key or no match is a no-op (`Ok(false)`), matching every other
    /// backend's "nothing removed" contract. If any matching row is a derived
    /// `observation` - which Hindsight v0.8.4 does not allow curating - this
    /// returns a clear error instead of silently skipping it or leaving it
    /// behind after removing sibling rows, so a caller never believes a key
    /// was fully forgotten when part of it is actually undeletable.
    async fn forget_by_key_in_bank(&self, bank: &str, key: &str) -> Result<bool> {
        if key.trim().is_empty() {
            return Ok(false);
        }
        let (ids, blocked_observation) = self.resolve_context_to_ids(bank, key).await?;
        if blocked_observation {
            anyhow::bail!(
                "hindsight forget refused: key {key:?} matches a derived 'observation' fact, \
                 which Hindsight does not allow curating (invalidating); only 'world'/'experience' \
                 facts can be deleted"
            );
        }
        if ids.is_empty() {
            return Ok(false);
        }
        let mut removed_any = false;
        for id in ids {
            if self.invalidate_in_bank(bank, &id).await? {
                removed_any = true;
            }
        }
        Ok(removed_any)
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
        let body = RecallBody {
            query,
            limit,
            types: &self.recall_types,
        };
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

    /// Raw list of a single named bank's items, unfiltered by `recall_types`.
    /// The forget key-resolution path needs to see every fact type (including
    /// `observation`) so it can distinguish "no such key" from "key exists but
    /// names an undeletable observation", which the `recall_types`-filtered
    /// [`Self::list_bank`] view would otherwise hide.
    async fn list_bank_raw(&self, bank: &str) -> Result<Vec<ListItem>> {
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
        Ok(parsed.items)
    }

    /// List a single named bank.
    ///
    /// The Hindsight list endpoint has no server-side `types` filter, so when
    /// `recall_types` is configured the filter is applied LOCALLY on each row's
    /// fact type here. This makes the recent-recall (empty/`*` query) path -
    /// which falls back to `list` - honor the same type restriction as the
    /// query-based `recall` path, instead of returning every fact type. A row
    /// with no server-provided type is KEPT so unlabeled/legacy history is never
    /// silently dropped by the filter.
    async fn list_bank(&self, bank: &str) -> Result<Vec<MemoryEntry>> {
        Ok(self
            .list_bank_raw(bank)
            .await?
            .into_iter()
            .filter(|i| self.fact_type_allowed(i.fact_type.as_deref()))
            .map(|i| Self::to_entry(i.id, i.text, i.context, i.mentioned_at, &i.tags, None))
            .collect())
    }

    /// Resolve a caller-facing logical `key` (the `context` value `store`
    /// wrote) to the Hindsight item id(s) currently carrying it in `bank`, so
    /// `forget`/`forget_for_agent` can invalidate by the SAME key `store`
    /// accepted rather than misinterpreting the key as an opaque server id.
    ///
    /// Returns `(deletable_ids, blocked_observation)`: `deletable_ids` are
    /// `world`/`experience` item ids matching `key` (Hindsight v0.8.4 only
    /// allows curating those two fact types), and `blocked_observation` is
    /// `true` when at least one matching row is a derived `observation` that
    /// the curation PATCH cannot remove. A caller sees `blocked_observation`
    /// even when other deletable rows also matched, so `forget` can refuse the
    /// whole operation rather than silently leaving an undeletable row behind.
    async fn resolve_context_to_ids(&self, bank: &str, key: &str) -> Result<(Vec<String>, bool)> {
        let items = self.list_bank_raw(bank).await?;
        let mut ids = Vec::new();
        let mut blocked_observation = false;
        for item in items {
            if item.context.as_deref() != Some(key) {
                continue;
            }
            let Some(id) = item.id else { continue };
            match item.fact_type.as_deref() {
                Some("observation") => blocked_observation = true,
                _ => ids.push(id),
            }
        }
        Ok((ids, blocked_observation))
    }

    /// Whether a row with the given server fact type passes the configured
    /// `recall_types` filter. No configured filter admits everything; a row
    /// whose type is absent is admitted (legacy/unlabeled rows are never
    /// silently dropped); otherwise the type must be in the configured set.
    fn fact_type_allowed(&self, fact_type: Option<&str>) -> bool {
        if self.recall_types.is_empty() {
            return true;
        }
        match fact_type {
            None => true,
            Some(ft) => {
                let ft = ft.trim();
                self.recall_types.iter().any(|t| t == ft)
            }
        }
    }

    /// Dedicated write-time lookup of the COMPLETE set of ACTIVE (valid)
    /// PRIVATE Daily rows, used ONLY by the per-turn Daily dedup gate
    /// ([`Memory::list_own_daily_history`]).
    ///
    /// Deliberately does NOT reuse [`Self::list_bank`], because the dedup
    /// decision must see every active private-Daily record - not the rows
    /// recall chooses to PRESENT. Three differences from `list_bank`:
    ///
    ///   - `state=valid`: the Hindsight list route INCLUDES invalidated rows by
    ///     default. Without this filter an explicitly invalidated Daily match
    ///     could suppress its own replacement, so an invalidation would leave no
    ///     active record. Restricting to `valid` means dedup only ever compares
    ///     against rows that still exist.
    ///   - COMPLETE pagination: the route returns only the first page
    ///     (`PRIVATE_DAILY_PAGE_SIZE` rows) by default, so a valid duplicate
    ///     beyond page 1 would be invisible and the same summary appended again.
    ///     This pages until the server's reported `total` is covered (a short
    ///     page also terminates, so a server that omits `total` still stops).
    ///   - NO `recall_types` filter: S3 applies the `recall_types`
    ///     recall-PRESENTATION predicate per row inside `list_bank`
    ///     ([`Self::fact_type_allowed`]). Write-time dedup must be independent of
    ///     how recall filters what it shows, so this path never calls
    ///     `fact_type_allowed`. The recall path is untouched: recent-recall
    ///     still routes through `list`/`list_bank`, which keeps S3's filter.
    ///
    /// Daily narrowing is expressed to the server as a `tags=daily` filter AND
    /// re-enforced locally on the decoded category, so a server that ignores the
    /// tag param still yields only Daily rows.
    async fn list_private_daily_active(&self) -> Result<Vec<MemoryEntry>> {
        let mut entries: Vec<MemoryEntry> = Vec::new();
        let mut offset: usize = 0;
        loop {
            let resp = self
                .client
                .get(self.list_url_for(&self.bank))
                .bearer_auth(&self.token)
                // Validity + Daily narrowing. `state=valid` excludes invalidated
                // rows; `tags=daily` asks the server to return only Daily rows.
                // The literal "daily" is the retain-time category tag (see
                // `tags_for`), so it round-trips through `category_from_tags`.
                .query(&[("state", "valid"), ("tags", DAILY_CATEGORY_TAG)])
                // Complete pagination over the active private Daily set.
                .query(&[("limit", PRIVATE_DAILY_PAGE_SIZE), ("offset", offset)])
                .send()
                .await
                .context("hindsight private-daily list request failed")?;
            let status = resp.status();
            if !status.is_success() {
                let body = bounded_error_body(resp).await;
                anyhow::bail!("hindsight private-daily list returned HTTP {status}: {body}");
            }
            let parsed: ListResponse = read_json_capped(
                resp,
                "hindsight private-daily list returned unparseable JSON",
            )
            .await?;
            let page_len = parsed.items.len();
            let total = parsed.total;
            // Re-enforce the Daily category locally (defense in depth if the
            // server ignores `tags=daily`). NOTE: intentionally no
            // `fact_type_allowed` call - the write-time set must not inherit
            // S3's recall_types presentation filter.
            entries.extend(
                parsed
                    .items
                    .into_iter()
                    .map(|i| Self::to_entry(i.id, i.text, i.context, i.mentioned_at, &i.tags, None))
                    .filter(|e| e.category == MemoryCategory::Daily),
            );
            offset += page_len;
            // Terminate on a short/empty page (no more rows) or once the
            // server's reported total is covered.
            let covered_total = total.is_some_and(|t| offset as u64 >= t);
            if page_len < PRIVATE_DAILY_PAGE_SIZE || page_len == 0 || covered_total {
                break;
            }
        }
        Ok(entries)
    }
}

/// Rows requested per page when exhaustively paging the active private Daily
/// dedup candidate set. The Hindsight list route caps a single page at 100
/// rows, so the dedicated write-time query pages until the server's reported
/// `total` is covered.
const PRIVATE_DAILY_PAGE_SIZE: usize = 100;

/// Retain-time category tag for Daily rows (`tags_for(Daily)` pushes this), sent
/// as the server-side `tags=` narrowing filter on the write-time dedup query.
const DAILY_CATEGORY_TAG: &str = "daily";

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
    /// Server-side fact-type filter. Empty slice serializes to nothing (via
    /// `skip_serializing_if`), so the default recall body stays byte-identical
    /// to the historical `{query, limit}` shape. When populated, the live
    /// Hindsight API returns only these fact types.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    types: &'a [String],
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
    /// The server's Hindsight fact type (`experience`/`observation`/`world`).
    /// The list endpoint has no server-side `types` filter, so the recent-recall
    /// (empty/`*` query) path filters on this value locally to honor the
    /// configured `recall_types`. Absent on older rows; a missing type is kept
    /// so unlabeled history is never silently dropped.
    #[serde(default, rename = "type")]
    fact_type: Option<String>,
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

    /// Sort a merged, cross-tier entry set NEWEST FIRST with a TOTAL, reproducible
    /// order, so truncating to a limit afterwards keeps the most recent rows
    /// regardless of which tier (private/shared/system) they came from.
    ///
    /// Primary key: the RFC 3339 timestamp parsed to an instant, descending. A
    /// row whose timestamp is empty or unparseable is treated as the OLDEST
    /// (sorts last), so a row that cannot be dated never displaces a genuinely
    /// recent row - and, critically, a parseable vs unparseable comparison is
    /// decided by parse success rather than a meaningless lexical byte compare
    /// (`"not-a-date"` must NOT outrank a real 2026 date). Deterministic
    /// tiebreak, applied in order, so ties (equal instants, or two undatable
    /// rows) still yield ONE stable order: (1) the row id ascending, then (2)
    /// the content. Two rows are only truly equal when id and content match, so
    /// the order is total and reproducible run to run rather than depending on
    /// the banks' merge/append order.
    fn sort_recent_stable(entries: &mut [MemoryEntry]) {
        entries.sort_by(|a, b| {
            let at = chrono::DateTime::parse_from_rfc3339(a.timestamp.trim()).ok();
            let bt = chrono::DateTime::parse_from_rfc3339(b.timestamp.trim()).ok();
            // Newest first. `Option` orders `None < Some`, which already places
            // an undatable row (None) below any dated row; comparing b vs a then
            // yields descending instants with undatable rows sinking to the end.
            bt.cmp(&at)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.content.cmp(&b.content))
        });
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
        // `key` carries the caller-facing logical key (`store`'s `context`),
        // NOT the server-assigned item id, so a `store(key, ...)` /
        // `recall`/`list` / `forget(key)` round trip uses the same key
        // throughout - matching every other `Memory` backend's contract
        // (`forget` removes by the key `store` accepted). The opaque server
        // id is exposed separately on `id`, and `forget`/`forget_for_agent`
        // internally resolve a caller key back to it (see
        // `resolve_context_to_ids`).
        let context = context.unwrap_or_else(|| "default".to_string());
        MemoryEntry {
            id: id.unwrap_or_default(),
            key: context.clone(),
            content: text.unwrap_or_default(),
            category: Self::category_from_tags(tags),
            timestamp: mentioned_at.unwrap_or_default(),
            // Recover the originating session the write stamped into the tags
            // (`session:<id>`), so a materialized entry carries the real
            // session instead of `None`. Durable-global/legacy rows have no
            // session tag and stay `None`.
            session_id: Self::session_from_tags(tags),
            score,
            namespace: context,
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
        // Recent/omitted (or bare `*`) recall: there is no query to rank by, so
        // fall back to the recency-ordered `list` of every configured bank.
        // `list` already applies the private session gate + category filter and
        // merges the (session-less) shared/system tiers, so here we only bound
        // by the optional time window.
        if normalized.trim().is_empty() {
            let mut entries: Vec<MemoryEntry> = self
                .list(None, session_id)
                .await?
                .into_iter()
                .filter(|e| Self::passes_time_range(e, &window))
                .collect();
            // Merge before truncate: sort the MERGED set
            // across all tiers by recency BEFORE truncating to `limit`. `list`
            // appends private rows first, then shared, then system; truncating
            // that raw order would let a private history of `limit`+ rows fill
            // the budget and deterministically discard every newer shared/system
            // row, hiding fresh cross-agent or system guidance. Sorting newest
            // first here guarantees a newer shared/system row survives the cut.
            Self::sort_recent_stable(&mut entries);
            entries.truncate(effective_limit);
            return Ok(entries);
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

    async fn list_own_daily_history(&self) -> Result<Vec<MemoryEntry>> {
        // Write-time candidate lookup for the per-turn Daily dedup gate. Unlike
        // `list` (which merges the shared/system read tiers) AND unlike
        // `list_bank` (which is the recall-PRESENTATION path: first page only,
        // includes invalidated rows, and applies S3's `recall_types` filter),
        // this uses a DEDICATED query over the agent's PRIVATE bank that returns
        // the COMPLETE set of ACTIVE (valid) Daily rows with NO recall_types
        // filter (see `list_private_daily_active`). This guarantees:
        //   - a shared/system Daily row can never suppress a private Daily write
        //     (private bank only);
        //   - an invalidated match can never suppress its replacement
        //     (`state=valid`);
        //   - a valid duplicate beyond the first page is never missed
        //     (complete pagination);
        //   - a Daily row excluded from recall by `recall_types` is still
        //     compared at write time (no presentation filter here), while the
        //     recall path keeps S3's filter untouched.
        self.list_private_daily_active().await
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        // `key` is the SAME logical key `store` accepted (Hindsight's
        // `context`), matching the `Memory::forget` contract every other
        // backend implements. Resolve it to the underlying Hindsight item
        // id(s) in the private bank, then invalidate each one; empty/no-match
        // keys are a no-op, and a match that resolves to a derived
        // `observation` (which Hindsight's curation PATCH cannot remove) is a
        // clear error rather than a silent no-op or a misdirected PATCH.
        self.forget_by_key_in_bank(&self.bank, key).await
    }

    async fn forget_for_agent(&self, key: &str, _agent_id: &str) -> Result<bool> {
        // The bank is the per-agent scope, so agent_id is redundant here: the
        // private bank already isolates this agent's rows. Forget by key in the
        // private bank, same as `forget`.
        self.forget_by_key_in_bank(&self.bank, key).await
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

    /// Process-wide lock serializing EVERY Hindsight test that mutates a
    /// process-global environment variable. Rust's test harness runs tests on
    /// multiple threads by default and `std::env::set_var`/`remove_var` mutate
    /// shared process state, so without a single shared lock two env-mutating
    /// tests can interleave and observe each other's values (reproducible with
    /// `--test-threads=16`). A `std::sync::Mutex` is used so the guard can be
    /// held across the synchronous `from_config` env reads without an await
    /// point; the guarded data is `()` so a panicking test cannot corrupt it.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Acquire the shared env lock, tolerant of a prior test having poisoned it
    /// by panicking while holding it. There is no invariant to protect (the
    /// guarded value is `()`), so recover the guard and keep serializing.
    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// RAII guard that sets or clears one environment variable for the duration
    /// of a test and restores its PRIOR value (or prior absence) on drop - even
    /// if the test panics. Paired with [`env_test_lock`], this replaces the
    /// unenforceable "single-threaded test" comment with real serialization plus
    /// panic-safe restoration.
    struct EnvVarGuard {
        key: String,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        /// Set `key` to `value`, remembering the prior value to restore on drop.
        fn set(key: &str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: every caller holds `env_test_lock()`, so no other test
            // thread reads or writes the environment concurrently.
            unsafe { std::env::set_var(key, value) };
            Self {
                key: key.to_string(),
                prev,
            }
        }

        /// Ensure `key` is unset for the test, remembering the prior value.
        fn unset(key: &str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: see `set`.
            unsafe { std::env::remove_var(key) };
            Self {
                key: key.to_string(),
                prev,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: callers hold `env_test_lock()` for the guard's lifetime.
            match &self.prev {
                Some(v) => unsafe { std::env::set_var(&self.key, v) },
                None => unsafe { std::env::remove_var(&self.key) },
            }
        }
    }

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
            recall_types: Vec::new(),
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
        // Serialize on the shared env lock; the RAII guard restores the token
        // var (and its prior absence) even on panic.
        let _lock = env_test_lock();
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_A";
        let _tok = EnvVarGuard::set(env_name, "env-token-123");
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
    }

    #[test]
    fn from_config_falls_back_to_inline_token_when_env_absent() {
        let _lock = env_test_lock();
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_ABSENT";
        let _tok = EnvVarGuard::unset(env_name);
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
        let _lock = env_test_lock();
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_DEFAULT_EP";
        let _tok = EnvVarGuard::set(env_name, "tok");
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
    }

    #[test]
    fn from_config_rejects_plaintext_remote_endpoint() {
        // Plaintext http:// to a remote host is refused by the constructor's
        // re-validation on every path.
        let _lock = env_test_lock();
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_PLAINTEXT";
        let _tok = EnvVarGuard::set(env_name, "tok");
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
    }

    #[test]
    fn from_config_errors_without_any_token() {
        let _lock = env_test_lock();
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_MISSING";
        let _tok = EnvVarGuard::unset(env_name);
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
    async fn recall_without_filter_omits_types_field() {
        // Default (no recall_types): the recall body must be byte-identical to
        // the historical {query, limit} shape - no `types` key serialized.
        let server = MockServer::start().await;
        let captured: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = captured.clone();
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(move |req: &wiremock::Request| {
                *sink.lock().unwrap() = Some(req.body_json::<serde_json::Value>().unwrap());
                ResponseTemplate::new(200).set_body_json(json!({ "results": [] }))
            })
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.recall("otter", 3, None, None, None)
            .await
            .expect("recall should succeed");

        let body = captured.lock().unwrap().clone().expect("body captured");
        assert_eq!(body, json!({ "query": "otter", "limit": 3 }));
        assert!(
            body.get("types").is_none(),
            "no-filter recall must not serialize a `types` field: {body}"
        );
    }

    #[tokio::test]
    async fn recall_with_filter_sends_types_array() {
        // With recall_types configured, the body carries the exact `types`
        // array the live Hindsight API honors server-side.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_partial_json(json!({
                "query": "otter",
                "limit": 3,
                "types": ["observation"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {
                        "id": "m1",
                        "text": "PURPLE-OTTER-42",
                        "type": "observation",
                        "context": "fact",
                        "mentioned_at": "2026-07-10T00:00:00Z",
                        "scores": { "final": 0.91 }
                    }
                ]
            })))
            .mount(&server)
            .await;

        let mut mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.recall_types = vec!["observation".to_string()];
        let hits = mem
            .recall("otter", 3, None, None, None)
            .await
            .expect("filtered recall should succeed");
        // The mock only matches when `types: ["observation"]` is present, so a
        // returned hit proves the filter was sent on the wire.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "m1");
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

    #[test]
    fn recall_types_env_and_config_share_one_normalizing_validator() {
        // The config-load bridge routes the `ZC_HINDSIGHT_RECALL_TYPES` env
        // override through `HindsightMemoryConfig::normalize_recall_types` - the
        // SAME validator `validate_self` applies to the TOML value. Rather than
        // mutate the process-global env var (which would race parallel
        // `from_config` tests), assert the shared validator directly: an invalid
        // env-style token is rejected exactly like an invalid TOML token, and
        // whitespace normalizes identically for both sources.
        use zeroclaw_config::schema::HindsightMemoryConfig;
        // env-style comma split (what the config bridge feeds the validator)
        // with a typo.
        let env_tokens: Vec<String> = "observations, world"
            .split(',')
            .map(str::to_string)
            .collect();
        let err = HindsightMemoryConfig::normalize_recall_types(&env_tokens)
            .expect_err("an invalid env token must be rejected");
        assert_eq!(err, "observations");
        // Valid env-style value normalizes to the same canonical vec a TOML
        // value would.
        let ok_tokens: Vec<String> = " world , experience "
            .split(',')
            .map(str::to_string)
            .collect();
        let normalized = HindsightMemoryConfig::normalize_recall_types(&ok_tokens)
            .expect("valid env value must normalize");
        assert_eq!(
            normalized,
            vec!["world".to_string(), "experience".to_string()]
        );
    }

    /// Blocker fix (single observable source of truth): the effective
    /// `recall_types` must come ONLY from the typed config field. The
    /// `from_config` constructor must NOT re-read any environment variable -
    /// even the legacy `ZC_HINDSIGHT_RECALL_TYPES` name, which is now bridged
    /// into the typed field during config loading. Here the typed field is
    /// empty (no filter) while the legacy env is set to `observation`; the
    /// constructor must honor the typed field and ignore the stray env, proving
    /// the backend no longer has a second, config-invisible source.
    #[tokio::test]
    async fn from_config_recall_types_ignores_legacy_env_uses_typed_field_only() {
        let _lock = env_test_lock();
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_RECALL_TYPED_ONLY";
        let _tok = EnvVarGuard::set(env_name, "tok");
        // A stray legacy env value must NOT reach the backend; only config
        // loading bridges it, and that is exercised in the config crate.
        let _stray = EnvVarGuard::set("ZC_HINDSIGHT_RECALL_TYPES", "observation");
        let cfg = HindsightMemoryConfig {
            base_url: "https://memory.example.com/hs".to_string(),
            token_env: env_name.to_string(),
            // Typed config says "no filter"; the stray env would (wrongly) add one.
            recall_types: Vec::new(),
            ..HindsightMemoryConfig::default()
        };
        let mem = HindsightMemory::from_config(&cfg, "scout", "").expect("construct");
        assert!(
            mem.recall_types.is_empty(),
            "from_config must use the typed field only and ignore the legacy \
             ZC_HINDSIGHT_RECALL_TYPES env (bridged during config load instead)"
        );
    }

    #[tokio::test]
    async fn empty_query_recall_honors_recall_types_on_list() {
        // The recent-recall (empty query) path falls back to `list`, which has
        // no server-side type filter. With recall_types configured, mixed fact
        // types returned by list must be filtered locally so an
        // observations-only agent does not receive experience/world rows. A row
        // with no server type is kept (legacy/unlabeled history).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "obs", "text": "kept-observation", "type": "observation", "context": "c" },
                    { "id": "exp", "text": "dropped-experience", "type": "experience", "context": "c" },
                    { "id": "wor", "text": "dropped-world", "type": "world", "context": "c" },
                    { "id": "leg", "text": "kept-legacy-untyped", "context": "c" }
                ],
                "total": 4
            })))
            .mount(&server)
            .await;

        let mut mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.recall_types = vec!["observation".to_string()];
        // Empty query -> list fallback; only observation + untyped survive.
        let hits = mem.recall("", 10, None, None, None).await.expect("list");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert!(
            ids.contains(&"obs"),
            "observation row must be kept: {ids:?}"
        );
        assert!(
            ids.contains(&"leg"),
            "untyped legacy row must be kept: {ids:?}"
        );
        assert!(
            !ids.contains(&"exp") && !ids.contains(&"wor"),
            "experience/world rows must be dropped by the type filter: {ids:?}"
        );
    }

    #[tokio::test]
    async fn star_query_recall_honors_recall_types_with_mixed_types() {
        // Same as above via the bare `*` recent-recall alias, proving the
        // normalized empty/`*` branch applies the filter (regression for the
        // `*` case specifically).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "w", "text": "world-fact", "type": "world", "context": "c" },
                    { "id": "e", "text": "exp-fact", "type": "experience", "context": "c" }
                ],
                "total": 2
            })))
            .mount(&server)
            .await;

        let mut mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.recall_types = vec!["world".to_string(), "experience".to_string()];
        let hits = mem.recall("*", 10, None, None, None).await.expect("list");
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert!(
            ids.contains(&"w") && ids.contains(&"e"),
            "both configured types must survive: {ids:?}"
        );
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

        // The whole point of decoding tags: the Daily gate can now find the
        // Daily candidate instead of every row reading back as Core.
        let daily = crate::dedup::daily_candidates(hits);
        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].id, "d1");
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

    /// A HindsightMemory configured with a shared bank, so tests can prove
    /// `list_own_daily_history` never merges the shared tier the way ordinary
    /// `list`/`recall` do.
    fn memory_for_with_shared(base_url: &str, bank: &str, shared_bank: &str) -> HindsightMemory {
        HindsightMemory {
            alias: "tester".to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            bank: bank.to_string(),
            shared_bank: Some(shared_bank.to_string()),
            system_bank: None,
            token: "test-token".to_string(),
            default_top_k: DEFAULT_HINDSIGHT_TOP_K,
            recall_types: Vec::new(),
            client: build_client(DEFAULT_HINDSIGHT_TIMEOUT_SECS),
        }
    }

    #[tokio::test]
    async fn list_own_daily_history_never_reads_the_shared_bank() {
        // Regression for the S4 review blocker: a shared/system Daily row must
        // never suppress a private Daily write. `list_own_daily_history` must
        // read ONLY the private bank, unlike `list`/`recall` which merge the
        // shared/system tiers. No mock is mounted for the shared bank's list
        // endpoint at all, so if the implementation regresses to merging
        // tiers, this test fails on an unmatched-request panic rather than
        // silently passing.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-private/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "priv-daily", "text": "private daily row", "tags": ["daily"] }
                ],
                "total": 1
            })))
            .mount(&server)
            .await;
        // Intentionally NOT mounting a mock for the shared bank's list
        // endpoint: if `list_own_daily_history` ever queries it, wiremock
        // returns a 404 and the call fails loudly instead of silently
        // merging shared rows in.

        let mem = memory_for_with_shared(&server.uri(), "zeroclaw-private", "zeroclaw-shared");
        let rows = mem
            .list_own_daily_history()
            .await
            .expect("private-only lookup must succeed without touching the shared bank");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "priv-daily");
    }

    #[tokio::test]
    async fn list_own_daily_history_filters_out_non_daily_private_rows() {
        // Regression for the S4 review blocker: unrelated categories in the
        // private bank must not crowd out (or masquerade as) the real Daily
        // candidate set used by the dedup gate.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "core1", "text": "unrelated core fact", "tags": ["core"] },
                    { "id": "daily1", "text": "daily summary one", "tags": ["daily"] },
                    { "id": "conv1", "text": "conversation turn", "tags": ["conversation"] },
                    { "id": "daily2", "text": "daily summary two", "tags": ["daily"] }
                ],
                "total": 4
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let rows = mem
            .list_own_daily_history()
            .await
            .expect("list should succeed");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["daily1", "daily2"], "only Daily rows: {ids:?}");
    }

    #[tokio::test]
    async fn list_own_daily_history_requests_only_valid_state_so_invalidated_rows_cannot_suppress()
    {
        // B2 regression (a): the Hindsight list route INCLUDES invalidated rows
        // by default, so an explicitly invalidated Daily match could suppress
        // its own replacement at write time. The dedicated write-time query
        // must send `state=valid`; the mock ONLY matches when that param is
        // present, and returns the single active row. If the query regressed to
        // the default (no state filter), wiremock would 404 and the call would
        // fail loudly instead of silently including invalidated rows.
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .and(query_param("state", "valid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "active-daily", "text": "the surviving replacement", "tags": ["daily"] }
                ],
                "total": 1
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let rows = mem
            .list_own_daily_history()
            .await
            .expect("write-time daily lookup must send state=valid and succeed");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["active-daily"],
            "only the active (valid) Daily row is a dedup candidate: {ids:?}"
        );
    }

    #[tokio::test]
    async fn list_own_daily_history_pages_beyond_the_first_hundred_candidates() {
        // B2 regression (b): the list route returns only the first page (100
        // rows) by default, so a valid duplicate beyond page 1 would be
        // invisible and the same summary appended again. The dedicated query
        // must page COMPLETELY. Page 1 (offset=0) returns 100 rows with
        // total=101; page 2 (offset=100) returns the 101st row. Both must be in
        // the returned candidate set.
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;

        let page1: Vec<_> = (0..100)
            .map(|i| json!({ "id": format!("d{i}"), "text": format!("daily {i}"), "tags": ["daily"] }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .and(query_param("offset", "0"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "items": page1, "total": 101 })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .and(query_param("offset", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "d100", "text": "daily 100 (page 2)", "tags": ["daily"] }
                ],
                "total": 101
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let rows = mem
            .list_own_daily_history()
            .await
            .expect("complete pagination must succeed");
        assert_eq!(
            rows.len(),
            101,
            "every active Daily candidate across all pages must be returned, got {}",
            rows.len()
        );
        assert!(
            rows.iter().any(|r| r.id == "d100"),
            "the 101st row on page 2 must be included so a beyond-page-1 duplicate is not missed"
        );
    }

    #[tokio::test]
    async fn list_own_daily_history_bypasses_recall_types_on_the_write_path() {
        // B2 regression (c) + the S3 interaction: S3 applies `recall_types` as a
        // per-row presentation predicate inside `list_bank`. Write-time dedup
        // must compare against the COMPLETE active Daily set regardless of
        // recall visibility, so the dedicated query must NOT inherit that
        // filter. Here the agent is configured observations-only, yet a Daily
        // row typed `experience` (which recall would hide) must still surface as
        // a dedup candidate. The mock has no `type`-based behavior; the point is
        // that the returned experience-typed Daily row is kept, proving
        // `fact_type_allowed` is never applied on this path.
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .and(query_param("state", "valid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "obs-daily", "text": "observation daily", "type": "observation", "tags": ["daily"] },
                    { "id": "exp-daily", "text": "experience daily", "type": "experience", "tags": ["daily"] }
                ],
                "total": 2
            })))
            .mount(&server)
            .await;

        let mut mem = memory_for(&server.uri(), "zeroclaw-test");
        // Recall presentation is restricted to observations only...
        mem.recall_types = vec!["observation".to_string()];

        // ...but the write-time dedup candidate set must include BOTH Daily rows.
        let rows = mem
            .list_own_daily_history()
            .await
            .expect("write-time daily lookup must succeed");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"obs-daily") && ids.contains(&"exp-daily"),
            "write-time dedup must see the complete active Daily set, ignoring recall_types: {ids:?}"
        );

        // And prove the recall PATH is unaffected: the same mixed-type rows via
        // the recall/list presentation path still drop the experience row.
        // Served from a SECOND server because `list_bank` sends a plain GET
        // (no `state=valid`), which the write-time mock above intentionally
        // does not match.
        let recall_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "obs-daily", "text": "observation daily", "type": "observation", "tags": ["daily"] },
                    { "id": "exp-daily", "text": "experience daily", "type": "experience", "tags": ["daily"] }
                ],
                "total": 2
            })))
            .mount(&recall_server)
            .await;
        let mut recall_mem = memory_for(&recall_server.uri(), "zeroclaw-test");
        recall_mem.recall_types = vec!["observation".to_string()];
        let presented = recall_mem
            .list_bank(&recall_mem.bank)
            .await
            .expect("recall-presentation list must succeed");
        let presented_ids: Vec<&str> = presented.iter().map(|r| r.id.as_str()).collect();
        assert!(
            presented_ids.contains(&"obs-daily") && !presented_ids.contains(&"exp-daily"),
            "S3's recall_types filter must still apply on the recall path: {presented_ids:?}"
        );
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
            recall_types: Vec::new(),
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
    async fn forget_resolves_caller_key_to_server_id_then_invalidates() {
        // The Memory contract: forget(key) removes by the SAME key store()
        // accepted. store() writes the caller key as `context`; the server
        // assigns its own opaque id ("srv-abc123", NOT "user_lang"). forget
        // must resolve "user_lang" -> "srv-abc123" via a list scan, then PATCH
        // invalidate on THAT id - never PATCH the raw caller key as if it were
        // the server id.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "srv-abc123", "text": "Rust", "context": "user_lang", "type": "world" }
                ]
            })))
            .mount(&server)
            .await;
        // The mock only matches a PATCH to the RESOLVED server id - a PATCH to
        // the literal caller key "user_lang" would 404 against this mock,
        // failing the test.
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/srv-abc123"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_partial_json(json!({ "state": "invalidated" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(
            mem.forget("user_lang")
                .await
                .expect("forget should succeed"),
            "resolving the caller key to the server id must invalidate the right resource"
        );
    }

    #[tokio::test]
    async fn store_recall_forget_round_trip_uses_the_same_caller_key() {
        // Full contract proof: store(key) -> recall() surfaces MemoryEntry::key
        // == the original caller key (not the server id) -> forget(that same
        // key) resolves and removes the right resource. This is the "real
        // store->recall->forget round trip" the key-contract fix must satisfy,
        // as opposed to a test that fabricates a server id directly.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "id": "srv-xyz", "text": "Prefers Rust", "context": "user_lang", "type": "world" }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "srv-xyz", "text": "Prefers Rust", "context": "user_lang", "type": "world" }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/srv-xyz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.store("user_lang", "Prefers Rust", MemoryCategory::Core, None)
            .await
            .expect("store should succeed");

        let recalled = mem
            .recall("Rust", 5, None, None, None)
            .await
            .expect("recall should succeed");
        assert_eq!(recalled.len(), 1);
        // The caller-facing key survives the round trip; it is NOT the opaque
        // server id.
        assert_eq!(recalled[0].key, "user_lang");
        assert_eq!(recalled[0].id, "srv-xyz");

        let removed = mem
            .forget(&recalled[0].key)
            .await
            .expect("forget with the recalled caller key should succeed");
        assert!(removed, "forget must resolve the caller key and remove it");
    }

    #[tokio::test]
    async fn forget_no_matching_key_is_a_noop() {
        // A key with no matching `context` in the bank must not fire any PATCH
        // and must report nothing removed, same as every other backend's
        // "not found" contract.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "srv-1", "text": "unrelated", "context": "other_key", "type": "world" }
                ]
            })))
            .mount(&server)
            .await;
        // No PATCH mock mounted: a request to any PATCH path fails the test.

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(
            !mem.forget("missing_key")
                .await
                .expect("no match must not error")
        );
    }

    #[tokio::test]
    async fn forget_maps_404_to_false() {
        // A resolved id that the server has already removed (404 on the
        // invalidate PATCH) maps to Ok(false), so hygiene degrades gracefully.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "srv-gone", "text": "t", "context": "stale_key", "type": "world" }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/srv-gone"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(!mem.forget("stale_key").await.expect("404 must not error"));
    }

    #[tokio::test]
    async fn forget_surfaces_server_error() {
        // A 5xx from the invalidate PATCH is a real failure and must surface
        // as an error (not a silent false), so the caller can retry/log.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "srv-boom", "text": "t", "context": "boom_key", "type": "world" }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/srv-boom"))
            .respond_with(ResponseTemplate::new(500).set_body_string("kaboom"))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let err = mem.forget("boom_key").await.unwrap_err();
        assert!(
            err.to_string().contains("500"),
            "5xx must surface the status: {err}"
        );
    }

    #[tokio::test]
    async fn forget_empty_key_is_a_noop() {
        // No mock mounted: an empty/whitespace key must not fire any request.
        let mem = memory_for("http://127.0.0.1:1", "zeroclaw-test");
        assert!(!mem.forget("   ").await.expect("empty key short-circuits"));
    }

    #[tokio::test]
    async fn forget_refuses_dot_id_segments() {
        // Even if a resolved (or directly-supplied) id were exactly "." or
        // "..", the invalidate path must refuse it rather than let unencoded
        // dot-segment normalization re-route the authenticated PATCH away from
        // the intended memory item.
        let mem = memory_for("http://127.0.0.1:1", "zeroclaw-test");
        let err = mem
            .invalidate_in_bank("zeroclaw-test", ".")
            .await
            .expect_err("a single-dot id must be refused");
        assert!(err.to_string().contains('.'));

        let err = mem
            .invalidate_in_bank("zeroclaw-test", "..")
            .await
            .expect_err("a double-dot id must be refused");
        assert!(err.to_string().contains(".."));
    }

    #[tokio::test]
    async fn forget_refuses_observation_fact_type() {
        // Hindsight v0.8.4 only allows curating (invalidating) world/experience
        // facts, not derived observations. A key that resolves to an
        // observation must return a clear typed error rather than silently
        // no-op-ing or firing a PATCH the server would reject anyway.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "srv-obs", "text": "derived", "context": "obs_key", "type": "observation" }
                ]
            })))
            .mount(&server)
            .await;
        // No PATCH mock mounted: an observation must never reach the PATCH
        // call at all.

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let err = mem
            .forget("obs_key")
            .await
            .expect_err("forget on an observation-backed key must error");
        assert!(
            err.to_string().to_lowercase().contains("observation"),
            "error must name the observation constraint: {err}"
        );
    }

    #[tokio::test]
    async fn forget_for_agent_resolves_caller_key_in_private_bank() {
        // forget_for_agent ignores agent_id (the bank is the per-agent scope)
        // and resolves+invalidates by key in the private bank, same as forget.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "srv-mem-9", "text": "t", "context": "agent_key", "type": "world" }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/srv-mem-9"))
            .and(body_partial_json(json!({ "state": "invalidated" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(
            mem.forget_for_agent("agent_key", "any-agent")
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
    async fn count_reports_total_from_list_endpoint() {
        // The dashboard memory-count path calls `count()`; a hindsight bank with
        // many entries must map through as a non-zero total (the bug it fixes:
        // the UI showed 0 while the bank was full).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "1", "text": "a" },
                    { "id": "2", "text": "b" }
                ],
                "total": 12
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let n = mem.count().await.expect("count should succeed");
        assert_eq!(n, 12, "count must reflect the bank total, not 0");
    }

    #[tokio::test]
    async fn count_falls_back_to_item_len_without_total() {
        // When the server omits `total`, the item count is the fallback - still
        // non-zero for a populated bank.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "1", "text": "a" },
                    { "id": "2", "text": "b" },
                    { "id": "3", "text": "c" }
                ]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert_eq!(mem.count().await.expect("count"), 3);
    }

    #[tokio::test]
    async fn list_returns_bank_items() {
        // The dashboard/gateway list path maps hindsight list items to entries.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "m1", "text": "first", "context": "c1" },
                    { "id": "m2", "text": "second", "context": "c2" }
                ],
                "total": 2
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        let items = mem.list(None, None).await.expect("list should succeed");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].content, "first");
        assert_eq!(items[1].content, "second");
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

    /// Recent recall must merge every
    /// tier and sort by recency BEFORE truncating to `limit`. Here the private
    /// bank alone returns `limit` OLDER rows and the shared bank returns a
    /// single NEWER row. The buggy behavior (append private-first, then
    /// truncate) fills the whole budget with the older private rows and drops
    /// the newer shared row; the fix sorts newest-first across tiers first, so
    /// the newer shared row must survive the cut.
    #[tokio::test]
    async fn recent_recall_merges_tiers_by_recency_before_truncation() {
        let server = MockServer::start().await;
        let limit = 3;
        // Private bank: `limit` rows, all OLDER than the shared row.
        let private_items: Vec<_> = (0..limit)
            .map(|i| {
                json!({
                    "id": format!("priv-{i}"),
                    "text": format!("old-private-{i}"),
                    "tags": ["zeroclaw", "core"],
                    "mentioned_at": format!("2026-01-0{}T00:00:00Z", i + 1)
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-and/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": private_items,
                "total": limit
            })))
            .mount(&server)
            .await;
        // Shared bank: one row NEWER than every private row.
        Mock::given(method("GET"))
            .and(path("/v1/default/banks/zeroclaw-house/memories/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    { "id": "shared-new", "text": "fresh-shared-guidance",
                      "tags": ["zeroclaw", "core"],
                      "mentioned_at": "2026-08-01T00:00:00Z" }
                ],
                "total": 1
            })))
            .mount(&server)
            .await;

        let mem = memory_with_tiers(&server.uri(), Some("zeroclaw-house"), None);
        // Bare `*` normalizes to the recent/empty query -> list-fallback path.
        let hits = mem
            .recall("*", limit, None, None, None)
            .await
            .expect("recent recall should succeed");

        assert_eq!(hits.len(), limit, "must truncate to the limit");
        let texts: Vec<&str> = hits.iter().map(|h| h.content.as_str()).collect();
        assert!(
            texts.contains(&"fresh-shared-guidance"),
            "the NEWER shared row must survive truncation, not be starved by \
             older private rows: {texts:?}"
        );
        // It is the newest overall, so it must sort to the front.
        assert_eq!(
            hits[0].content, "fresh-shared-guidance",
            "newest-first ordering must place the fresh shared row at the top: {texts:?}"
        );
    }

    /// The recency sort must be a TOTAL, reproducible order even when timestamps
    /// tie or fail to parse: equal/empty timestamps fall back to id, then
    /// content, so the same input always yields the same order regardless of the
    /// banks' merge/append order.
    #[test]
    fn sort_recent_stable_is_total_and_reproducible() {
        let mk = |id: &str, ts: &str, content: &str| MemoryEntry {
            id: id.to_string(),
            key: id.to_string(),
            content: content.to_string(),
            category: MemoryCategory::Core,
            timestamp: ts.to_string(),
            session_id: None,
            score: None,
            namespace: "default".to_string(),
            importance: None,
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        };
        // Two rows share a timestamp; one has an unparseable timestamp.
        let base = vec![
            mk("b", "2026-05-01T00:00:00Z", "z"),
            mk("a", "2026-05-01T00:00:00Z", "y"),
            mk("c", "not-a-date", "x"),
            mk("d", "2026-09-01T00:00:00Z", "w"),
        ];
        let mut first = base.clone();
        HindsightMemory::sort_recent_stable(&mut first);
        // A different starting permutation must yield the SAME final order.
        let mut second = base;
        second.reverse();
        HindsightMemory::sort_recent_stable(&mut second);
        let ids = |v: &[MemoryEntry]| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&first), ids(&second), "order must be reproducible");
        // Newest first (d), then the tie broken by id (a before b), then the
        // unparseable timestamp last (c sorts oldest).
        assert_eq!(ids(&first), vec!["d", "a", "b", "c"]);
    }
}
