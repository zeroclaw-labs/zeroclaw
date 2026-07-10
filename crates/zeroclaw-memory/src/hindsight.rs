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
//! | `ZC_HINDSIGHT_SHARED_BANK`| Extra read-only bank merged into recall/list (never written) | (none) |
//!
//! Shared read bank: `ZC_HINDSIGHT_SHARED_BANK` names a second bank every
//! agent can READ from (recall + list) but never WRITE to. Writes always land
//! in the per-agent private `bank`, so personal memory stays isolated while a
//! common household/shared bank is visible to all agents. This is the native
//! mechanism for "private per agent + one shared bank both can read".

use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use zeroclaw_config::schema::HindsightMemoryConfig;

const DEFAULT_BASE: &str = "https://tokengate.appz.cloud/api/embedding/hindsight";
const DEFAULT_TOP_K: usize = 5;
/// Env var naming the extra shared read-only bank (merged into recall/list).
const SHARED_BANK_ENV: &str = "ZC_HINDSIGHT_SHARED_BANK";

/// Hindsight-backed memory store bound to a single bank.
pub struct HindsightMemory {
    alias: String,
    base_url: String,
    bank: String,
    /// Optional extra bank merged into recall/list as READ-ONLY. Writes never
    /// touch it, so it acts as a shared bank all agents can read.
    shared_bank: Option<String>,
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
            .field("default_top_k", &self.default_top_k)
            .finish_non_exhaustive()
    }
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
        let shared_bank = std::env::var(SHARED_BANK_ENV)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && *s != bank);
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
        // Optional shared read-only bank; ignored if it equals the private bank.
        let shared_bank = std::env::var("ZC_HINDSIGHT_SHARED_BANK")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && *s != bank);
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

    /// The optional shared read-only bank merged into recall/list.
    #[must_use]
    pub fn shared_bank(&self) -> Option<&str> {
        self.shared_bank.as_deref()
    }

    fn memories_url(&self) -> String {
        format!("{}/v1/default/banks/{}/memories", self.base_url, self.bank)
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
        // Shared read-only bank, if set, is merged in (read-only, never written).
        if let Some(shared) = self.shared_bank.as_deref() {
            let shared_entries = self
                .recall_bank(shared, normalized, effective_limit)
                .await?;
            entries.extend(shared_entries);
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
        if let Some(shared) = self.shared_bank.as_deref() {
            entries.extend(self.list_bank(shared).await?);
        }
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
}
