//! Background worker that processes the STM extraction queue.

use std::sync::Arc;
use chrono::Utc;
use tokio::sync::mpsc;
use crate::memory::tiered::extractor::FactExtractor;
use crate::memory::tiered::facts::*;
use crate::memory::tiered::prompts::build_stm_extraction_prompt;
use crate::memory::tiered::ExtractionRequest;
use crate::memory::tiered::SharedMemory;
use crate::memory::traits::MemoryCategory;

/// Run the STM extraction worker loop until the channel is closed.
pub async fn run_extraction_worker(
    mut rx: mpsc::Receiver<ExtractionRequest>,
    stm: SharedMemory,
    extractor: Arc<dyn FactExtractor>,
    conversation_id: String,
) {
    while let Some(req) = rx.recv().await {
        if let Err(e) = process_extraction(&req, &stm, &extractor, &conversation_id).await {
            // Log but don't crash — extraction is best-effort
            eprintln!("STM fact extraction failed (non-fatal): {:#}", e);
        }
    }
}

async fn process_extraction(
    req: &ExtractionRequest,
    stm: &SharedMemory,
    extractor: &Arc<dyn FactExtractor>,
    conversation_id: &str,
) -> anyhow::Result<()> {
    let prompt = build_stm_extraction_prompt(&req.content, "", &[]);
    let drafts = extractor.extract(&prompt).await?;
    let now_ms = Utc::now().timestamp_millis();
    let source_role = if req.role == "agent" { SourceRole::Agent } else { SourceRole::User };
    let stm_guard = stm.lock().await;

    for draft in drafts {
        let fact_key = build_fact_key(&draft.category, &draft.subject, &draft.attribute);
        let confidence = match draft.confidence.to_lowercase().as_str() {
            "high" => FactConfidence::High,
            "low" => FactConfidence::Low,
            _ => FactConfidence::Medium,
        };
        let volatility = match draft.volatility_class.to_lowercase().as_str() {
            "stable" => VolatilityClass::Stable,
            "volatile" => VolatilityClass::Volatile,
            _ => VolatilityClass::SemiStable,
        };

        let mut entry = FactEntry {
            fact_id: uuid::Uuid::new_v4().to_string(),
            fact_key: fact_key.clone(),
            category: draft.category,
            subject: draft.subject,
            attribute: draft.attribute,
            value: draft.value,
            context_narrative: draft.context_narrative,
            source_turn: SourceTurnRef {
                conversation_id: conversation_id.to_string(),
                turn_index: 0,
                message_id: Some(req.key.clone()),
                role: source_role.clone(),
                timestamp_unix_ms: req.timestamp_unix_ms,
            },
            confidence,
            related_facts: draft.related_facts,
            extracted_by_tier: "stm".to_string(),
            extracted_at_unix_ms: now_ms,
            source_role: source_role.clone(),
            status: FactStatus::Active,
            revision: 1,
            supersedes_fact_id: None,
            tags: vec![],
            volatility_class: volatility,
            ttl_days: None,
            expires_at_unix_ms: None,
            last_verified_unix_ms: Some(now_ms),
        };

        entry.apply_poisoning_guard();
        let content = serde_json::to_string(&entry)?;
        stm_guard.store(&fact_key, &content, MemoryCategory::Core, None).await?;
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::tiered::extractor::{FactEntryDraft, MockFactExtractor};
    use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // ── InMemoryBackend (test double) ────────────────────────────────────

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

    fn make_shared(backend: InMemoryBackend) -> SharedMemory {
        Arc::new(Mutex::new(Box::new(backend)))
    }

    fn sample_draft() -> FactEntryDraft {
        FactEntryDraft {
            category: "personal".to_string(),
            subject: "user".to_string(),
            attribute: "favorite_color".to_string(),
            value: "blue".to_string(),
            context_narrative: "User said they love blue.".to_string(),
            confidence: "high".to_string(),
            related_facts: vec![],
            volatility_class: "stable".to_string(),
        }
    }

    #[tokio::test]
    async fn worker_processes_extraction_request() {
        let stm = make_shared(InMemoryBackend::new());
        let extractor: Arc<dyn FactExtractor> =
            Arc::new(MockFactExtractor::new(vec![sample_draft()]));

        let req = ExtractionRequest {
            content: "My favorite color is blue".to_string(),
            role: "user".to_string(),
            key: "msg:user:001".to_string(),
            session_id: None,
            timestamp_unix_ms: 1_700_000_000_000,
        };

        let (tx, rx) = mpsc::channel(16);
        tx.send(req).await.unwrap();
        drop(tx); // Close channel so worker loop exits after processing

        run_extraction_worker(rx, Arc::clone(&stm), extractor, "conv-test".to_string()).await;

        // Verify the fact was stored in STM
        let guard = stm.lock().await;
        let fact_key = "fact:personal:user:favorite-color";
        let entry = guard.get(fact_key).await.unwrap();
        assert!(
            entry.is_some(),
            "expected fact to be stored under key `{fact_key}`"
        );

        let entry = entry.unwrap();
        assert_eq!(entry.category, MemoryCategory::Core);
        assert!(entry.key.starts_with("fact:"));

        // Verify the stored content is a valid FactEntry
        let fact: FactEntry = serde_json::from_str(&entry.content).unwrap();
        assert_eq!(fact.value, "blue");
        assert_eq!(fact.subject, "user");
        assert_eq!(fact.extracted_by_tier, "stm");
    }

    #[tokio::test]
    async fn worker_applies_poisoning_guard_for_agent_messages() {
        let mut draft = sample_draft();
        draft.confidence = "high".to_string(); // Agent with High → should be demoted

        let stm = make_shared(InMemoryBackend::new());
        let extractor: Arc<dyn FactExtractor> =
            Arc::new(MockFactExtractor::new(vec![draft]));

        let req = ExtractionRequest {
            content: "I think blue is a great color".to_string(),
            role: "agent".to_string(),
            key: "msg:agent:002".to_string(),
            session_id: None,
            timestamp_unix_ms: 1_700_000_000_000,
        };

        let (tx, rx) = mpsc::channel(16);
        tx.send(req).await.unwrap();
        drop(tx);

        run_extraction_worker(rx, Arc::clone(&stm), extractor, "conv-test".to_string()).await;

        // Verify the fact was stored and confidence was demoted
        let guard = stm.lock().await;
        let fact_key = "fact:personal:user:favorite-color";
        let entry = guard.get(fact_key).await.unwrap().expect("fact should exist");
        let fact: FactEntry = serde_json::from_str(&entry.content).unwrap();

        assert_eq!(
            fact.confidence,
            FactConfidence::Medium,
            "agent-sourced High confidence should be demoted to Medium by poisoning guard"
        );
        assert_eq!(fact.source_role, SourceRole::Agent);
    }
}
