//! EverOS HTTP memory backend.
//!
//! Talks to a local EverOS sidecar (`everos server start`) over `/api/v2/memory/*`.
//! Optionally keeps a SQLite hot cache for turn latency and offline fallback
//! (Lucid-style hybrid).

use super::sqlite::SqliteMemory;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use zeroclaw_config::schema::EverosStorageConfig;

const KEY_MARKER_PREFIX: &str = "[zeroclaw key=";
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const FAILURE_COOLDOWN_MS: u64 = 15_000;

/// EverOS-backed memory with optional local SQLite cache.
pub struct EverOSMemory {
    alias: String,
    client: reqwest::Client,
    base_url: String,
    app_id: String,
    project_id: String,
    user_id: String,
    search_method: String,
    timeout: Duration,
    local: Option<SqliteMemory>,
    failure_cooldown: Duration,
    last_failure_at: Mutex<Option<Instant>>,
}

impl EverOSMemory {
    /// Build from a resolved `[storage.everos.<alias>]` entry.
    pub fn from_config(
        alias: &str,
        cfg: &EverosStorageConfig,
        local: Option<SqliteMemory>,
    ) -> Self {
        let timeout_ms = cfg
            .timeout_ms
            .filter(|ms| *ms > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let timeout = Duration::from_millis(timeout_ms);
        let timeout_secs = timeout_ms.div_ceil(1000).max(1);
        let client = zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "memory.everos",
            timeout_secs,
            timeout_secs,
        );
        let method = cfg.search_method.trim().to_ascii_lowercase();
        let search_method = match method.as_str() {
            "keyword" | "vector" | "hybrid" | "agentic" => method,
            _ => "hybrid".to_string(),
        };

        Self {
            alias: alias.to_string(),
            client,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            app_id: cfg.app_id.clone(),
            project_id: cfg.project_id.clone(),
            user_id: cfg.user_id.clone(),
            search_method,
            timeout,
            local,
            failure_cooldown: Duration::from_millis(FAILURE_COOLDOWN_MS),
            last_failure_at: Mutex::new(None),
        }
    }

    fn mark_failure(&self) {
        *self.last_failure_at.lock() = Some(Instant::now());
    }

    fn in_cooldown(&self) -> bool {
        self.last_failure_at
            .lock()
            .is_some_and(|at| at.elapsed() < self.failure_cooldown)
    }

    fn clear_failure(&self) {
        *self.last_failure_at.lock() = None;
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/api/v2{}", self.base_url, path)
    }

    fn encode_content(key: &str, content: &str, category: &MemoryCategory) -> String {
        format!("{KEY_MARKER_PREFIX}{key} category={category}]\n{content}")
    }

    fn parse_key_from_content(content: &str) -> Option<String> {
        let rest = content.strip_prefix(KEY_MARKER_PREFIX)?;
        let (key, _) = rest.split_once(' ')?;
        if key.is_empty() {
            None
        } else {
            Some(key.to_string())
        }
    }

    fn category_from_content(content: &str) -> MemoryCategory {
        if let Some(rest) = content.strip_prefix(KEY_MARKER_PREFIX)
            && let Some((_, after_key)) = rest.split_once(" category=")
            && let Some((cat, _)) = after_key.split_once(']')
        {
            return match cat.trim() {
                "core" => MemoryCategory::Core,
                "daily" => MemoryCategory::Daily,
                "conversation" => MemoryCategory::Conversation,
                other => MemoryCategory::Custom(other.to_string()),
            };
        }
        MemoryCategory::Conversation
    }

    fn strip_marker(content: &str) -> String {
        if let Some((_, body)) = content.split_once("]\n") {
            body.to_string()
        } else {
            content.to_string()
        }
    }

