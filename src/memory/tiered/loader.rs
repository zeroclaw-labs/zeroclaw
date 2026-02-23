//! TieredMemoryLoader — context loader for the three-tier memory system.
//!
//! Always injects all STM entries (split into raw entries and index entries),
//! then fills remaining token budget with MTM summaries. Output is formatted
//! as Markdown sections.

use std::fmt::Write;
use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::memory_loader::MemoryLoader;
use crate::memory::tiered::budget::estimate_tokens;
use crate::memory::tiered::facts::{FactConfidence, FactEntry, FactStatus};
use crate::memory::tiered::types::{IndexEntry, TierConfig};
use crate::memory::tiered::SharedMemory;
use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};

/// Category string used for cross-tier index entries stored in STM.
const STM_INDEX_CATEGORY: &str = "stm_index";

/// Context loader that reads from the STM and MTM tiers of a [`TieredMemory`]
/// and formats the result as Markdown sections.
///
/// STM entries are always included in full. MTM entries are added until the
/// configured `mtm_token_budget` is exhausted.
pub struct TieredMemoryLoader {
    stm: SharedMemory,
    mtm: SharedMemory,
    cfg: Arc<TierConfig>,
}

impl TieredMemoryLoader {
    /// Create a new loader that reads directly from the given tier backends.
    pub fn new(stm: SharedMemory, mtm: SharedMemory, cfg: Arc<TierConfig>) -> Self {
        Self { stm, mtm, cfg }
    }
}

#[async_trait]
impl MemoryLoader for TieredMemoryLoader {
    async fn load_context(
        &self,
        _memory: &dyn Memory,
        _user_message: &str,
    ) -> anyhow::Result<String> {
        // 1. Get all STM entries.
        let all_stm = self.stm.lock().await.list(None, None).await?;

        // 2. Split into raw entries, index entries, and fact entries.
        let mut raw_entries: Vec<MemoryEntry> = Vec::new();
        let mut index_entries: Vec<IndexEntry> = Vec::new();
        let mut fact_entries: Vec<FactEntry> = Vec::new();

        for entry in all_stm {
            if entry.key.starts_with("fact:") && entry.category == MemoryCategory::Core {
                // Try to deserialize as FactEntry; only include active facts.
                if let Ok(fact) = serde_json::from_str::<FactEntry>(&entry.content) {
                    if fact.status == FactStatus::Active {
                        fact_entries.push(fact);
                    }
                }
            } else if entry.category == MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string()) {
                // Try to deserialize as IndexEntry; fall back to ignoring.
                if let Ok(idx) = serde_json::from_str::<IndexEntry>(&entry.content) {
                    index_entries.push(idx);
                }
            } else {
                raw_entries.push(entry);
            }
        }

        // 3. Get MTM entries, add under token budget.
        let all_mtm = self.mtm.lock().await.list(None, None).await?;
        let budget = self.cfg.mtm_token_budget;
        let mut mtm_used_tokens: usize = 0;
        let mut mtm_included: Vec<MemoryEntry> = Vec::new();

        for entry in all_mtm {
            let tokens = estimate_tokens(&entry.content);
            if mtm_used_tokens + tokens > budget {
                break;
            }
            mtm_used_tokens += tokens;
            mtm_included.push(entry);
        }

        // 4. Format output as Markdown sections.
        let mut output = String::new();

        // -- Active Memory (Short-Term) --
        if !raw_entries.is_empty() {
            writeln!(output, "## Active Memory (Short-Term)").unwrap();
            writeln!(output).unwrap();
            for entry in &raw_entries {
                writeln!(output, "- **{}**: {}", entry.key, entry.content).unwrap();
            }
            writeln!(output).unwrap();
        }

        // -- Known Facts --
        if !fact_entries.is_empty() {
            writeln!(output, "## Known Facts").unwrap();
            writeln!(output).unwrap();
            for fact in &fact_entries {
                let confidence_tag = match &fact.confidence {
                    FactConfidence::High => "high",
                    FactConfidence::Medium => "medium",
                    FactConfidence::Low => "low",
                };
                writeln!(
                    output,
                    "- [{}] {} {}: {} ({})",
                    confidence_tag,
                    fact.subject,
                    fact.attribute,
                    fact.value,
                    fact.context_narrative
                )
                .unwrap();
            }
            writeln!(output).unwrap();
        }

