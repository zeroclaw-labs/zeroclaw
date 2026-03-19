//! Mem0 (OpenMemory) memory backend.
//!
//! Connects to a self-hosted OpenMemory server via its REST API
//! and implements the [`Memory`] trait for seamless integration with
//! ZeroClaw's auto-save, auto-recall, and hygiene lifecycle.
//!
//! Deploy OpenMemory: `docker compose up` from the mem0 repo.
//! Default endpoint: `http://localhost:8765`.

use super::traits::{Memory, MemoryCategory, MemoryEntry, ProceduralMessage};
use crate::config::schema::Mem0Config;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Memory backend backed by a mem0 (OpenMemory) REST API.
pub struct Mem0Memory {
    client: Client,
    base_url: String,
    user_id: String,
    app_name: String,
    infer: bool,
}

// ── mem0 API request/response types ────────────────────────────────

#[derive(Serialize)]
struct AddMemoryRequest<'a> {
    user_id: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Mem0Metadata<'a>>,
    infer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    app: Option<&'a str>,
}

#[derive(Serialize)]
struct Mem0Metadata<'a> {
    key: &'a str,
    category: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

#[derive(Serialize)]
struct AddProceduralRequest<'a> {
    user_id: &'a str,
    messages: &'a [ProceduralMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct DeleteMemoriesRequest<'a> {
    memory_ids: Vec<&'a str>,
    user_id: &'a str,
}

#[derive(Deserialize)]
struct Mem0MemoryItem {
    id: String,
    #[serde(alias = "content", alias = "text", default)]
    memory: String,
    #[serde(default)]
    created_at: Option<serde_json::Value>,
    #[serde(default, rename = "metadata_")]
    metadata: Option<Mem0ResponseMetadata>,
    #[serde(alias = "relevance_score", default)]
    score: Option<f64>,
}

#[derive(Deserialize, Default)]
struct Mem0ResponseMetadata {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct Mem0ListResponse {
    #[serde(default)]
    items: Vec<Mem0MemoryItem>,
    #[serde(default)]
    total: usize,
}

// ── Implementation ─────────────────────────────────────────────────

impl Mem0Memory {
    /// Create a new mem0 memory backend from config.
    pub fn new(config: &Mem0Config) -> anyhow::Result<Self> {
        let base_url = config.url.trim_end_matches('/').to_string();
        if base_url.is_empty() {
            anyhow::bail!("mem0 URL is empty; set [memory.mem0] url or MEM0_URL env var");
        }

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(Self {
            client,
            base_url,
            user_id: config.user_id.clone(),
            app_name: config.app_name.clone(),
            infer: config.infer,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.base_url, path)
    }

    /// Use `session_id` as the effective mem0 `user_id` when provided,
    /// falling back to the configured default.  This enables per-user
    /// and per-group memory scoping via the existing `Memory` trait.
    fn effective_user_id<'a>(&'a self, session_id: Option<&'a str>) -> &'a str {
        session_id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.user_id)
    }

    /// Recall memories with optional search filters.
    ///
    /// - `created_after` / `created_before`: ISO 8601 timestamps for time-range filtering.
    /// - `metadata_filter`: arbitrary JSON object passed to the mem0 SDK `filters` param.
    pub async fn recall_filtered(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        created_after: Option<&str>,
        created_before: Option<&str>,
        metadata_filter: Option<&serde_json::Value>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let effective_user = self.effective_user_id(session_id);
        let limit_str = limit.to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("user_id", effective_user),
            ("search_query", query),
            ("size", &limit_str),
        ];
        if let Some(after) = created_after {
            params.push(("created_after", after));
        }
        if let Some(before) = created_before {
            params.push(("created_before", before));
        }
        let meta_json;
        if let Some(mf) = metadata_filter {
            meta_json = serde_json::to_string(mf)?;
            params.push(("metadata_filter", &meta_json));
        }

        let resp = self
            .client
            .get(self.api_url("/memories/"))
            .query(&params)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("mem0 recall failed ({status}): {text}");
        }

