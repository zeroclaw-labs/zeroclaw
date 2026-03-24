//! Structured memory: confidence-scored fact database.
//!
//! Ported from RustyClaw. Stores extracted facts in SQLite with
//! confidence scoring, deduplication, access tracking, and LRU pruning.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A single fact extracted from conversations.
#[derive(Debug, Clone)]
pub struct Fact {
    pub id: Option<i64>,
    pub content: String,
    pub category: Option<String>,
    pub confidence: f64,
    pub source: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub access_count: i64,
}

impl Fact {
    /// Create a new fact with default confidence (0.5).
    pub fn new(content: impl Into<String>) -> Self {
        let now = now_epoch();
        Self {
            id: None,
            content: content.into(),
            category: None,
            confidence: 0.5,
            source: None,
            created_at: now,
            updated_at: now,
            access_count: 0,
        }
    }

    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn with_confidence(mut self, score: f64) -> Self {
        self.confidence = score.clamp(0.0, 1.0);
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
}

/// Configuration for structured memory.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct StructuredMemoryConfig {
    /// Enable structured fact storage.
    #[serde(default)]
    pub enabled: bool,
    /// Minimum confidence to store a fact (0.0–1.0).
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
    /// Maximum facts before LRU pruning.
    #[serde(default = "default_max_facts")]
    pub max_facts: usize,
}

fn default_min_confidence() -> f64 {
    0.5
}
fn default_max_facts() -> usize {
    10_000
}

impl Default for StructuredMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_confidence: default_min_confidence(),
            max_facts: default_max_facts(),
        }
    }
}

/// SQLite-backed structured memory store.
pub struct StructuredMemory {
    conn: Mutex<Connection>,
    config: StructuredMemoryConfig,
}

impl StructuredMemory {
    /// Open or create the facts database.
    pub fn open(workspace_dir: &Path, config: StructuredMemoryConfig) -> Result<Self> {
        let db_path = workspace_dir.join("memory").join("facts.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open facts DB: {}", db_path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            config,
        })
    }

    /// Open an in-memory database (for testing).
    #[cfg(test)]
    pub fn open_in_memory(config: StructuredMemoryConfig) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            config,
        })
    }

    /// Store a fact. Deduplicates by content (upserts with higher confidence).
    /// Returns the row ID.
    pub fn store_fact(&self, fact: &Fact) -> Result<i64> {
        if fact.confidence < self.config.min_confidence {
            anyhow::bail!(
                "fact confidence {:.2} is below threshold {:.2}",
                fact.confidence,
                self.config.min_confidence
            );
        }

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO facts (content, category, confidence, source, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(content) DO UPDATE SET
                confidence = MAX(facts.confidence, excluded.confidence),
                updated_at = excluded.updated_at",
            rusqlite::params![
                fact.content,
                fact.category,
                fact.confidence,
                fact.source,
                fact.created_at,
                fact.updated_at,
            ],
        )?;
        let id = conn.last_insert_rowid();

        // Prune if over capacity
        drop(conn);
        self.prune_if_needed()?;

        Ok(id)
    }

    /// Search facts by keyword (case-insensitive LIKE on content and category).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Fact>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{query}%");
        let mut stmt = conn.prepare(
            "SELECT id, content, category, confidence, source, created_at, updated_at, access_count
             FROM facts
             WHERE content LIKE ?1 OR category LIKE ?1
             ORDER BY confidence DESC, access_count DESC
             LIMIT ?2",
        )?;
        let facts = stmt
            .query_map(rusqlite::params![pattern, limit as i64], row_to_fact)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(facts)
    }

    /// Get high-confidence facts above a threshold.
    pub fn high_confidence(&self, threshold: f64, limit: usize) -> Result<Vec<Fact>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, content, category, confidence, source, created_at, updated_at, access_count
             FROM facts WHERE confidence >= ?1
             ORDER BY confidence DESC, access_count DESC
             LIMIT ?2",
        )?;
        let facts = stmt
            .query_map(rusqlite::params![threshold, limit as i64], row_to_fact)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(facts)
    }

    /// Record an access to a fact (increments counter).
    pub fn record_access(&self, fact_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE facts SET access_count = access_count + 1, updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now_epoch(), fact_id],
        )?;
        Ok(())
    }

    /// Delete a fact by ID.
    pub fn delete_fact(&self, fact_id: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let affected = conn.execute("DELETE FROM facts WHERE id = ?1", [fact_id])?;
        Ok(affected > 0)
    }

    /// Count total facts.
    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Prune lowest-value facts when over capacity.
    fn prune_if_needed(&self) -> Result<()> {
        let count = self.count()?;
        if count <= self.config.max_facts {
            return Ok(());
        }
        let to_remove = count - self.config.max_facts;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM facts WHERE id IN (
                SELECT id FROM facts
                ORDER BY confidence ASC, access_count ASC, created_at ASC
                LIMIT ?1
            )",
            [to_remove as i64],
        )?;
        tracing::info!(
            "pruned {to_remove} low-value facts (capacity: {})",
            self.config.max_facts
        );
        Ok(())
    }
}

/// Extract facts from text using heuristic confidence scoring.
pub fn extract_facts(text: &str, min_confidence: f64) -> Vec<Fact> {
    text.split(|c: char| matches!(c, '.' | '!' | '?' | '\n'))
        .map(str::trim)
        .filter(|s| s.len() >= 20)
        .filter_map(|sentence| {
            let confidence = calculate_confidence(sentence);
            if confidence >= min_confidence {
                Some(
                    Fact::new(sentence)
                        .with_confidence(confidence)
                        .with_source("conversation"),
                )
            } else {
                None
            }
        })
        .collect()
}

