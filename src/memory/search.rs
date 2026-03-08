use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::traits::{MemoryCategory, MemoryEntry};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilter {
    pub category: Option<MemoryCategory>,
    pub session_id: Option<String>,
    pub min_score: Option<f64>,
    pub after: Option<String>,
    pub before: Option<String>,
}

impl SearchFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_category(mut self, category: MemoryCategory) -> Self {
        self.category = Some(category);
        self
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_min_score(mut self, score: f64) -> Self {
        self.min_score = Some(score);
        self
    }

    pub fn after(mut self, timestamp: impl Into<String>) -> Self {
        self.after = Some(timestamp.into());
        self
    }

    pub fn before(mut self, timestamp: impl Into<String>) -> Self {
        self.before = Some(timestamp.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub entry: MemoryEntry,
    pub relevance_score: f64,
    pub match_type: MatchType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MatchType {
    Keyword,
    Semantic,
    Hybrid,
    Exact,
}

impl std::fmt::Display for MatchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keyword => write!(f, "keyword"),
            Self::Semantic => write!(f, "semantic"),
            Self::Hybrid => write!(f, "hybrid"),
            Self::Exact => write!(f, "exact"),
        }
    }
}

#[async_trait]
pub trait SearchableMemory: Send + Sync {
    async fn search(
        &self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>>;

    async fn semantic_search(
        &self,
        query: &str,
        limit: usize,
        min_score: f64,
    ) -> anyhow::Result<Vec<SearchResult>>;

    async fn keyword_search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_filter_builder_pattern() {
        let filter = SearchFilter::new()
            .with_category(MemoryCategory::Core)
            .with_session("session-1")
            .with_min_score(0.5)
            .after("2026-01-01")
            .before("2026-12-31");

        assert_eq!(filter.category, Some(MemoryCategory::Core));
        assert_eq!(filter.session_id.as_deref(), Some("session-1"));
        assert_eq!(filter.min_score, Some(0.5));
        assert_eq!(filter.after.as_deref(), Some("2026-01-01"));
        assert_eq!(filter.before.as_deref(), Some("2026-12-31"));
    }

    #[test]
    fn search_filter_default_is_empty() {
        let filter = SearchFilter::default();
        assert!(filter.category.is_none());
        assert!(filter.session_id.is_none());
        assert!(filter.min_score.is_none());
        assert!(filter.after.is_none());
        assert!(filter.before.is_none());
    }

    #[test]
    fn match_type_display() {
        assert_eq!(MatchType::Keyword.to_string(), "keyword");
        assert_eq!(MatchType::Semantic.to_string(), "semantic");
        assert_eq!(MatchType::Hybrid.to_string(), "hybrid");
        assert_eq!(MatchType::Exact.to_string(), "exact");
    }

    #[test]
    fn search_result_roundtrip() {
        let result = SearchResult {
            entry: MemoryEntry {
                id: "id-1".into(),
                key: "test".into(),
                content: "hello world".into(),
                category: MemoryCategory::Core,
                timestamp: "2026-03-07T00:00:00Z".into(),
                session_id: None,
                score: Some(0.95),
            },
            relevance_score: 0.95,
            match_type: MatchType::Hybrid,
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: SearchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.relevance_score, 0.95);
        assert_eq!(parsed.match_type, MatchType::Hybrid);
        assert_eq!(parsed.entry.key, "test");
    }

    #[test]
    fn search_filter_serde_roundtrip() {
        let filter = SearchFilter::new()
            .with_category(MemoryCategory::Daily)
            .with_min_score(0.7);

        let json = serde_json::to_string(&filter).unwrap();
        let parsed: SearchFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.category, Some(MemoryCategory::Daily));
        assert_eq!(parsed.min_score, Some(0.7));
    }
}
