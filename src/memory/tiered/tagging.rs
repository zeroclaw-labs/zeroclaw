//! Semantic tag extraction from memory entries.
//!
//! [`BasicTagExtractor`] uses keyword-frequency analysis (no LLM dependency)
//! to produce 2–6 lowercase kebab-case tags for a batch of [`MemoryEntry`] values.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use async_trait::async_trait;

use crate::memory::traits::MemoryEntry;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Extracts semantic tags from a batch of memory entries.
#[async_trait]
pub trait TagExtractor: Send + Sync {
    /// Returns `(tags, confidence)`.
    ///
    /// * **tags** — 2–6 lowercase kebab-case strings.
    /// * **confidence** — a value in `(0.0, 1.0]`.
    async fn extract_tags(&self, entries: &[MemoryEntry]) -> Result<(Vec<String>, f32)>;
}

// ---------------------------------------------------------------------------
// BasicTagExtractor
// ---------------------------------------------------------------------------

/// Simple frequency-based tag extractor. No LLM dependency.
pub struct BasicTagExtractor {
    boost_words: Vec<String>,
    stop_words: HashSet<String>,
}

impl BasicTagExtractor {
    /// Create a new extractor with sensible defaults for stop/boost words.
    pub fn new() -> Self {
        let stop_words: HashSet<String> = [
            "the", "a", "an", "is", "in", "on", "at", "to", "and", "or", "of", "for", "with",
            "it", "was", "i", "you", "we", "that", "this", "have", "be",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();

        let boost_words: Vec<String> = [
            "auth",
            "database",
            "api",
            "cache",
            "error",
            "memory",
            "config",
            "test",
            "deploy",
            "migration",
            "session",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();

        Self {
            boost_words,
            stop_words,
        }
    }

    /// Normalize a raw token: trim non-alphabetic characters from both ends,
    /// lowercase, and return `None` when the result is too short or is a stop
    /// word.
    fn normalize(&self, raw: &str) -> Option<String> {
        let trimmed: String = raw
            .trim_matches(|c: char| !c.is_alphabetic())
            .to_lowercase();
        if trimmed.len() < 3 {
            return None;
        }
        if self.stop_words.contains(&trimmed) {
            return None;
        }
        Some(trimmed)
    }
}

impl Default for BasicTagExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TagExtractor for BasicTagExtractor {
    async fn extract_tags(&self, entries: &[MemoryEntry]) -> Result<(Vec<String>, f32)> {
        // 1. Split all entry content by whitespace.
        let mut freq: HashMap<String, usize> = HashMap::new();
        for entry in entries {
            for token in entry.content.split_whitespace() {
                if let Some(word) = self.normalize(token) {
                    *freq.entry(word).or_insert(0) += 1;
                }
            }
        }

        // 4. Boost domain words by +2 frequency.
        for boost in &self.boost_words {
            if let Some(count) = freq.get_mut(boost) {
                *count += 2;
            }
        }

        // 5. Sort by frequency desc, take top 6.
        let mut pairs: Vec<(String, usize)> = freq.into_iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let tags: Vec<String> = pairs.into_iter().take(6).map(|(word, _)| word).collect();

        // 6. Confidence: 0.7 if >= 2 tags, 0.3 otherwise.
        let confidence = if tags.len() >= 2 { 0.7 } else { 0.3 };

        Ok((tags, confidence))
    }
}

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
    async fn basic_extractor_extracts_tags_from_content() {
        let extractor = BasicTagExtractor::new();
        let entries = vec![make_test_entry(
            "auth middleware jwt token login session",
        )];
        let (tags, confidence) = extractor.extract_tags(&entries).await.unwrap();
        assert!(!tags.is_empty());
        assert!(tags.len() <= 6);
        assert!(
            confidence > 0.0 && confidence <= 1.0,
            "confidence {confidence} out of range"
        );
        for tag in &tags {
            assert!(
                tag.chars().all(|c| c.is_lowercase() || c == '-'),
                "tag '{}' not kebab-case",
                tag
            );
        }
    }

    #[tokio::test]
    async fn basic_extractor_returns_at_least_two_tags_for_rich_content() {
        let extractor = BasicTagExtractor::new();
        let entries = vec![
            make_test_entry("database migration postgres schema"),
            make_test_entry("postgres connection pool timeout"),
        ];
        let (tags, _) = extractor.extract_tags(&entries).await.unwrap();
        assert!(tags.len() >= 2, "got {:?}", tags);
    }

    #[tokio::test]
    async fn basic_extractor_boosts_domain_words() {
        let extractor = BasicTagExtractor::new();
        // "auth" appears once but is boosted (+2 -> effective 3);
        // "randomword" appears 3 times (effective 3) but sorts after "auth" alphabetically.
        let entries = vec![make_test_entry(
            "auth randomword randomword randomword",
        )];
        let (tags, _) = extractor.extract_tags(&entries).await.unwrap();
        assert!(
            tags.contains(&"auth".to_string()),
            "tags {:?} should contain 'auth'",
            tags
        );
    }

    #[tokio::test]
    async fn basic_extractor_confidence_low_for_sparse_input() {
        let extractor = BasicTagExtractor::new();
        // Only one meaningful word after normalization (len >= 3, not a stop word).
        let entries = vec![make_test_entry("hi")];
        let (tags, confidence) = extractor.extract_tags(&entries).await.unwrap();
        assert!(tags.len() < 2);
        assert!(
            (confidence - 0.3).abs() < f32::EPSILON,
            "expected 0.3, got {confidence}"
        );
    }

    #[tokio::test]
    async fn basic_extractor_skips_stop_words() {
        let extractor = BasicTagExtractor::new();
        let entries = vec![make_test_entry(
            "the database is for the api and the cache",
        )];
        let (tags, _) = extractor.extract_tags(&entries).await.unwrap();
        for tag in &tags {
            assert!(
                !["the", "is", "for", "and"].contains(&tag.as_str()),
                "stop word '{}' should not appear in tags",
                tag
            );
        }
    }

    #[tokio::test]
    async fn basic_extractor_caps_at_six_tags() {
        let extractor = BasicTagExtractor::new();
        let entries = vec![make_test_entry(
            "alpha bravo charlie delta echo foxtrot golf hotel india juliet",
        )];
        let (tags, _) = extractor.extract_tags(&entries).await.unwrap();
        assert!(tags.len() <= 6, "got {} tags: {:?}", tags.len(), tags);
    }

    #[tokio::test]
    async fn basic_extractor_empty_input_returns_zero_tags() {
        let extractor = BasicTagExtractor::new();
        let entries: Vec<MemoryEntry> = vec![];
        let (tags, confidence) = extractor.extract_tags(&entries).await.unwrap();
        assert!(tags.is_empty());
        assert!(
            (confidence - 0.3).abs() < f32::EPSILON,
            "expected 0.3, got {confidence}"
        );
    }
}
