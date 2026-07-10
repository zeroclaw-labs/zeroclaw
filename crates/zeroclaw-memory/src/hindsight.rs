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

/// Percent-encode a single URL path segment (bank id or server-provided memory
/// id). Encodes everything that is not an unreserved URL character so a bank
/// name or id containing `/`, `?`, `#`, spaces, or other reserved bytes cannot
/// break out of its path segment or inject query/fragment components. Mirrors
/// the repo convention of routing configurable/remote strings through
/// `urlencoding` before interpolating them into a request URL.
fn encode_segment(segment: &str) -> String {
    urlencoding::encode(segment).into_owned()
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

/// Parse `ZC_HINDSIGHT_RECALL_TYPES` (comma-separated fact types) into a trimmed,
/// non-empty-token list. Returns `None` when the var is unset so callers can
/// fall back to typed config; returns `Some(vec![])` when the var is set but
/// blank so an explicit empty override disables the filter.
fn recall_types_from_env() -> Option<Vec<String>> {
    std::env::var(RECALL_TYPES_ENV).ok().map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
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
        // else the typed config value. Empty means "no filter" (all types).
        let recall_types = recall_types_from_env().unwrap_or_else(|| cfg.recall_types.clone());

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
        let parsed: ListResponse = resp
            .json()
            .await
            .context("hindsight list returned unparseable JSON")?;
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
        MemoryEntry {
            id: id.clone().unwrap_or_default(),
            key: id.unwrap_or_default(),
            content: text.unwrap_or_default(),
            category: Self::category_from_tags(tags),
            timestamp: mentioned_at.unwrap_or_default(),
            session_id: None,
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