        let list: Mem0ListResponse = resp.json().await?;
        Ok(list.items.into_iter().map(|i| self.to_entry(i)).collect())
    }

    fn to_entry(&self, item: Mem0MemoryItem) -> MemoryEntry {
        let meta = item.metadata.unwrap_or_default();
        let timestamp = match item.created_at {
            Some(serde_json::Value::Number(n)) => {
                // Unix timestamp → ISO 8601
                if let Some(ts) = n.as_i64() {
                    chrono::DateTime::from_timestamp(ts, 0)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            Some(serde_json::Value::String(s)) => s,
            _ => String::new(),
        };

        let category = match meta.category.as_deref() {
            Some("daily") => MemoryCategory::Daily,
            Some("conversation") => MemoryCategory::Conversation,
            Some(other) if other != "core" => MemoryCategory::Custom(other.to_string()),
            // "core" or None → default
            _ => MemoryCategory::Core,
        };

        MemoryEntry {
            id: item.id,
            key: meta.key.unwrap_or_default(),
            content: item.memory,
            category,
            timestamp,
            session_id: meta.session_id,
            score: item.score,
        }
    }

    /// Store a conversation trace as procedural memory.
    ///
    /// Sends the message history (user input, tool calls, assistant response)
    /// to the mem0 procedural endpoint so that "how to" patterns can be
    /// extracted and stored for future recall.
    pub async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        let effective_user = self.effective_user_id(session_id);
        let body = AddProceduralRequest {
            user_id: effective_user,
            messages,
            metadata: None,
        };

        let resp = self
            .client
            .post(self.api_url("/memories/procedural"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("mem0 store_procedural failed ({status}): {text}");
        }

        Ok(())
    }
}

// ── History API types ─────────────────────────────────────────────

#[derive(Deserialize)]
struct Mem0HistoryResponse {
    #[serde(default)]
    history: Vec<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

impl Mem0Memory {
    /// Retrieve the edit history (audit trail) for a specific memory by ID.
    pub async fn history(&self, memory_id: &str) -> anyhow::Result<String> {
        let url = self.api_url(&format!("/memories/{memory_id}/history"));
        let resp = self.client.get(&url).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("mem0 history failed ({status}): {text}");
        }

        let body: Mem0HistoryResponse = resp.json().await?;

        if let Some(err) = body.error {
            anyhow::bail!("mem0 history error: {err}");
        }

        if body.history.is_empty() {
            return Ok(format!("No history found for memory {memory_id}."));
        }

        let mut lines = Vec::with_capacity(body.history.len() + 1);
        lines.push(format!("History for memory {memory_id}:"));

        for (i, entry) in body.history.iter().enumerate() {
            let event = entry
                .get("event")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let old_memory = entry
                .get("old_memory")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let new_memory = entry
                .get("new_memory")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let timestamp = entry
                .get("created_at")
                .or_else(|| entry.get("timestamp"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            lines.push(format!(
                "  {idx}. [{event}] at {timestamp}\n     old: {old_memory}\n     new: {new_memory}",
                idx = i + 1,
            ));
        }

        Ok(lines.join("\n"))
    }
}

#[async_trait]
impl Memory for Mem0Memory {
    fn name(&self) -> &str {
        "mem0"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let cat_str = category.to_string();
        let effective_user = self.effective_user_id(session_id);
        let body = AddMemoryRequest {
            user_id: effective_user,
            text: content,
            metadata: Some(Mem0Metadata {
                key,
                category: &cat_str,
                session_id,
            }),
            infer: self.infer,
            app: Some(&self.app_name),
        };

        let resp = self
            .client
            .post(self.api_url("/memories/"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("mem0 store failed ({status}): {text}");
        }

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.recall_filtered(query, limit, session_id, None, None, None)
            .await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        // mem0 doesn't have a get-by-key API, so we search by key in metadata
        let results = self.recall(key, 1, None).await?;
        Ok(results.into_iter().find(|e| e.key == key))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let effective_user = self.effective_user_id(session_id);
        let resp = self
            .client
            .get(self.api_url("/memories/"))
            .query(&[
                ("user_id", effective_user),
                ("size", "100"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("mem0 list failed ({status}): {text}");
        }

        let list: Mem0ListResponse = resp.json().await?;
        let entries: Vec<MemoryEntry> = list.items.into_iter().map(|i| self.to_entry(i)).collect();

        // Client-side category filter (mem0 API doesn't filter by metadata)
        match category {
            Some(cat) => Ok(entries.into_iter().filter(|e| &e.category == cat).collect()),
            None => Ok(entries),
        }
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        // Find the memory ID by key first
        let entry = self.get(key).await?;
        let entry = match entry {
            Some(e) => e,
            None => return Ok(false),
        };

        let body = DeleteMemoriesRequest {
            memory_ids: vec![&entry.id],
            user_id: &self.user_id,
        };

        let resp = self
            .client
            .delete(self.api_url("/memories/"))
            .json(&body)
            .send()
            .await?;

        Ok(resp.status().is_success())
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let resp = self
            .client
            .get(self.api_url("/memories/"))
            .query(&[
                ("user_id", self.user_id.as_str()),
                ("size", "1"),
                ("page", "1"),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("mem0 count failed ({status}): {text}");
        }

        let list: Mem0ListResponse = resp.json().await?;
        Ok(list.total)
    }

    async fn health_check(&self) -> bool {
        self.client
            .get(self.api_url("/memories/"))
            .query(&[
                ("user_id", self.user_id.as_str()),
                ("size", "1"),
                ("page", "1"),
            ])
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Mem0Memory::store_procedural(self, messages, session_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Mem0Config {
        Mem0Config {
            url: "http://localhost:8765".into(),
            user_id: "test-user".into(),
            app_name: "test-app".into(),
            infer: true,
        }
    }

    #[test]
    fn new_rejects_empty_url() {
        let config = Mem0Config {
            url: String::new(),
            ..test_config()
        };
        assert!(Mem0Memory::new(&config).is_err());
    }

    #[test]
    fn new_trims_trailing_slash() {
        let config = Mem0Config {
            url: "http://localhost:8765/".into(),
            ..test_config()
        };
        let mem = Mem0Memory::new(&config).unwrap();
        assert_eq!(mem.base_url, "http://localhost:8765");
    }

    #[test]
    fn api_url_builds_correct_path() {
        let mem = Mem0Memory::new(&test_config()).unwrap();
        assert_eq!(
            mem.api_url("/memories/"),
            "http://localhost:8765/api/v1/memories/"
        );
    }

    #[test]
    fn to_entry_maps_unix_timestamp() {
        let mem = Mem0Memory::new(&test_config()).unwrap();
        let item = Mem0MemoryItem {
            id: "id-1".into(),
            memory: "hello".into(),
            created_at: Some(serde_json::json!(1_700_000_000)),
            metadata: Some(Mem0ResponseMetadata {
                key: Some("k1".into()),
                category: Some("core".into()),
                session_id: None,
            }),
            score: Some(0.95),
        };
        let entry = mem.to_entry(item);
        assert_eq!(entry.id, "id-1");
        assert_eq!(entry.key, "k1");
        assert_eq!(entry.category, MemoryCategory::Core);
        assert!(!entry.timestamp.is_empty());
        assert_eq!(entry.score, Some(0.95));
    }

    #[test]
    fn to_entry_maps_string_timestamp() {
        let mem = Mem0Memory::new(&test_config()).unwrap();
        let item = Mem0MemoryItem {
            id: "id-2".into(),
            memory: "world".into(),
            created_at: Some(serde_json::json!("2024-01-01T00:00:00Z")),
            metadata: None,
            score: None,
        };
        let entry = mem.to_entry(item);
        assert_eq!(entry.timestamp, "2024-01-01T00:00:00Z");
        assert_eq!(entry.category, MemoryCategory::Core); // default
    }

    #[test]
    fn to_entry_handles_missing_metadata() {
        let mem = Mem0Memory::new(&test_config()).unwrap();
        let item = Mem0MemoryItem {
            id: "id-3".into(),
            memory: "bare".into(),
            created_at: None,
            metadata: None,
            score: None,
        };
        let entry = mem.to_entry(item);
        assert_eq!(entry.key, "");
        assert_eq!(entry.category, MemoryCategory::Core);
        assert!(entry.timestamp.is_empty());
        assert_eq!(entry.score, None);
    }

    #[test]
    fn to_entry_custom_category() {
        let mem = Mem0Memory::new(&test_config()).unwrap();
        let item = Mem0MemoryItem {
            id: "id-4".into(),
            memory: "custom".into(),
            created_at: None,
            metadata: Some(Mem0ResponseMetadata {
                key: Some("k".into()),
                category: Some("project_notes".into()),
                session_id: Some("s1".into()),
            }),
            score: None,
        };
        let entry = mem.to_entry(item);
        assert_eq!(entry.category, MemoryCategory::Custom("project_notes".into()));
        assert_eq!(entry.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn name_returns_mem0() {
        let mem = Mem0Memory::new(&test_config()).unwrap();
        assert_eq!(mem.name(), "mem0");
    }

    #[test]
    fn procedural_request_serializes_messages() {
        let messages = vec![
            ProceduralMessage {
                role: "user".into(),
                content: "How do I deploy?".into(),
                name: None,
            },
            ProceduralMessage {
                role: "tool".into(),
                content: "deployment started".into(),
                name: Some("shell".into()),
            },
            ProceduralMessage {
                role: "assistant".into(),
                content: "Deployment complete.".into(),
                name: None,
            },
        ];
        let req = AddProceduralRequest {
            user_id: "test-user",
            messages: &messages,
            metadata: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["user_id"], "test-user");
        let msgs = json["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["name"], "shell");
        // metadata should be absent when None
        assert!(json.get("metadata").is_none());
    }
}
