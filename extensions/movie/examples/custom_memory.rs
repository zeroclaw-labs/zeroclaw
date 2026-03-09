//! Example: Implementing a custom Memory backend for ZeroClaw
//!
//! This demonstrates how to create a Redis-backed memory backend.
//! The Memory trait is async and pluggable — implement it for any storage.
//!
//! Run: cargo run --example custom_memory

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

// ── Re-define the trait types (in your app, import from zeroclaw::memory) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryCategory {
    Core,
    Daily,
    Conversation,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub timestamp: String,
    pub score: Option<f64>,
}

#[async_trait]
pub trait Memory: Send + Sync {
    fn name(&self) -> &str;
    async fn store(&self, key: &str, content: &str, category: MemoryCategory)
        -> anyhow::Result<()>;
    async fn recall(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryEntry>>;
    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>>;
    async fn forget(&self, key: &str) -> anyhow::Result<bool>;
    async fn count(&self) -> anyhow::Result<usize>;
}

// ── Your custom implementation ─────────────────────────────────────

/// In-memory HashMap backend (great for testing or ephemeral sessions)
pub struct InMemoryBackend {
    store: Mutex<HashMap<String, MemoryEntry>>,
}

impl Default for InMemoryBackend {
    fn default() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
        }
    }
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Memory for InMemoryBackend {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
    ) -> anyhow::Result<()> {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            key: key.to_string(),
            content: content.to_string(),
            category,
            timestamp: chrono::Local::now().to_rfc3339(),
            score: None,
        };
        self.store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .insert(key.to_string(), entry);
        Ok(())
    }

    async fn recall(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryEntry>> {
        let store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let query_lower = query.to_lowercase();

        let mut results: Vec<MemoryEntry> = store
            .values()
            .filter(|e| e.content.to_lowercase().contains(&query_lower))
            .cloned()
            .collect();

        results.truncate(limit);
        Ok(results)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(store.get(key).cloned())
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let mut store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(store.remove(key).is_some())
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let store = self.store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(store.len())
    }
}

// ── Demo usage ─────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let brain = InMemoryBackend::new();

    println!("🧠 ZeroClaw Memory Demo — InMemoryBackend\n");

    // Store some memories
    brain
        .store("user_lang", "User prefers Rust", MemoryCategory::Core)
        .await?;
    brain
        .store("user_tz", "Timezone is EST", MemoryCategory::Core)
        .await?;
    brain
        .store(
            "today_note",
            "Completed memory system implementation",
            MemoryCategory::Daily,
        )
        .await?;

    println!("Stored {} memories", brain.count().await?);

    // Recall by keyword
    let results = brain.recall("Rust", 5).await?;
    println!("\nRecall 'Rust' → {} results:", results.len());
    for entry in &results {
        println!("  [{:?}] {}: {}", entry.category, entry.key, entry.content);
    }

    // Get by key
    if let Some(entry) = brain.get("user_tz").await? {
        println!("\nGet 'user_tz' → {}", entry.content);
    }

    // Forget
    let removed = brain.forget("user_tz").await?;
    println!("Forget 'user_tz' → removed: {removed}");
    println!("Remaining: {} memories", brain.count().await?);

    println!("\n✅ Memory backend works! Implement the Memory trait for any storage.");
    Ok(())
}