    fn session_or_default(session_id: Option<&str>) -> String {
        session_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("zeroclaw-default")
            .to_string()
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        let url = self.api_url(path);
        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .with_context(|| format!("EverOS request failed: POST {path}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("EverOS response body unreadable")?;
        if !status.is_success() {
            anyhow::bail!("EverOS {path} returned {status}: {text}");
        }
        let value: Value =
            serde_json::from_str(&text).context("EverOS response was not valid JSON")?;
        Ok(value)
    }

    async fn add_and_flush(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let session = Self::session_or_default(session_id);
        let encoded = Self::encode_content(key, content, &category);
        let now_ms = Utc::now().timestamp_millis();
        let sender = agent_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(self.user_id.as_str());

        let add_body = json!({
            "session_id": session,
            "app_id": self.app_id,
            "project_id": self.project_id,
            "messages": [{
                "sender_id": sender,
                "role": "user",
                "timestamp": now_ms,
                "content": encoded,
            }],
        });
        self.post_json("/memory/add", add_body).await?;

        let flush_body = json!({
            "session_id": session,
            "app_id": self.app_id,
            "project_id": self.project_id,
        });
        // Flush is best-effort; add already persisted the buffer.
        if let Err(err) = self.post_json("/memory/flush", flush_body).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                "EverOS flush failed after add; markdown may still be durable after next flush"
            );
        }
        Ok(())
    }

