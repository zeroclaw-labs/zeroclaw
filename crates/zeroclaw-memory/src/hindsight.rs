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
//! Configuration comes from the typed `[memory.hindsight]` section via
//! [`HindsightMemory::from_config`]; the bearer token is resolved from the
//! environment (or an inline non-committed `token`) so no secret lands in a
//! committed config file. [`HindsightMemory::from_env`] is retained for entry
//! points with no typed config in scope (CLI migration probes) and reads:
//!
//! | Env var             | Meaning                                   | Default |
//! |---------------------|-------------------------------------------|---------|
//! | `ZC_HINDSIGHT_TOKEN`| Bearer token (raw, no "Bearer " prefix)   | (required) |
//! | `ZC_HINDSIGHT_BASE` | API base URL                              | `https://tokengate.appz.cloud/api/embedding/hindsight` |
//! | `ZC_HINDSIGHT_BANK` | Explicit bank id (overrides per-agent)    | `zeroclaw-<alias>` |
//! | `ZC_HINDSIGHT_TOP_K`| Default recall limit when caller passes 0 | `5` |
//! | `ZC_HINDSIGHT_SHARED_BANK`| Shared/family bank merged read-only into recall/list; written only via the `shared_memory_store` tool | (none) |
//! | `ZC_HINDSIGHT_SYSTEM_BANK`| System bank merged read-only into recall/list; written only via the `system_memory_store` tool | (none) |
//!
//! Shared and system banks: `ZC_HINDSIGHT_SHARED_BANK` and
//! `ZC_HINDSIGHT_SYSTEM_BANK` name two extra banks every agent can READ from
//! (merged into recall + list). Ordinary writes (`store`, including automatic
//! per-turn consolidation) always land in the per-agent private `bank`, so
//! personal memory stays isolated. The shared/system banks are written ONLY via
//! the explicit [`HindsightMemory::store_to_bank`] path behind the dedicated
//! `shared_memory_store` / `system_memory_store` tools, which are per-agent
//! gateable by name. This is the native mechanism for a tiered memory model:
//! private per agent + permitted shared/family writes + admin-only system
//! writes, all readable by everyone.
//!
//! Bank resolution precedence for shared/system: the typed `[memory.hindsight]`
//! `shared_bank` / `system_bank` fields win when non-empty; otherwise the
//! driver falls back to the env vars above (parity with the private-bank env
//! fallback). A shared/system bank equal to the private bank is ignored so a
//! misconfig never turns private writes into shared ones.
//!
//! Deletion (`forget` / `forget_for_agent`): mapped to the hindsight invalidate
//! endpoint (`PATCH .../memories/{id}` with `state=invalidated`), a soft-delete.
//! The `key` the trait passes is the memory id the read paths surface (`id` and
//! `key` are both the server id in `to_entry`). Deletion targets the PRIVATE
//! bank only - the same bank writes land in and the one retention/hygiene needs
//! to prune. The read-merged shared/system banks are never written or pruned
//! through this driver instance, so their rows are not removable here; that is
//! intentional (shared/system are append-only tiers managed out of band).

use super::traits::{Memory, MemoryCategory, MemoryEntry, SharedWritable};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use zeroclaw_config::schema::HindsightMemoryConfig;

const DEFAULT_BASE: &str = "https://tokengate.appz.cloud/api/embedding/hindsight";
const DEFAULT_TOP_K: usize = 5;
/// Env var naming the shared/family bank (read-merged; written via tool only).
const SHARED_BANK_ENV: &str = "ZC_HINDSIGHT_SHARED_BANK";
/// Env var naming the system bank (read-merged; written via tool only).
const SYSTEM_BANK_ENV: &str = "ZC_HINDSIGHT_SYSTEM_BANK";

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

