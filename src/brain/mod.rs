//! Embedded brain module — full memory engine compiled into zeroclaw.
//!
//! Provides `EmbeddedBrainMemory` which implements the `Memory` trait using
//! in-process RVF + SONA + Knowledge Graph + Neural Filter.

mod knowledge_graph;
mod learning;
mod neural_filter;
mod rvf_memory;
mod text_store;
mod vault_watcher;

pub use learning::{LearningBrain, EMBED_DIM};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use ruvector_onnx_embeddings::{Embedder, EmbedderConfig};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};

/// Embedded brain memory backend — full cognitive engine in-process.
pub struct EmbeddedBrainMemory {
    brain: Arc<Mutex<LearningBrain>>,
    embedder: Arc<Mutex<Embedder>>,
    /// Keep watcher alive — dropping it stops file watching.
    _vault_watcher: Option<notify::RecommendedWatcher>,
}

impl EmbeddedBrainMemory {
    /// Create a new embedded brain from config paths.
    pub async fn new(
        rvf_path: &str,
        db_path: &str,
        branch: &str,
        vault_path: Option<&str>,
    ) -> Result<Self> {
        let brain = LearningBrain::new(rvf_path, db_path, branch).await?;
        let config = EmbedderConfig::default();
        let embedder = Embedder::new(config)
            .await
            .map_err(|e| anyhow!("ONNX embedder init failed: {e}"))?;

        let brain_arc = Arc::new(Mutex::new(brain));
        let embedder_arc = Arc::new(Mutex::new(embedder));

        // Optionally start vault watcher
        let vault_watcher = if let Some(vp) = vault_path {
            let vp = std::path::PathBuf::from(vp);
            if vp.is_dir() {
                match vault_watcher::spawn_vault_watcher(
                    vp,
                    Arc::clone(&brain_arc),
                    Arc::clone(&embedder_arc),
                ) {
                    Ok(w) => Some(w),
                    Err(e) => {
                        tracing::warn!(error = %e, "vault watcher failed to start");
                        None
                    }
                }
            } else {
                tracing::warn!(path = vp.display().to_string(), "vault path not a directory");
                None
            }
        } else {
            None
        };

        let instance = Self {
            brain: brain_arc,
            embedder: embedder_arc,
            _vault_watcher: vault_watcher,
        };

        // Spawn background consolidation (5 min interval)
        let brain_bg = Arc::clone(&instance.brain);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                let mut brain = brain_bg.lock().await;
                if let Err(e) = brain.background_consolidation().await {
                    tracing::warn!(error = %e, "background consolidation failed");
                }
            }
        });

        // Spawn hourly reinforce signal
        let brain_hr = Arc::clone(&instance.brain);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                let mut brain = brain_hr.lock().await;
                brain.record_feedback(0.75);
            }
        });

        // Spawn daily backfill
        let brain_daily = Arc::clone(&instance.brain);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400));
            loop {
                interval.tick().await;
                let mut brain = brain_daily.lock().await;
                let _ = brain.backfill_learn_telemetry();
            }
        });

        Ok(instance)
    }

    /// Get a clone of the brain Arc for direct access (tools, reports, etc.).
    pub fn brain(&self) -> Arc<Mutex<LearningBrain>> {
        Arc::clone(&self.brain)
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embedder
            .lock()
            .await
            .embed_one(text)
            .map_err(|e| anyhow!("embedding failed: {e}"))
    }
}

#[async_trait]
impl Memory for EmbeddedBrainMemory {
    fn name(&self) -> &str {
        "embedded"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let embedding = self.embed(content).await?;
        let cat_str = category.to_string();
        let mut tags = vec![key, cat_str.as_str()];
        let sid_owned;
        if let Some(sid) = session_id {
            sid_owned = sid.to_string();
            tags.push(&sid_owned);
        }
        let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_ref()).collect();

        let mut brain = self.brain.lock().await;
        brain
            .process_and_learn(
                content,
                embedding,
                "zeroclaw",
                "memory",
                &tag_refs,
                matches!(category, MemoryCategory::Core),
            )
            .await?;
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        _session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let embedding = self.embed(query).await?;
        let mut brain = self.brain.lock().await;
        let results = brain.smart_recall(embedding, limit).await?;

        let entries = results
            .into_iter()
            .enumerate()
            .map(|(i, (r, text))| {
                let content = text.unwrap_or_default();
                MemoryEntry {
                    id: i.to_string(),
                    key: i.to_string(),
                    content,
                    category: MemoryCategory::Core,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    session_id: None,
                    score: Some((1.0 - r.distance as f64).max(0.0)),
                }
            })
            .collect();

        Ok(entries)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let results = self.recall(key, 1, None).await?;
        Ok(results.into_iter().next())
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.recall("summary context overview", 50, None).await
    }

    async fn forget(&self, _key: &str) -> Result<bool> {
        // RVF delete requires vector IDs — not wired yet
        Ok(false)
    }

    async fn count(&self) -> Result<usize> {
        let brain = self.brain.lock().await;
        brain.rvf_memory.text_store.count()
    }

    async fn health_check(&self) -> bool {
        let brain = self.brain.lock().await;
        let status = brain.rvf_memory.store.lock().await.status();
        status.total_vectors > 0 || brain.rvf_memory.text_store.count().unwrap_or(0) == 0
    }

    async fn send_feedback(&self, quality: f32) {
        let mut brain = self.brain.lock().await;
        brain.record_feedback(quality);
    }
}
