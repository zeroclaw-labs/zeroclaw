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
//! Retain durability: writes (`store`, `store_to_bank`) are SYNCHRONOUS by
//! default (`[memory.hindsight] retain_async = false`), so a successful
//! `Memory::store` return means the item is durably queryable before the call
//! returns - an immediate recall, next-turn injection, or another agent's
//! shared-bank read cannot miss it. Setting `retain_async = true` (or the
//! `ZC_HINDSIGHT_RETAIN_ASYNC` env override, read through this same
//! [`HindsightMemory::from_config`] constructor so it reaches every runtime
//! path) sends the server-side `async` flag instead, trading that
//! read-after-write guarantee for lower in-turn latency; the env value is
//! parsed strictly (`true`/`false`/`1`/`0`/`yes`/`no`, case-insensitive) and
//! any other value is a startup error rather than a silent fallback.
//!
//! Recall type filter: `recall_types` (typed `[memory.hindsight] recall_types`,
//! env fallback `ZC_HINDSIGHT_RECALL_TYPES`) restricts recall to selected
//! Hindsight fact types (`experience`, `observation`, `world`); it is sent as
//! the recall body's `types` array and applied on BOTH the query and the
//! recent/empty-query (`list`) paths. Empty = no filter (all types).
//!
//! Shared and system tiers (this slice): the typed `[memory.hindsight]`
//! `shared_bank` / `system_bank` fields (env fallback `ZC_HINDSIGHT_SHARED_BANK`
//! / `ZC_HINDSIGHT_SYSTEM_BANK`) name two extra banks every agent can READ from
//! (merged into recall + list). Ordinary writes (`store`, including automatic
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
use serde::Deserialize;
use zeroclaw_config::schema::{
    DEFAULT_HINDSIGHT_TIMEOUT_SECS, DEFAULT_HINDSIGHT_TOP_K, HindsightMemoryConfig,
};

/// Env var naming the shared/family bank (read-merged; written via tool only).
const SHARED_BANK_ENV: &str = "ZC_HINDSIGHT_SHARED_BANK";
/// Env var naming the system bank (read-merged; written via tool only).
const SYSTEM_BANK_ENV: &str = "ZC_HINDSIGHT_SYSTEM_BANK";
/// Env var overriding `[memory.hindsight] retain_async`. Read inside
/// [`HindsightMemory::from_config`] (the single canonical constructor) so the
/// override reaches every runtime path, not just an env-only entry point.
const RETAIN_ASYNC_ENV: &str = "ZC_HINDSIGHT_RETAIN_ASYNC";

/// Strictly parse a boolean-shaped env value. Unlike a falsey-only check, an
/// unrecognized value (e.g. a typo) is REJECTED as an error instead of
/// silently defaulting to `true` - a typo in a durability-affecting override
/// must fail loudly, not flip retain semantics unnoticed.
fn parse_strict_bool(raw: &str) -> std::result::Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        other => Err(other.to_string()),
    }
}

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

/// Maximum number of bytes of a remote error body echoed into an error message.
/// Remote bodies are attacker/operator-influenced and may contain secrets or
/// large payloads, so they are truncated before surfacing.
const MAX_REMOTE_ERROR_BODY: usize = 512;

/// Read a failed response's body and reduce it to a bounded, single-line
/// snippet safe to embed in an error message. Collapses whitespace/newlines and
/// truncates to [`MAX_REMOTE_ERROR_BODY`] so a large or multi-line remote body
/// cannot flood logs or smuggle control characters into the error surface.
async fn bounded_error_body(resp: reqwest::Response) -> String {
    let raw = resp.text().await.unwrap_or_default();
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() > MAX_REMOTE_ERROR_BODY {
        let mut end = MAX_REMOTE_ERROR_BODY;
        while !collapsed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}… (truncated)", &collapsed[..end])
    } else {
        collapsed
    }
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
    /// When true, retain (write) requests set the server-side `async` flag so
    /// vectorization runs off the caller's critical path, trading away
    /// read-after-write durability. Applies to `store` and `store_to_bank`.
    /// Read paths are unaffected. Default `false` (synchronous): a successful
    /// `store` return means the item is immediately queryable.
    retain_async: bool,
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
            .field("retain_async", &self.retain_async)
            .finish_non_exhaustive()
    }
}