    fn entry_from_hit(hit: &Value, fallback_category: MemoryCategory) -> Option<MemoryEntry> {
        let id = hit
            .get("id")
            .or_else(|| hit.get("episode_id"))
            .or_else(|| hit.get("memory_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let raw_content = hit
            .get("content")
            .or_else(|| hit.get("text"))
            .or_else(|| hit.get("summary"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                hit.get("atomic_facts")
                    .and_then(Value::as_array)
                    .map(|facts| {
                        facts
                            .iter()
                            .filter_map(|f| {
                                f.as_str()
                                    .or_else(|| f.get("text").and_then(Value::as_str))
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
            })
            .unwrap_or_default();
        if id.is_empty() && raw_content.is_empty() {
            return None;
        }
        let key = Self::parse_key_from_content(&raw_content)
            .or_else(|| hit.get("key").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| {
                if id.is_empty() {
                    format!("everos-{}", uuid::Uuid::new_v4())
                } else {
                    id.clone()
                }
            });
        let category = if Self::parse_key_from_content(&raw_content).is_some() {
            Self::category_from_content(&raw_content)
        } else {
            fallback_category
        };
        let content = Self::strip_marker(&raw_content);
        let score = hit
            .get("score")
            .or_else(|| hit.get("relevance"))
            .and_then(Value::as_f64);
        let session_id = hit
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let timestamp = hit
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let timestamp = if timestamp.is_empty() {
            Utc::now().to_rfc3339()
        } else {
            timestamp
        };
        Some(MemoryEntry {
            id: if id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                id
            },
            key,
            content,
            category,
            timestamp,
            session_id,
            score,
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            principal_id: None,
            agent_alias: None,
            agent_id: None,
        })
    }

    fn collect_entries(data: &Value, limit: usize) -> Vec<MemoryEntry> {
        let mut out = Vec::new();
        let buckets = [
            ("episodes", MemoryCategory::Conversation),
            ("profiles", MemoryCategory::Core),
            ("agent_cases", MemoryCategory::Daily),
            ("agent_skills", MemoryCategory::Core),
            ("unprocessed_messages", MemoryCategory::Conversation),
        ];
        for (field, category) in buckets {
            let Some(items) = data.get(field).and_then(Value::as_array) else {
                continue;
            };
            for item in items {
                if out.len() >= limit {
                    return out;
                }
                if let Some(entry) = Self::entry_from_hit(item, category.clone()) {
                    out.push(entry);
                }
            }
        }
        out
    }

    async fn search_remote(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let mut body = json!({
            "query": if query.trim().is_empty() || query.trim() == "*" { "*" } else { query },
            "method": self.search_method,
            "top_k": limit.max(1),
            "app_id": self.app_id,
            "project_id": self.project_id,
            "include_profile": true,
        });
        if let Some(agent) = agent_id.map(str::trim).filter(|s| !s.is_empty()) {
            body["agent_id"] = json!(agent);
        } else {
            body["user_id"] = json!(self.user_id);
        }
        if let Some(session) = session_id.map(str::trim).filter(|s| !s.is_empty()) {
            body["filters"] = json!({
                "session_id": session,
            });
        }

        let response = self.post_json("/memory/search", body).await?;
        let data = response.get("data").unwrap_or(&response);
        Ok(Self::collect_entries(data, limit.max(1)))
    }

    async fn get_remote_page(
        &self,
        memory_type: &str,
        page_size: usize,
        agent_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let mut body = json!({
            "memory_type": memory_type,
            "page": 1,
            "page_size": page_size.max(1),
            "app_id": self.app_id,
            "project_id": self.project_id,
            "sort_by": "timestamp",
            "sort_order": "desc",
        });
        if let Some(agent) = agent_id.map(str::trim).filter(|s| !s.is_empty()) {
            body["agent_id"] = json!(agent);
        } else {
            body["user_id"] = json!(self.user_id);
        }
        let response = self.post_json("/memory/get", body).await?;
        let data = response.get("data").unwrap_or(&response);
        let category = match memory_type {
            "profile" | "agent_skill" => MemoryCategory::Core,
            "agent_case" => MemoryCategory::Daily,
            _ => MemoryCategory::Conversation,
        };
        let field = match memory_type {
            "profile" => "profiles",
            "agent_case" => "agent_cases",
            "agent_skill" => "agent_skills",
            _ => "episodes",
        };
        let mut out = Vec::new();
        if let Some(items) = data.get(field).and_then(Value::as_array) {
            for item in items {
                if let Some(entry) = Self::entry_from_hit(item, category.clone()) {
                    out.push(entry);
                }
            }
        }
        Ok(out)
    }

    async fn delete_by_ids(&self, ids: &[String]) -> Result<bool> {
        if ids.is_empty() {
            return Ok(false);
        }
        let body = json!({
            "ids": ids,
            "app_id": self.app_id,
            "project_id": self.project_id,
            "user_id": self.user_id,
        });
        match self.post_json("/memory/delete_by_ids", body).await {
            Ok(_) => Ok(true),
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "EverOS delete_by_ids failed"
                );
                Ok(false)
            }
        }
    }

    async fn purge_session_remote(&self, session_id: &str) -> Result<usize> {
        let body = json!({
            "session_id": session_id,
            "app_id": self.app_id,
            "project_id": self.project_id,
            "user_id": self.user_id,
        });
        match self.post_json("/memory/delete", body).await {
            Ok(value) => {
                let data = value.get("data").unwrap_or(&value);
                Ok(data
                    .get("deleted_count")
                    .or_else(|| data.get("count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(1) as usize)
            }
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "EverOS session delete failed"
                );
                Ok(0)
            }
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for EverOSMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(
            ::zeroclaw_api::attribution::MemoryKind::Everos,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Memory for EverOSMemory {
    fn name(&self) -> &str {
        "everos"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.store_with_agent(key, content, category, session_id, None, None, None)
            .await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.recall_for_agents(&[], query, limit, session_id, None, None)
            .await
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        if let Some(local) = &self.local
            && let Some(hit) = local.get(key).await?
        {
            return Ok(Some(hit));
        }
        if self.in_cooldown() {
            return Ok(None);
        }
        match self.search_remote(key, 20, None, None).await {
            Ok(entries) => {
                self.clear_failure();
                Ok(entries.into_iter().find(|e| e.key == key))
            }
            Err(err) => {
                self.mark_failure();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "EverOS get/search failed"
                );
                Ok(None)
            }
        }
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        if self.in_cooldown() {
            if let Some(local) = &self.local {
                return local.list(category, session_id).await;
            }
            return Ok(Vec::new());
        }
        let memory_type = match category {
            Some(MemoryCategory::Core) => "profile",
            Some(MemoryCategory::Daily) => "agent_case",
            _ => "episode",
        };
        match self.get_remote_page(memory_type, 100, None).await {
            Ok(mut entries) => {
                self.clear_failure();
                if let Some(session) = session_id {
                    entries.retain(|e| e.session_id.as_deref() == Some(session));
                }
                if let Some(cat) = category {
                    entries.retain(|e| &e.category == cat);
                }
                Ok(entries)
            }
            Err(err) => {
                self.mark_failure();
                if let Some(local) = &self.local {
                    return local.list(category, session_id).await;
                }
                Err(err)
            }
        }
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let mut removed = false;
        if let Some(local) = &self.local {
            removed = local.forget(key).await? || removed;
        }
        if self.in_cooldown() {
            return Ok(removed);
        }
        match self.search_remote(key, 50, None, None).await {
            Ok(entries) => {
                self.clear_failure();
                let ids: Vec<String> = entries
                    .into_iter()
                    .filter(|e| e.key == key)
                    .map(|e| e.id)
                    .collect();
                removed = self.delete_by_ids(&ids).await? || removed;
            }
            Err(err) => {
                self.mark_failure();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "EverOS forget search failed"
                );
            }
        }
        Ok(removed)
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> Result<bool> {
        let mut removed = false;
        if let Some(local) = &self.local {
            removed = local.forget_for_agent(key, agent_id).await? || removed;
        }
        if self.in_cooldown() {
            return Ok(removed);
        }
        match self.search_remote(key, 50, None, Some(agent_id)).await {
            Ok(entries) => {
                self.clear_failure();
                let ids: Vec<String> = entries
                    .into_iter()
                    .filter(|e| e.key == key)
                    .map(|e| e.id)
                    .collect();
                removed = self.delete_by_ids(&ids).await? || removed;
            }
            Err(err) => {
                self.mark_failure();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "EverOS forget_for_agent search failed"
                );
            }
        }
        Ok(removed)
    }

    async fn purge_session(&self, session_id: &str) -> Result<usize> {
        let mut count = 0usize;
        if let Some(local) = &self.local {
            count += local.purge_session(session_id).await.unwrap_or(0);
        }
        count += self.purge_session_remote(session_id).await?;
        Ok(count)
    }

    async fn purge_session_for_agent(&self, session_id: &str, agent_id: &str) -> Result<usize> {
        let mut count = 0usize;
        if let Some(local) = &self.local {
            count += local
                .purge_session_for_agent(session_id, agent_id)
                .await
                .unwrap_or(0);
        }
        // EverOS session delete is scoped by user/app/project; agent filter is best-effort.
        count += self.purge_session_remote(session_id).await?;
        Ok(count)
    }

    async fn count(&self) -> Result<usize> {
        if self.in_cooldown() {
            if let Some(local) = &self.local {
                return local.count().await;
            }
            return Ok(0);
        }
        match self.get_remote_page("episode", 1, None).await {
            Ok(_) => {
                // Prefer total_count from a fresh get call.
                let body = json!({
                    "memory_type": "episode",
                    "page": 1,
                    "page_size": 1,
                    "app_id": self.app_id,
                    "project_id": self.project_id,
                    "user_id": self.user_id,
                });
                match self.post_json("/memory/get", body).await {
                    Ok(value) => {
                        self.clear_failure();
                        let data = value.get("data").unwrap_or(&value);
                        Ok(data
                            .get("total_count")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as usize)
                    }
                    Err(err) => {
                        self.mark_failure();
                        if let Some(local) = &self.local {
                            return local.count().await;
                        }
                        Err(err)
                    }
                }
            }
            Err(err) => {
                self.mark_failure();
                if let Some(local) = &self.local {
                    return local.count().await;
                }
                Err(err)
            }
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/health", self.base_url);
        match self.client.get(&url).timeout(self.timeout).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.clear_failure();
                true
            }
            Ok(_) | Err(_) => {
                self.mark_failure();
                // SQLite cache keeps the agent loop usable while EverOS is down.
                if let Some(local) = &self.local {
                    local.health_check().await
                } else {
                    false
                }
            }
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
        agent_id: Option<&str>,
    ) -> Result<()> {
        if let Some(local) = &self.local {
            local
                .store_with_agent(
                    key,
                    content,
                    category.clone(),
                    session_id,
                    None,
                    None,
                    agent_id,
                )
                .await?;
        }

        if self.in_cooldown() {
            return Ok(());
        }

        match self
            .add_and_flush(key, content, category, session_id, agent_id)
            .await
        {
            Ok(()) => {
                self.clear_failure();
                Ok(())
            }
            Err(err) => {
                self.mark_failure();
                if self.local.is_some() {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                        "EverOS store failed; kept SQLite cache write"
                    );
                    Ok(())
                } else {
                    Err(err)
                }
            }
        }
    }

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        if self.in_cooldown() {
            if let Some(local) = &self.local {
                return local
                    .recall_for_agents(allowed_agent_ids, query, limit, session_id, None, None)
                    .await;
            }
            return Ok(Vec::new());
        }

        let agent_id = allowed_agent_ids.first().copied();
        match self
            .search_remote(query, limit, session_id, agent_id)
            .await
        {
            Ok(entries) => {
                self.clear_failure();
                Ok(entries)
            }
            Err(err) => {
                self.mark_failure();
                if let Some(local) = &self.local {
                    return local
                        .recall_for_agents(allowed_agent_ids, query, limit, session_id, None, None)
                        .await;
                }
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn read_http_request(stream: &mut impl Read) -> String {
        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).unwrap_or(0);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    fn respond(stream: &mut impl Write, status: &str, body: &str) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    fn test_config(base_url: &str, local_cache: bool) -> EverosStorageConfig {
        EverosStorageConfig {
            base_url: base_url.to_string(),
            app_id: "zeroclaw".into(),
            project_id: "default".into(),
            user_id: "local".into(),
            timeout_ms: Some(2_000),
            search_method: "keyword".into(),
            local_cache,
        }
    }

    #[tokio::test]
    async fn store_posts_add_and_flush() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for expected_path in ["/api/v2/memory/add", "/api/v2/memory/flush"] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(
                    request.contains(expected_path),
                    "expected {expected_path} in {request}"
                );
                if expected_path.ends_with("/add") {
                    assert!(request.contains("zeroclaw key=pref"));
                }
                respond(
                    &mut stream,
                    "200 OK",
                    &json!({"request_id": "abc", "data": {"status": "extracted"}}).to_string(),
                );
            }
        });

        let mem = EverOSMemory::from_config("default", &test_config(&endpoint, false), None);
        mem.store("pref", "likes rust", MemoryCategory::Core, Some("s1"))
            .await
            .unwrap();
        server.join().unwrap();
    }

    #[tokio::test]
    async fn recall_maps_search_hits() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.contains("/api/v2/memory/search"));
            let body = json!({
                "request_id": "abc",
                "data": {
                    "episodes": [{
                        "id": "ep1",
                        "content": "[zeroclaw key=pref category=core]\nlikes rust",
                        "score": 0.9,
                        "session_id": "s1",
                        "timestamp": "2026-11-18T00:00:00Z"
                    }],
                    "profiles": [],
                    "agent_cases": [],
                    "agent_skills": [],
                    "unprocessed_messages": []
                }
            })
            .to_string();
            respond(&mut stream, "200 OK", &body);
        });

