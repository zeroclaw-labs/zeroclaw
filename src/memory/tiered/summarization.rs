//! Tier summarization: compress STM→MTM and MTM→LTM.
//!
//! [`TierSummarizer`] is the trait consumed by the compression pipeline.
//! [`MockSummarizer`] is a deterministic implementation for tests.
//! [`LlmTierSummarizer`] is a skeleton that delegates to the provider layer
//! — see the TODO comments inside for wiring instructions.

use anyhow::Result;
use async_trait::async_trait;
use chrono::NaiveDate;

use crate::memory::traits::MemoryEntry;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Produces compressed text from batches of memory entries.
///
/// Two operations map to the two tier transitions:
///
/// * **STM → MTM** — `summarize_stm_day` distils a single day's raw entries
///   into a compact paragraph suitable for the medium-term tier.
/// * **MTM → LTM** — `compress_mtm_batch` merges several day-summaries into a
///   single long-term record.
#[async_trait]
pub trait TierSummarizer: Send + Sync {
    /// Summarize a day's STM entries into a compact MTM record.
    async fn summarize_stm_day(
        &self,
        day: NaiveDate,
        stm_entries: &[MemoryEntry],
        tags: &[String],
    ) -> Result<String>;

    /// Compress a batch of MTM day-summaries into a single LTM entry.
    async fn compress_mtm_batch(&self, mtm_entries: &[MemoryEntry]) -> Result<String>;
}

// ---------------------------------------------------------------------------
// MockSummarizer
// ---------------------------------------------------------------------------

/// Deterministic summarizer for unit / integration tests.
///
/// Returns predictable strings that embed entry counts and dates so callers
/// can assert on structure without needing an LLM.
pub struct MockSummarizer;

#[async_trait]
impl TierSummarizer for MockSummarizer {
    async fn summarize_stm_day(
        &self,
        day: NaiveDate,
        entries: &[MemoryEntry],
        _tags: &[String],
    ) -> Result<String> {
        Ok(format!("Summary of {} entries for {}", entries.len(), day))
    }

    async fn compress_mtm_batch(&self, entries: &[MemoryEntry]) -> Result<String> {
        Ok(format!("Compressed {} summaries", entries.len()))
    }
}

// ---------------------------------------------------------------------------
// LlmTierSummarizer (skeleton)
// ---------------------------------------------------------------------------

// TODO: Wire up to the real provider once a concrete `Provider` instance is
// available at construction time. The skeleton below shows the intended shape;
// uncomment and fill in the prompt templates when integrating.
//
// ```rust
// use std::sync::Arc;
// use crate::providers::traits::Provider;
//
// pub struct LlmTierSummarizer {
//     provider: Arc<dyn Provider>,
//     model: String,
// }
//
// impl LlmTierSummarizer {
//     pub fn new(provider: Arc<dyn Provider>, model: String) -> Self {
//         Self { provider, model }
//     }
//
//     fn build_stm_prompt(day: NaiveDate, entries: &[MemoryEntry], tags: &[String]) -> String {
//         let mut prompt = format!(
//             "Summarize the following {} memory entries from {} (tags: {}):\n\n",
//             entries.len(),
//             day,
//             tags.join(", "),
//         );
//         for entry in entries {
//             prompt.push_str(&format!("- [{}] {}\n", entry.key, entry.content));
//         }
//         prompt.push_str(
//             "\nProduce a single concise paragraph capturing the key facts and decisions.",
//         );
//         prompt
//     }
//
//     fn build_mtm_prompt(entries: &[MemoryEntry]) -> String {
//         let mut prompt = format!(
//             "Compress the following {} day-summaries into a single long-term record:\n\n",
//             entries.len(),
//         );
//         for entry in entries {
//             prompt.push_str(&format!("- [{}] {}\n", entry.key, entry.content));
//         }
//         prompt.push_str(
//             "\nProduce a concise paragraph preserving only the most important information.",
//         );
//         prompt
//     }
// }
//
// #[async_trait]
// impl TierSummarizer for LlmTierSummarizer {
//     async fn summarize_stm_day(
//         &self,
//         day: NaiveDate,
//         stm_entries: &[MemoryEntry],
//         tags: &[String],
//     ) -> Result<String> {
//         let prompt = Self::build_stm_prompt(day, stm_entries, tags);
//         let response = self
//             .provider
//             .chat_with_system(
//                 Some("You are a memory compression assistant. Be concise and factual."),
//                 &prompt,
//                 &self.model,
//                 0.3,
//             )
//             .await?;
//         Ok(response)
//     }
//
//     async fn compress_mtm_batch(&self, mtm_entries: &[MemoryEntry]) -> Result<String> {
//         let prompt = Self::build_mtm_prompt(mtm_entries);
//         let response = self
//             .provider
//             .chat_with_system(
//                 Some("You are a memory compression assistant. Be concise and factual."),
//                 &prompt,
//                 &self.model,
//                 0.3,
//             )
//             .await?;
//         Ok(response)
//     }
// }
// ```

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::traits::{MemoryCategory, MemoryEntry};

    /// Helper: build a [`MemoryEntry`] from a content string.
    fn make_test_entry(content: &str) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            key: "test-key".to_string(),
            content: content.to_string(),
            category: MemoryCategory::Core,
            timestamp: "2026-02-22T00:00:00Z".to_string(),
            session_id: None,
            score: None,
        }
    }

    #[tokio::test]
    async fn mock_summarizer_stm_day_returns_non_empty() {
        let s = MockSummarizer;
        let result = s
            .summarize_stm_day(
                NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
                &[], // empty entries is fine for mock
                &["auth".to_string()],
            )
            .await
            .unwrap();
        assert!(!result.is_empty());
        assert!(result.contains("0 entries"));
    }

    #[tokio::test]
    async fn mock_summarizer_compress_returns_non_empty() {
        let s = MockSummarizer;
        let result = s.compress_mtm_batch(&[]).await.unwrap();
        assert!(!result.is_empty());
        assert!(result.contains("0 summaries"));
    }

    #[tokio::test]
    async fn mock_summarizer_includes_entry_count() {
        let s = MockSummarizer;
        let entries = vec![make_test_entry("entry 1"), make_test_entry("entry 2")];
        let result = s
            .summarize_stm_day(NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(), &entries, &[])
            .await
            .unwrap();
        assert!(result.contains("2 entries"), "result: {}", result);
    }

    #[tokio::test]
    async fn mock_summarizer_stm_day_includes_date() {
        let s = MockSummarizer;
        let result = s
            .summarize_stm_day(
                NaiveDate::from_ymd_opt(2026, 3, 10).unwrap(),
                &[make_test_entry("something")],
                &[],
            )
            .await
            .unwrap();
        assert!(
            result.contains("2026-03-10"),
            "result should contain the date: {}",
            result
        );
    }

    #[tokio::test]
    async fn mock_summarizer_compress_counts_multiple_entries() {
        let s = MockSummarizer;
        let entries = vec![
            make_test_entry("day 1 summary"),
            make_test_entry("day 2 summary"),
            make_test_entry("day 3 summary"),
        ];
        let result = s.compress_mtm_batch(&entries).await.unwrap();
        assert!(result.contains("3 summaries"), "result: {}", result);
    }
}
