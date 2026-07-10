//! Hindsight external memory backend.
//!
//! A first-class [`Memory`] implementation that routes the agent's normal
//! store/recall path to the native Hindsight HTTP API (server-side
//! vectorization + embedding search) instead of the local SQLite/BM25 store.
//!
//! Selection: `[memory] backend = "hindsight"`. The runtime factory
//! (`create_memory_for_agent`) short-circuits to this backend before the
//! per-agent SQL/Markdown dispatch, so hindsight becomes the agent's built-in
//! memory pipeline (both the automatic per-turn consolidation writes and the
//! `memory_store` / `memory_recall` tools).
//!
//! Per-agent isolation: the bank id is derived per agent (`zeroclaw-<alias>` by
//! default, or `ZC_HINDSIGHT_BANK` to pin one). Because each agent gets its own
//! server-namespaced bank, the bank itself is the private-per-agent scope - no
//! local agent_id column is needed.
//!
//! Configuration is read from the environment so no secret lands in a committed
//! config file, and so the driver stays independent of the large schema crate:
//!
//! | Env var             | Meaning                                   | Default |
//! |---------------------|-------------------------------------------|---------|
//! | `ZC_HINDSIGHT_TOKEN`| Bearer token (raw, no "Bearer " prefix)   | (required) |
//! | `ZC_HINDSIGHT_BASE` | API base URL                              | `https://tokengate.appz.cloud/api/embedding/hindsight` |
//! | `ZC_HINDSIGHT_BANK` | Explicit bank id (overrides per-agent)    | `zeroclaw-<alias>` |
//! | `ZC_HINDSIGHT_TOP_K`| Default recall limit when caller passes 0 | `5` |

use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

const DEFAULT_BASE: &str = "https://tokengate.appz.cloud/api/embedding/hindsight";
const DEFAULT_TOP_K: usize = 5;

/// Hindsight-backed memory store bound to a single bank.
pub struct HindsightMemory {
    alias: String,
    base_url: String,
    bank: String,
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
            .field("default_top_k", &self.default_top_k)
            .finish_non_exhaustive()
    }
}

impl HindsightMemory {
    /// Build a hindsight backend for `agent_alias`, reading endpoint/token/bank
    /// from the environment. The bank defaults to `zeroclaw-<alias>` unless
    /// `ZC_HINDSIGHT_BANK` pins an explicit one.
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
        let default_top_k = std::env::var("ZC_HINDSIGHT_TOP_K")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|k| *k > 0)
            .unwrap_or(DEFAULT_TOP_K);

        Ok(Self {
            alias: agent_alias.to_string(),
            base_url,
            bank,
            token,
            default_top_k,
            client: reqwest::Client::new(),
        })
    }

    /// The resolved bank id (server namespaces it further, e.g. `u6--<bank>`).
    #[must_use]
    pub fn bank(&self) -> &str {
        &self.bank
    }

    fn memories_url(&self) -> String {
        format!("{}/v1/default/banks/{}/memories", self.base_url, self.bank)
    }

    fn recall_url(&self) -> String {
        format!(
            "{}/v1/default/banks/{}/memories/recall",
            self.base_url, self.bank
        )
    }

    fn list_url(&self) -> String {
        format!(
            "{}/v1/default/banks/{}/memories/list",
            self.base_url, self.bank
        )
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
        let effective_limit = if limit == 0 { self.default_top_k } else { limit };
        let normalized = super::traits::normalize_recent_recall_query(query);
        // Hindsight recall needs a query; for recent/empty queries fall back to list.
        if normalized.trim().is_empty() {
            return self.list(None, None).await.map(|mut v| {
                v.truncate(effective_limit);
                v
            });
        }
        let body = RecallBody {
            query: normalized,
            limit: effective_limit,
        };
        let resp = self
            .client
            .post(self.recall_url())
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
        let entries = parsed
            .results
            .into_iter()
            .map(|r| {
                let score = r.scores.as_ref().and_then(final_score);
                Self::to_entry(r.id, r.text, r.context, r.mentioned_at, score)
            })
            .collect();
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
        let resp = self
            .client
            .get(self.list_url())
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
        let entries = parsed
            .items
            .into_iter()
            .map(|i| Self::to_entry(i.id, i.text, i.context, i.mentioned_at, None))
            .collect();
        Ok(entries)
    }

    async fn forget(&self, _key: &str) -> Result<bool> {
        // Deletion maps to hindsight invalidate (PATCH); not wired for the
        // verification scope. Report "nothing removed" rather than error so the
        // memory tools degrade gracefully.
        Ok(false)
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> Result<usize> {
        let resp = self
            .client
            .get(self.list_url())
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
        Ok(parsed
            .total
            .map_or(parsed.items.len(), |t| usize::try_from(t).unwrap_or(usize::MAX)))
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