        let mem = EverOSMemory::from_config("default", &test_config(&endpoint, false), None);
        let hits = mem
            .recall("rust", 5, Some("s1"), None, None)
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "pref");
        assert_eq!(hits[0].content, "likes rust");
        assert!(matches!(hits[0].category, MemoryCategory::Core));
    }

    #[tokio::test]
    async fn health_check_hits_health_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.contains("GET /health"));
            respond(&mut stream, "200 OK", r#"{"status":"ok"}"#);
        });

        let mem = EverOSMemory::from_config("default", &test_config(&endpoint, false), None);
        assert!(mem.health_check().await);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn store_falls_back_to_cache_when_everos_down() {
        let dir = tempfile::tempdir().unwrap();
        let local = SqliteMemory::new("sqlite", dir.path()).unwrap();
        // Bind nothing that accepts connections — use a closed port.
        let endpoint = "http://127.0.0.1:1";
        let mem = EverOSMemory::from_config("default", &test_config(endpoint, true), Some(local));
        mem.store("k", "v", MemoryCategory::Core, None)
            .await
            .unwrap();
        let hit = mem.get("k").await.unwrap();
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().content, "v");
    }

    #[tokio::test]
    async fn recall_uses_cache_during_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let local = SqliteMemory::new("sqlite", dir.path()).unwrap();
        local
            .store("cached", "from-cache", MemoryCategory::Core, None)
            .await
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        // Accept one failing search to enter cooldown, then drop.
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut stream);
            respond(&mut stream, "503 Service Unavailable", r#"{"error":{"code":"EXTERNAL_SERVICE_UNAVAILABLE","message":"down"}}"#);
        });

        let mem = EverOSMemory::from_config("default", &test_config(&endpoint, true), Some(local));
        let _ = mem.recall("cached", 5, None, None, None).await;
        server.join().unwrap();

        let hits = mem.recall("cached", 5, None, None, None).await.unwrap();
        assert!(
            hits.iter().any(|e| e.key == "cached"),
            "cooldown should serve SQLite cache"
        );
    }

    #[test]
    fn encode_parse_round_trip() {
        let encoded = EverOSMemory::encode_content("pref", "likes tea", &MemoryCategory::Core);
        assert_eq!(
            EverOSMemory::parse_key_from_content(&encoded).as_deref(),
            Some("pref")
        );
        assert!(matches!(
            EverOSMemory::category_from_content(&encoded),
            MemoryCategory::Core
        ));
        assert_eq!(EverOSMemory::strip_marker(&encoded), "likes tea");
    }
}