        // -- Memory Index --
        if !index_entries.is_empty() {
            writeln!(output, "## Memory Index").unwrap();
            writeln!(output).unwrap();
            for idx in &index_entries {
                writeln!(output, "- {}", idx.to_display()).unwrap();
            }
            writeln!(output).unwrap();
        }

        // -- Medium-Term Memory (Recent Summaries) --
        if !mtm_included.is_empty() {
            writeln!(output, "## Medium-Term Memory (Recent Summaries)").unwrap();
            writeln!(output).unwrap();
            for entry in &mtm_included {
                writeln!(output, "### {}", entry.key).unwrap();
                writeln!(output).unwrap();
                writeln!(output, "{}", entry.content).unwrap();
                writeln!(output).unwrap();
            }
        }

        Ok(output)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // ── InMemoryBackend ───────────────────────────────────────────────────

    /// Simple HashMap-backed Memory implementation for testing.
    struct InMemoryBackend {
        entries: std::sync::Mutex<HashMap<String, MemoryEntry>>,
    }

    impl InMemoryBackend {
        fn new() -> Self {
            Self {
                entries: std::sync::Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl Memory for InMemoryBackend {
        fn name(&self) -> &str {
            "in-memory-test"
        }

        async fn store(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            let entry = MemoryEntry {
                id: key.to_string(),
                key: key.to_string(),
                content: content.to_string(),
                category,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: session_id.map(String::from),
                score: None,
            };
            self.entries.lock().unwrap().insert(key.to_string(), entry);
            Ok(())
        }

        async fn recall(
            &self,
            query: &str,
            limit: usize,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            let guard = self.entries.lock().unwrap();
            let results: Vec<MemoryEntry> = guard
                .values()
                .filter(|e| e.content.contains(query) || e.key.contains(query))
                .take(limit)
                .cloned()
                .collect();
            Ok(results)
        }

        async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(self.entries.lock().unwrap().get(key).cloned())
        }

        async fn list(
            &self,
            category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            let guard = self.entries.lock().unwrap();
            let results: Vec<MemoryEntry> = guard
                .values()
                .filter(|e| category.map_or(true, |c| &e.category == c))
                .cloned()
                .collect();
            Ok(results)
        }

        async fn forget(&self, key: &str) -> anyhow::Result<bool> {
            Ok(self.entries.lock().unwrap().remove(key).is_some())
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.entries.lock().unwrap().len())
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_shared(backend: InMemoryBackend) -> SharedMemory {
        Arc::new(Mutex::new(Box::new(backend)))
    }

    /// Minimal no-op Memory used as the `_memory` argument to `load_context`.
    struct NoopMemory;

    #[async_trait]
    impl Memory for NoopMemory {
        fn name(&self) -> &str {
            "noop"
        }
        async fn store(
            &self,
            _: &str,
            _: &str,
            _: MemoryCategory,
            _: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            _: &str,
            _: usize,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _: Option<&MemoryCategory>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
        async fn forget(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn empty_tiers_produce_empty_output() {
        let stm = make_shared(InMemoryBackend::new());
        let mtm = make_shared(InMemoryBackend::new());
        let cfg = Arc::new(TierConfig::default());

        let loader = TieredMemoryLoader::new(stm, mtm, cfg);
        let output = loader.load_context(&NoopMemory, "hello").await.unwrap();
        assert!(output.is_empty(), "expected empty output, got: {output}");
    }

    #[tokio::test]
    async fn stm_raw_entries_appear_in_active_memory_section() {
        let stm_backend = InMemoryBackend::new();
        stm_backend
            .store("user-pref", "likes Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        stm_backend
            .store(
                "project-note",
                "working on whiskey",
                MemoryCategory::Conversation,
                None,
            )
            .await
            .unwrap();

        let stm = make_shared(stm_backend);
        let mtm = make_shared(InMemoryBackend::new());
        let cfg = Arc::new(TierConfig::default());

        let loader = TieredMemoryLoader::new(stm, mtm, cfg);
        let output = loader.load_context(&NoopMemory, "test").await.unwrap();

        assert!(
            output.contains("## Active Memory (Short-Term)"),
            "missing STM header in:\n{output}"
        );
        assert!(
            output.contains("user-pref"),
            "missing user-pref in:\n{output}"
        );
        assert!(
            output.contains("likes Rust"),
            "missing content in:\n{output}"
        );
        assert!(
            output.contains("project-note"),
            "missing project-note in:\n{output}"
        );
    }

    #[tokio::test]
    async fn stm_index_entries_appear_in_memory_index_section() {
        let stm_backend = InMemoryBackend::new();

        // Store a raw STM entry.
        stm_backend
            .store("raw-entry", "some context", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Store an index entry (as the manager would).
        let mut idx = IndexEntry::new(
            "auth",
            chrono::NaiveDate::from_ymd_opt(2026, 2, 20).unwrap(),
        );
        idx.tags = vec!["auth".to_string(), "middleware".to_string()];
        idx.mtm_ref_id = Some("mtm-2026-02-20".to_string());
        let idx_json = serde_json::to_string(&idx).unwrap();
        stm_backend
            .store(
                &idx.id,
                &idx_json,
                MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string()),
                None,
            )
            .await
            .unwrap();

        let stm = make_shared(stm_backend);
        let mtm = make_shared(InMemoryBackend::new());
        let cfg = Arc::new(TierConfig::default());

        let loader = TieredMemoryLoader::new(stm, mtm, cfg);
        let output = loader.load_context(&NoopMemory, "test").await.unwrap();

        assert!(
            output.contains("## Memory Index"),
            "missing Index header in:\n{output}"
        );
        assert!(
            output.contains("auth"),
            "missing tag in index section:\n{output}"
        );
        assert!(
            output.contains("## Active Memory (Short-Term)"),
            "raw entries should still appear:\n{output}"
        );
        assert!(
            output.contains("raw-entry"),
            "raw entry should be in active memory:\n{output}"
        );
    }

    #[tokio::test]
    async fn mtm_entries_respect_token_budget() {
        let stm = make_shared(InMemoryBackend::new());
        let mtm_backend = InMemoryBackend::new();

        // Each entry ~100 tokens (400 chars / 4).
        let content_400 = "x".repeat(400);

        mtm_backend
            .store("mtm-day1", &content_400, MemoryCategory::Daily, None)
            .await
            .unwrap();
        mtm_backend
            .store("mtm-day2", &content_400, MemoryCategory::Daily, None)
            .await
            .unwrap();
        mtm_backend
            .store("mtm-day3", &content_400, MemoryCategory::Daily, None)
            .await
            .unwrap();

        let mtm = make_shared(mtm_backend);

        // Set a budget that allows only ~150 tokens (fits 1 entry of ~100, not 2).
        let mut tier_cfg = TierConfig::default();
        tier_cfg.mtm_token_budget = 150;
        let cfg = Arc::new(tier_cfg);

        let loader = TieredMemoryLoader::new(stm, mtm, cfg);
        let output = loader.load_context(&NoopMemory, "test").await.unwrap();

        assert!(
            output.contains("## Medium-Term Memory (Recent Summaries)"),
            "missing MTM header in:\n{output}"
        );

        // Count how many ### sub-sections appear (one per included MTM entry).
        let mtm_subsections = output.matches("### mtm-day").count();
        assert_eq!(
            mtm_subsections, 1,
            "expected 1 MTM entry under budget=150, got {mtm_subsections}\n{output}"
        );
    }

    #[tokio::test]
    async fn full_context_has_all_three_sections_in_order() {
        let stm_backend = InMemoryBackend::new();

        // Raw STM entry.
        stm_backend
            .store("fact", "user prefers dark mode", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Index entry.
        let mut idx = IndexEntry::new("ui", chrono::NaiveDate::from_ymd_opt(2026, 2, 21).unwrap());
        idx.tags = vec!["ui".to_string(), "preferences".to_string()];
        idx.mtm_ref_id = Some("mtm-2026-02-21".to_string());
        let idx_json = serde_json::to_string(&idx).unwrap();
        stm_backend
            .store(
                &idx.id,
                &idx_json,
                MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string()),
                None,
            )
            .await
            .unwrap();

        let stm = make_shared(stm_backend);

        // MTM entry (well within budget).
        let mtm_backend = InMemoryBackend::new();
        mtm_backend
            .store(
                "mtm-2026-02-21",
                "Summary of UI changes",
                MemoryCategory::Daily,
                None,
            )
            .await
            .unwrap();
        let mtm = make_shared(mtm_backend);

        let cfg = Arc::new(TierConfig::default());
        let loader = TieredMemoryLoader::new(stm, mtm, cfg);
        let output = loader.load_context(&NoopMemory, "test").await.unwrap();

        // All three sections present.
        assert!(
            output.contains("## Active Memory (Short-Term)"),
            "missing STM section"
        );
        assert!(output.contains("## Memory Index"), "missing Index section");
        assert!(
            output.contains("## Medium-Term Memory (Recent Summaries)"),
            "missing MTM section"
        );

        // Correct ordering: STM before Known Facts before Index before MTM.
        let stm_pos = output.find("## Active Memory (Short-Term)").unwrap();
        let idx_pos = output.find("## Memory Index").unwrap();
        let mtm_pos = output
            .find("## Medium-Term Memory (Recent Summaries)")
            .unwrap();
        assert!(stm_pos < idx_pos, "STM should come before Index");
        assert!(idx_pos < mtm_pos, "Index should come before MTM");
    }

    #[tokio::test]
    async fn loads_facts_section_from_core_entries() {
        use crate::memory::tiered::facts::{
            FactConfidence, FactEntry, FactStatus, SourceRole, SourceTurnRef, VolatilityClass,
        };

        let stm_backend = InMemoryBackend::new();

        // Create a fact entry and store as JSON with key "fact:personal:user:name"
        let fact = FactEntry {
            fact_id: "f-001".to_string(),
            fact_key: "fact:personal:user:name".to_string(),
            category: "personal".to_string(),
            subject: "user".to_string(),
            attribute: "name".to_string(),
            value: "Alice".to_string(),
            context_narrative: "User introduced themselves".to_string(),
            source_turn: SourceTurnRef {
                conversation_id: "conv-1".to_string(),
                turn_index: 1,
                message_id: None,
                role: SourceRole::User,
                timestamp_unix_ms: 1_700_000_000_000,
            },
            confidence: FactConfidence::High,
            related_facts: vec![],
            extracted_by_tier: "stm".to_string(),
            extracted_at_unix_ms: 1_700_000_001_000,
            source_role: SourceRole::User,
            status: FactStatus::Active,
            revision: 1,
            supersedes_fact_id: None,
            tags: vec!["personal".to_string()],
            volatility_class: VolatilityClass::Stable,
            ttl_days: None,
            expires_at_unix_ms: None,
            last_verified_unix_ms: None,
        };
        let fact_json = serde_json::to_string(&fact).unwrap();
        stm_backend
            .store(
                "fact:personal:user:name",
                &fact_json,
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();

        // Also add a regular raw entry
        stm_backend
            .store("pref-1", "likes tea", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        let stm = make_shared(stm_backend);
        let mtm = make_shared(InMemoryBackend::new());
        let cfg = Arc::new(TierConfig::default());

        let loader = TieredMemoryLoader::new(stm, mtm, cfg);
        let output = loader.load_context(&NoopMemory, "test").await.unwrap();

        assert!(
            output.contains("## Known Facts"),
            "missing Known Facts header in:\n{output}"
        );
        assert!(output.contains("Alice"), "missing fact value in:\n{output}");
        assert!(
            output.contains("[high]"),
            "missing confidence tag in:\n{output}"
        );
        assert!(
            output.contains("user name"),
            "missing subject/attribute in:\n{output}"
        );

        // Known Facts should appear after Active Memory and before Memory Index
        if let Some(stm_pos) = output.find("## Active Memory (Short-Term)") {
            let facts_pos = output.find("## Known Facts").unwrap();
            assert!(
                stm_pos < facts_pos,
                "Active Memory should come before Known Facts"
            );
        }
    }
}