impl HindsightMemory {
    /// Build a hindsight backend for `agent_alias` from the typed
    /// `[memory.hindsight]` config plus a per-agent `bank_id` override.
    ///
    /// The bearer token is resolved from the environment variable named by
    /// `cfg.token_env` first (the recommended path, keeping secrets out of the
    /// committed config), then from an inline `cfg.token` as an escape hatch
    /// for non-committed local configs. The optional shared read-only bank
    /// still comes from `ZC_HINDSIGHT_SHARED_BANK` so it can be toggled without
    /// editing config.
    ///
    /// This is the primary constructor used by `create_memory_for_agent`.
    pub fn from_config(
        cfg: &HindsightMemoryConfig,
        agent_alias: &str,
        bank_override: &str,
    ) -> Result<Self> {
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
        // dropped so private writes can never leak into a shared tier.
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
            DEFAULT_TOP_K
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
            client: reqwest::Client::new(),
        })
    }

    /// Build a hindsight backend for `agent_alias`, reading endpoint/token/bank
    /// from the environment. The bank defaults to `zeroclaw-<alias>` unless
    /// `ZC_HINDSIGHT_BANK` pins an explicit one. Retained for entry points with
    /// no typed config in scope (e.g. CLI migration probes); the daemon path
    /// uses [`HindsightMemory::from_config`].
    pub fn from_env(agent_alias: &str) -> Result<Self> {
        let token = std::env::var("ZC_HINDSIGHT_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .context(
                "memory backend 'hindsight' requires ZC_HINDSIGHT_TOKEN (the tokengate bearer token)",
            )?;
        let base_url = std::env::var("ZC_HINDSIGHT_BASE")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE.to_string());
        let bank = std::env::var("ZC_HINDSIGHT_BANK")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("zeroclaw-{agent_alias}"));
        // Optional shared/system read-only banks; ignored if they equal an
        // already-taken bank (private, or for system the shared bank).
        let shared_bank = resolve_secondary_bank(None, SHARED_BANK_ENV, &[bank.as_str()]);
        let system_bank = resolve_secondary_bank(
            None,
            SYSTEM_BANK_ENV,
            &[bank.as_str(), shared_bank.as_deref().unwrap_or_default()],
        );
        let default_top_k = std::env::var("ZC_HINDSIGHT_TOP_K")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|k| *k > 0)
            .unwrap_or(DEFAULT_TOP_K);

        Ok(Self {
            alias: agent_alias.to_string(),
            base_url,
            bank,
            shared_bank,
            system_bank,
            token,
            default_top_k,
            client: reqwest::Client::new(),
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
            default_top_k: DEFAULT_TOP_K,
            client: reqwest::Client::new(),
        }
    }

    fn memories_url_for(&self, bank: &str) -> String {
        format!("{}/v1/default/banks/{}/memories", self.base_url, bank)
    }

    fn memories_url(&self) -> String {
        self.memories_url_for(&self.bank)
    }

    fn recall_url_for(&self, bank: &str) -> String {
        format!(
            "{}/v1/default/banks/{}/memories/recall",
            self.base_url, bank
        )
    }

    fn list_url_for(&self, bank: &str) -> String {
        format!("{}/v1/default/banks/{}/memories/list", self.base_url, bank)
    }

    /// URL of a single memory item, used by the invalidate (soft-delete) PATCH.
    fn memory_item_url_for(&self, bank: &str, id: &str) -> String {
        format!(
            "{}/v1/default/banks/{}/memories/{}",
            self.base_url, bank, id
        )
    }

    /// Soft-delete (invalidate) a memory item in `bank` by id via
    /// `PATCH .../memories/{id}` with `{"state":"invalidated"}` - the same call
    /// the ops diagnosis used to prune remote rows. Returns `Ok(true)` when the
    /// server accepted the invalidation, `Ok(false)` for a `404` (already gone /
    /// unknown id) so retention/hygiene degrade gracefully. Other non-success
    /// statuses surface as an error. Client errors are mapped to `Ok`/`false`
    /// rather than bubbling so a bad id cannot trip the caller's breaker.
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
        // Treat "not found" as a graceful no-op: the row is already absent, so
        // retention has nothing to remove.
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("hindsight invalidate returned HTTP {status}: {text}");
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
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("hindsight recall returned HTTP {status}: {text}");
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
                Self::to_entry(r.id, r.text, r.context, r.mentioned_at, score)
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
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("hindsight shared/system retain returned HTTP {status}: {text}");
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
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("hindsight list returned HTTP {status}: {text}");
        }
        let parsed: ListResponse = resp
            .json()
            .await
            .context("hindsight list returned unparseable JSON")?;
        Ok(parsed
            .items
            .into_iter()
            .map(|i| Self::to_entry(i.id, i.text, i.context, i.mentioned_at, None))
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

    fn to_entry(
        id: Option<String>,
        text: Option<String>,
        context: Option<String>,
        mentioned_at: Option<String>,
        score: Option<f64>,
    ) -> MemoryEntry {
        MemoryEntry {
            id: id.clone().unwrap_or_default(),
            key: id.unwrap_or_default(),
            content: text.unwrap_or_default(),
            category: MemoryCategory::Core,
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
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("hindsight retain returned HTTP {status}: {text}");
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
        // Private bank (writes land here) is always recalled.
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
        // the invalidate PATCH. Writes only ever land in the private bank, so
        // that is the correct target for retention/hygiene prunes; the
        // read-merged shared/system banks are not written or pruned from here
        // (see the limitation note in the module docs). A `404` maps to
        // `Ok(false)` so hygiene degrades gracefully.
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
        // Bank-per-agent already isolates; ignore the allowlist and recall.
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
            default_top_k: 5,
            client: reqwest::Client::new(),
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
            token_env: env_name.to_string(),
            token: Some("inline-token-xyz".to_string()),
            ..HindsightMemoryConfig::default()
        };
        let mem = HindsightMemory::from_config(&cfg, "scout", "pinned-bank").expect("construct");
        assert_eq!(mem.token, "inline-token-xyz");
        assert_eq!(mem.bank(), "pinned-bank");
    }

    #[test]
    fn from_config_errors_without_any_token() {
        let env_name = "ZC_HINDSIGHT_TEST_TOKEN_MISSING";
        unsafe { std::env::remove_var(env_name) };
        let cfg = HindsightMemoryConfig {
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
        let removed = mem.forget("mem-123").await.expect("forget should succeed");
        assert!(removed, "a 2xx invalidate must report the row removed");
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