/// Resolve a secondary (shared/system) bank from typed config with an env
/// fallback, dropping it when empty or when it collides with an already-taken
/// bank (private or, for system, the shared bank).
fn resolve_secondary_bank(
    configured: Option<&str>,
    env_var: &str,
    taken: &[&str],
) -> Option<String> {
    configured
        .map(str::to_string)
        .or_else(|| {
            std::env::var(env_var)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .filter(|b| !b.is_empty() && !taken.contains(&b.as_str()))
}

/// Env var restricting recall to specific Hindsight fact types (comma-separated).
const RECALL_TYPES_ENV: &str = "ZC_HINDSIGHT_RECALL_TYPES";

/// Split `ZC_HINDSIGHT_RECALL_TYPES` (comma-separated fact types) into raw
/// tokens. Returns `None` when the var is unset so callers fall back to typed
/// config; returns `Some(tokens)` when the var is set (even if blank, yielding an
/// explicit empty override that disables the filter). Trimming, blank-dropping,
/// and fact-type validation are all delegated to
/// [`HindsightMemoryConfig::normalize_recall_types`] so the env and TOML paths
/// share exactly one validator.
fn recall_types_from_env() -> Option<Vec<String>> {
    std::env::var(RECALL_TYPES_ENV)
        .ok()
        .map(|raw| raw.split(',').map(str::to_string).collect())
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
        // Shared/system banks: typed config wins, env is the fallback. A bank
        // colliding with the private bank (or, for system, the shared bank) is
        // dropped so private writes can never leak into a shared tier. The
        // authoritative cross-agent collision check runs at config load
        // (`validate_self` + install-wide `Config` validation); this per-instance
        // drop is defense-in-depth for the constructing instance.
        let shared_bank = resolve_secondary_bank(
            cfg.shared_bank_configured(),
            SHARED_BANK_ENV,
            &[bank.as_str()],
        );
        let system_bank = resolve_secondary_bank(
            cfg.system_bank_configured(),
            SYSTEM_BANK_ENV,
            &[bank.as_str(), shared_bank.as_deref().unwrap_or_default()],
        );
        let default_top_k = if cfg.top_k == 0 {
            DEFAULT_HINDSIGHT_TOP_K
        } else {
            cfg.top_k
        };

        // Recall type filter: an explicit env override (comma-separated) wins,
        // else the typed config value. BOTH sources are routed through the one
        // normalizing validator (`HindsightMemoryConfig::normalize_recall_types`)
        // so an invalid env value (e.g. a typo like `observations`) fails at
        // startup exactly like an invalid TOML value, instead of being silently
        // sent on every recall. The typed config was already validated by
        // `validate_self` above; the env value is validated here. Empty means
        // "no filter" (all types).
        let recall_types = match recall_types_from_env() {
            Some(raw) => HindsightMemoryConfig::normalize_recall_types(raw).map_err(|bad| {
                anyhow::Error::msg(format!(
                    "environment variable {RECALL_TYPES_ENV} contains an invalid Hindsight \
                     fact type {bad:?}; must be a comma-separated list of experience, \
                     observation, world"
                ))
            })?,
            None => cfg.recall_types.clone(),
        };

        // Retain durability: the typed default is synchronous (`false`), so a
        // successful `store`/`store_to_bank` is durably queryable before the
        // call returns. `ZC_HINDSIGHT_RETAIN_ASYNC` overrides it, but is read
        // HERE (the single canonical constructor every runtime path uses) so
        // the documented rollback actually reaches per-agent construction,
        // not just the removed env-only path. Parsed strictly: an
        // unrecognized value is a startup error rather than being silently
        // treated as `true`.
        let retain_async = match std::env::var(RETAIN_ASYNC_ENV) {
            Ok(raw) => parse_strict_bool(&raw).map_err(|bad| {
                anyhow::Error::msg(format!(
                    "environment variable {RETAIN_ASYNC_ENV} has an invalid value {bad:?}; \
                     must be one of true, false, 1, 0, yes, no (case-insensitive)"
                ))
            })?,
            Err(_) => cfg.retain_async,
        };

        Ok(Self {
            alias: agent_alias.to_string(),
            base_url,
            bank,
            shared_bank,
            system_bank,
            token,
            default_top_k,
            recall_types,
            retain_async,
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
            retain_async: false,
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
        let parsed: RecallResponse = resp
            .json()
            .await
            .context("hindsight recall returned unparseable JSON")?;
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
            is_async: self.retain_async,
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
        let parsed: ListResponse = resp
            .json()
            .await
            .context("hindsight list returned unparseable JSON")?;
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

impl HindsightMemory {
    fn tags_for(category: &MemoryCategory) -> Vec<String> {
        vec!["zeroclaw".to_string(), category.to_string()]
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
            session_id: None,
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
        _session_id: Option<&str>,
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
        let body = RetainBody {
            items: vec![RetainItem {
                content,
                context: Some(context_owned.as_str()),
                tags: Self::tags_for(&category),
            }],
            is_async: self.retain_async,
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
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let effective_limit = if limit == 0 {
            self.default_top_k
        } else {
            limit
        };
        let normalized = super::traits::normalize_recent_recall_query(query);
        // Hindsight recall needs a query; for recent/empty queries fall back to list.
        if normalized.trim().is_empty() {
            return self.list(None, None).await.map(|mut v| {
                v.truncate(effective_limit);
                v
            });
        }
        // Recall this agent's own private bank first.
        let mut entries = self
            .recall_bank(&self.bank, normalized, effective_limit)
            .await?;
        // Shared/system read-only banks, if set, are merged in (never written
        // through this path). Both tiers are readable by every agent.
        let mut merged_any = false;
        for extra in [self.shared_bank.as_deref(), self.system_bank.as_deref()]
            .into_iter()
            .flatten()
        {
            entries.extend(self.recall_bank(extra, normalized, effective_limit).await?);
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
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // List this agent's own private bank first, then merge the shared/system
        // read-only tiers (if set).
        let mut entries = self.list_bank(&self.bank).await?;
        for extra in [self.shared_bank.as_deref(), self.system_bank.as_deref()]
            .into_iter()
            .flatten()
        {
            entries.extend(self.list_bank(extra).await?);
        }
        Ok(entries)
    }

    async fn list_own_daily_history(&self) -> Result<Vec<MemoryEntry>> {
        // Private-bank + Daily-scoped candidate lookup for the per-turn Daily
        // dedup gate. Unlike `list` (which merges the shared/system read tiers),
        // this reads ONLY the agent's PRIVATE bank and keeps only Daily rows, so
        // a shared/system Daily row can never suppress a private Daily write and
        // an unrelated category can never crowd the private duplicate out. The
        // list endpoint has no server-side category filter, so the Daily filter
        // is applied locally on the decoded category.
        let entries = self.list_bank(&self.bank).await?;
        Ok(entries
            .into_iter()
            .filter(|e| e.category == MemoryCategory::Daily)
            .collect())
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
        let parsed: ListResponse = resp.json().await.unwrap_or(ListResponse {
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
    use wiremock::matchers::{body_partial_json, header, method, path};
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
            recall_types: Vec::new(),
            // Matches the production default: synchronous retain.
            retain_async: false,
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
        // Default is SYNCHRONOUS retain: the body carries "async": false so a
        // successful response means the item is durably queryable.
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
    async fn store_uses_sync_retain_by_default() {
        // Assert the async flag explicitly: with the default retain_async, the
        // body must carry "async": false.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .and(body_partial_json(json!({ "async": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(
            !mem.retain_async,
            "retain_async must default to false (sync)"
        );
        mem.store("k", "v", MemoryCategory::Core, None)
            .await
            .expect("sync retain should succeed");
    }

    #[tokio::test]
    async fn store_uses_async_retain_when_configured_on() {
        // With retain_async explicitly on, the body must carry "async": true.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .and(body_partial_json(json!({ "async": true })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mut mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.retain_async = true;
        mem.store("k", "v", MemoryCategory::Core, None)
            .await
            .expect("async retain should succeed");
    }

    #[tokio::test]
    async fn store_to_bank_uses_sync_retain_by_default() {
        // The shared/system write path honors retain_async too.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-house/memories"))
            .and(body_partial_json(json!({ "async": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mem = memory_with_tiers(&server.uri(), Some("zeroclaw-house"), None);
        assert!(!mem.retain_async);
        mem.store_to_bank("zeroclaw-house", "k", "v", MemoryCategory::Core, "shared")
            .await
            .expect("sync store_to_bank should succeed");
    }

    /// Blocker fix (stateful-success): a caller relying on the default
    /// (synchronous) retain must be able to immediately recall what it just
    /// stored, proving `Memory::store` success means "durably queryable" by
    /// default rather than "queued". This exercises store -> recall against
    /// the SAME mock server instance rather than asserting only that the
    /// request body carried a particular flag.
    #[tokio::test]
    async fn sync_store_is_immediately_visible_to_recall() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .and(body_partial_json(json!({ "async": false })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories/recall"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "id": "mem-1",
                    "text": "GOLDEN-EMU-77",
                    "type": "world",
                    "context": "fact",
                    "mentioned_at": "2026-07-10T00:00:00Z",
                    "scores": { "final": 1.0 }
                }]
            })))
            .mount(&server)
            .await;

        let mem = memory_for(&server.uri(), "zeroclaw-test");
        assert!(!mem.retain_async, "default must be synchronous");
        mem.store("fact", "GOLDEN-EMU-77", MemoryCategory::Core, None)
            .await
            .expect("sync store should succeed");
        let hits = mem
            .recall("GOLDEN-EMU-77", 5, None, None, None)
            .await
            .expect("recall should succeed immediately after a sync store");
        assert!(
            hits.iter().any(|e| e.content.contains("GOLDEN-EMU-77")),
            "a synchronously-stored item must be visible to an immediate recall"
        );
    }

    /// Blocker fix (async does not prove durability / delayed-read + failure
    /// coverage): asserting the request body says `"async": true` does NOT by
    /// itself prove the item was durably stored. This proves the corollary an
    /// opted-in async caller must accept: a `store()` call that returns `Ok`
    /// can still correspond to server-side work that later fails, and the
    /// driver has no completion/barrier signal for that queued outcome - the
    /// caller opted OUT of the read-after-write guarantee, it is not
    /// additionally guaranteed eventual success.
    #[tokio::test]
    async fn async_retain_ok_does_not_prove_eventual_success() {
        let server = MockServer::start().await;
        // The server acknowledges the queue submission with 200 even though
        // the item's vectorization could later fail or be cancelled server
        // side; the driver has no operation-id tracking to distinguish that
        // from a fully durable write.
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-test/memories"))
            .and(body_partial_json(json!({ "async": true })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let mut mem = memory_for(&server.uri(), "zeroclaw-test");
        mem.retain_async = true;
        let result = mem
            .store("k", "queued-item", MemoryCategory::Core, None)
            .await;
        assert!(
            result.is_ok(),
            "async submission is acknowledged as Ok even though it only proves \
             queuing, not the eventual embed/durability outcome"
        );
    }

    #[tokio::test]
    async fn from_config_defaults_retain_async_false() {
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_RETAIN";
        // SAFETY: single-threaded test; set + remove within this test only.
        unsafe { std::env::set_var(env_name, "tok") };
        unsafe { std::env::remove_var(RETAIN_ASYNC_ENV) };
        let cfg = HindsightMemoryConfig {
            base_url: "https://memory.example.com/hs".to_string(),
            token_env: env_name.to_string(),
            ..HindsightMemoryConfig::default()
        };
        let mem = HindsightMemory::from_config(&cfg, "scout", "").expect("construct");
        assert!(
            !mem.retain_async,
            "retain_async must default to false (sync)"
        );

        let cfg_on = HindsightMemoryConfig {
            base_url: "https://memory.example.com/hs".to_string(),
            token_env: env_name.to_string(),
            retain_async: true,
            ..HindsightMemoryConfig::default()
        };
        let mem_on = HindsightMemory::from_config(&cfg_on, "scout", "").expect("construct");
        assert!(mem_on.retain_async, "retain_async on must propagate");
        unsafe { std::env::remove_var(env_name) };
    }

    /// Blocker fix (env override reaches the real runtime path): the
    /// documented `ZC_HINDSIGHT_RETAIN_ASYNC` rollback must reach
    /// `from_config` (the canonical constructor `create_memory_for_agent`
    /// uses for every per-agent runtime), not merely a removed env-only
    /// constructor. This is the "production-construction regression" the
    /// review asked for: it drives the exact `from_config` call the runtime
    /// factory makes and proves the env value wins over a conflicting typed
    /// default.
    #[tokio::test]
    async fn retain_async_env_override_reaches_from_config_runtime_path() {
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_ENV_OVERRIDE";
        unsafe { std::env::set_var(env_name, "tok") };
        // Typed config says sync (false); the env override flips it to async.
        unsafe { std::env::set_var(RETAIN_ASYNC_ENV, "true") };
        let cfg = HindsightMemoryConfig {
            base_url: "https://memory.example.com/hs".to_string(),
            token_env: env_name.to_string(),
            retain_async: false,
            ..HindsightMemoryConfig::default()
        };
        let mem = HindsightMemory::from_config(&cfg, "scout", "").expect("construct");
        assert!(
            mem.retain_async,
            "ZC_HINDSIGHT_RETAIN_ASYNC=true must override a typed-config false \
             through the canonical from_config runtime path"
        );
        unsafe { std::env::remove_var(RETAIN_ASYNC_ENV) };
        unsafe { std::env::remove_var(env_name) };
    }

    /// Blocker fix (strict boolean parser rejects typos): an unrecognized env
    /// value must be a construction ERROR, not silently coerced to `true`
    /// (which would otherwise be a way for a typo to unknowingly disable the
    /// durability guarantee).
    #[tokio::test]
    async fn retain_async_env_override_rejects_invalid_value() {
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_ENV_TYPO";
        unsafe { std::env::set_var(env_name, "tok") };
        unsafe { std::env::set_var(RETAIN_ASYNC_ENV, "asyncc") };
        let cfg = HindsightMemoryConfig {
            base_url: "https://memory.example.com/hs".to_string(),
            token_env: env_name.to_string(),
            ..HindsightMemoryConfig::default()
        };
        let err = HindsightMemory::from_config(&cfg, "scout", "").unwrap_err();
        assert!(
            err.to_string().contains(RETAIN_ASYNC_ENV) && err.to_string().contains("asyncc"),
            "an invalid env value must fail construction naming the bad value: {err}"
        );
        unsafe { std::env::remove_var(RETAIN_ASYNC_ENV) };
        unsafe { std::env::remove_var(env_name) };
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
        // The driver's `from_config` routes the `ZC_HINDSIGHT_RECALL_TYPES` env
        // override through `HindsightMemoryConfig::normalize_recall_types` - the
        // SAME validator `validate_self` applies to the TOML value. Rather than
        // mutate the process-global env var (which would race parallel
        // `from_config` tests), assert the shared validator directly: an invalid
        // env-style token is rejected exactly like an invalid TOML token, and
        // whitespace normalizes identically for both sources.
        use zeroclaw_config::schema::HindsightMemoryConfig;
        // env-style comma split (what recall_types_from_env yields) with a typo.
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
            retain_async: false,
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
            retain_async: true,
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
}
