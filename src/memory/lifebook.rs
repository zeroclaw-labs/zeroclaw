//! Lifebook-agent memory backend for ZeroClaw.
//!
//! Delegates all memory operations to the lifebook-agent HTTP service,
//! giving ZeroClaw semantic memory backed by:
//!
//! - ONNX nomic-embed-text embeddings (real vector similarity, not char n-grams)
//! - RVF persistent world-model (survives restarts, compacts automatically)
//! - Knowledge graph context expansion (related node recall)
//! - SONA adaptive learning (improves with every interaction)
//! - 4-tier routing via `/ask` (`qwen3.5:0.8b` / `qwen3.5:9b` / `qwen3.5:27b` / cloud)
//! - `/learning_report` telemetry for cloud rescues and local-promotion candidates
//!
//! # Configuration
//! In `agent-config/config.toml`:
//! ```toml
//! memory_backend = "lifebook"
//! ```
//!
//! Override the endpoint (default `http://127.0.0.1:9090`):
//! ```bash
//! LIFEBOOK_URL=http://127.0.0.1:9090
//! ```

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::traits::{Memory, MemoryCategory, MemoryEntry};

const DEFAULT_LIFEBOOK_URL: &str = "http://127.0.0.1:9090";

fn lifebook_url() -> String {
    std::env::var("LIFEBOOK_URL")
        .unwrap_or_else(|_| DEFAULT_LIFEBOOK_URL.to_string())
}

// ── Wire types (mirror lifebook-agent http_bridge.rs) ────────────────────────

#[derive(Serialize)]
struct IngestReq<'a> {
    content: &'a str,
    sender: &'a str,
    channel: &'a str,
    tags: Vec<String>,
    is_important: bool,
}

#[derive(Serialize)]
struct RecallReq<'a> {
    query: &'a str,
    k: usize,
    filter_kind: Option<String>,
}

#[derive(Deserialize)]
struct RecallResp {
    results: Vec<RecallHit>,
}

#[derive(Deserialize)]
struct RecallHit {
    content: String,
    score: f32,
    metadata: HashMap<String, String>,
}

// ── Backend ───────────────────────────────────────────────────────────────────

/// ZeroClaw memory backend backed by the lifebook-agent HTTP service.
pub struct LifebookMemory {
    client: Client,
}

impl LifebookMemory {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl Default for LifebookMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Memory for LifebookMemory {
    fn name(&self) -> &str {
        "lifebook"
    }

    // ── store ─────────────────────────────────────────────────────────────────

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let mut tags = vec![key.to_string(), category.to_string()];
        if let Some(sid) = session_id {
            tags.push(sid.to_string());
        }

        let body = IngestReq {
            content,
            sender: "zeroclaw",
            channel: "memory",
            tags,
            is_important: matches!(category, MemoryCategory::Core),
        };

        let url = format!("{}/ingest", lifebook_url());
        let res = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("lifebook /ingest unreachable: {e}"))?;

        if !res.status().is_success() {
            return Err(anyhow!("lifebook /ingest returned {}", res.status()));
        }
        Ok(())
    }

    // ── recall ────────────────────────────────────────────────────────────────

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        _session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let body = RecallReq {
            query,
            k: limit,
            filter_kind: None,
        };

        let url = format!("{}/recall", lifebook_url());
        let resp: RecallResp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("lifebook /recall unreachable: {e}"))?
            .json()
            .await
            .map_err(|e| anyhow!("lifebook /recall bad response: {e}"))?;

        let entries = resp
            .results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                let id = r
                    .metadata
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| i.to_string());
                MemoryEntry {
                    id: id.clone(),
                    key: id,
                    content: r.content,
                    category: MemoryCategory::Core,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    session_id: None,
                    // distance → similarity: lower distance = higher similarity
                    score: Some((1.0 - r.score as f64).max(0.0)),
                }
            })
            .collect();

        Ok(entries)
    }

    // ── get ───────────────────────────────────────────────────────────────────

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let results = self.recall(key, 1, None).await?;
        Ok(results.into_iter().next())
    }

    // ── list ──────────────────────────────────────────────────────────────────

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Recall with a neutral query to surface the most relevant stored memories
        self.recall("summary context overview", 50, None).await
    }

    // ── forget ────────────────────────────────────────────────────────────────

    async fn forget(&self, _key: &str) -> Result<bool> {
        // lifebook-agent has no delete endpoint yet; return false gracefully
        // Will be wired when /forget is added to http_bridge.rs
        Ok(false)
    }

    // ── count ─────────────────────────────────────────────────────────────────

    async fn count(&self) -> Result<usize> {
        // lifebook-agent /health returns "ok" string; can't get count yet.
        // Approximate via a large recall and count results.
        let results = self.recall("*", 100, None).await.unwrap_or_default();
        Ok(results.len())
    }

    // ── health_check ──────────────────────────────────────────────────────────

    async fn health_check(&self) -> bool {
        let url = format!("{}/health", lifebook_url());
        self.client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    // ── send_feedback ─────────────────────────────────────────────────────────

    /// Forward quality signal to lifebook's SONA engine so it can adapt
    /// retrieval quality based on whether responses were actually useful.
    async fn send_feedback(&self, quality: f32) {
        let url = format!("{}/feedback", lifebook_url());
        let _ = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "quality": quality }))
            .send()
            .await;
    }
}