/// Heuristic confidence scoring for a text fragment.
pub fn calculate_confidence(text: &str) -> f64 {
    let mut score: f64 = 0.3;

    // Length bonus (up to 0.2 for 500+ chars)
    let len = text.len().min(500) as f64;
    score += (len / 500.0) * 0.2;

    // Contains numbers → specific data
    if text.chars().any(|c| c.is_ascii_digit()) {
        score += 0.15;
    }

    // Contains proper nouns (capitalized words after first)
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() > 1
        && words[1..]
            .iter()
            .any(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
    {
        score += 0.1;
    }

    // Statement keywords
    let lower = text.to_lowercase();
    if ["is", "are", "has", "was", "were"]
        .iter()
        .any(|kw| lower.split_whitespace().any(|w| w == *kw))
    {
        score += 0.1;
    }

    // Preference keywords
    if ["prefer", "should", "always", "never", "must", "important"]
        .iter()
        .any(|kw| lower.contains(kw))
    {
        score += 0.2;
    }

    score.clamp(0.0, 1.0)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL UNIQUE,
    category TEXT,
    confidence REAL DEFAULT 0.5,
    source TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    access_count INTEGER DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_facts_confidence ON facts(confidence DESC);
CREATE INDEX IF NOT EXISTS idx_facts_category ON facts(category);
";

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn row_to_fact(row: &rusqlite::Row) -> rusqlite::Result<Fact> {
    Ok(Fact {
        id: Some(row.get(0)?),
        content: row.get(1)?,
        category: row.get(2)?,
        confidence: row.get(3)?,
        source: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        access_count: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> StructuredMemoryConfig {
        StructuredMemoryConfig {
            enabled: true,
            min_confidence: 0.3,
            max_facts: 100,
        }
    }

    #[test]
    fn store_and_search() {
        let mem = StructuredMemory::open_in_memory(test_config()).unwrap();
        let fact = Fact::new("Rust is a systems programming language")
            .with_confidence(0.8)
            .with_category("technology");
        mem.store_fact(&fact).unwrap();

        let results = mem.search("Rust", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Rust is a systems programming language");
        assert_eq!(results[0].category.as_deref(), Some("technology"));
    }

    #[test]
    fn dedup_keeps_higher_confidence() {
        let mem = StructuredMemory::open_in_memory(test_config()).unwrap();
        mem.store_fact(&Fact::new("fact A").with_confidence(0.5))
            .unwrap();
        mem.store_fact(&Fact::new("fact A").with_confidence(0.9))
            .unwrap();

        let results = mem.search("fact A", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!((results[0].confidence - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn rejects_below_threshold() {
        let mem = StructuredMemory::open_in_memory(test_config()).unwrap();
        let result = mem.store_fact(&Fact::new("low").with_confidence(0.1));
        assert!(result.is_err());
    }

    #[test]
    fn high_confidence_filter() {
        let mem = StructuredMemory::open_in_memory(test_config()).unwrap();
        mem.store_fact(&Fact::new("low fact").with_confidence(0.4))
            .unwrap();
        mem.store_fact(&Fact::new("high fact").with_confidence(0.9))
            .unwrap();

        let high = mem.high_confidence(0.7, 10).unwrap();
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].content, "high fact");
    }

    #[test]
    fn access_count_increments() {
        let mem = StructuredMemory::open_in_memory(test_config()).unwrap();
        let id = mem
            .store_fact(&Fact::new("tracked fact").with_confidence(0.5))
            .unwrap();
        mem.record_access(id).unwrap();
        mem.record_access(id).unwrap();

        let results = mem.search("tracked", 10).unwrap();
        assert_eq!(results[0].access_count, 2);
    }

    #[test]
    fn delete_fact_works() {
        let mem = StructuredMemory::open_in_memory(test_config()).unwrap();
        let id = mem
            .store_fact(&Fact::new("to delete").with_confidence(0.5))
            .unwrap();
        assert!(mem.delete_fact(id).unwrap());
        assert_eq!(mem.count().unwrap(), 0);
    }

    #[test]
    fn pruning_removes_lowest_value() {
        let config = StructuredMemoryConfig {
            enabled: true,
            min_confidence: 0.3,
            max_facts: 3,
        };
        let mem = StructuredMemory::open_in_memory(config).unwrap();
        mem.store_fact(&Fact::new("low value fact here").with_confidence(0.3))
            .unwrap();
        mem.store_fact(&Fact::new("medium value fact here").with_confidence(0.6))
            .unwrap();
        mem.store_fact(&Fact::new("high value fact here").with_confidence(0.9))
            .unwrap();
        // This should trigger pruning
        mem.store_fact(&Fact::new("newest fact stored here").with_confidence(0.7))
            .unwrap();

        assert_eq!(mem.count().unwrap(), 3);
        // Low-value fact should be pruned
        let results = mem.search("low value", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn extract_facts_from_text() {
        let text = "The user always prefers dark mode for coding. Short. \
                    Python is used for data science tasks at the company.";
        let facts = extract_facts(text, 0.4);
        assert!(!facts.is_empty());
        assert!(facts.iter().any(|f| f.content.contains("dark mode")));
    }

    #[test]
    fn confidence_scoring() {
        let high =
            calculate_confidence("The user always prefers TypeScript for frontend development");
        let low = calculate_confidence("ok sure");
        assert!(high > low);
        assert!(high >= 0.5);
    }
}
