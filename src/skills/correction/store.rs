//! CorrectionStore — CRUD for observations and patterns.

use super::observer::CorrectionObservation;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Classification of correction patterns.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PatternType {
    /// Typo: misspelling (됬다 → 됐다)
    Typo,
    /// Style: ending style, formality (하였다 → 합니다)
    Style,
    /// Terminology: domain-specific term preference (채권자 → 원고)
    Terminology,
    /// Structure: sentence structure preference
    Structure,
}

impl PatternType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Typo => "typo",
            Self::Style => "style",
            Self::Terminology => "terminology",
            Self::Structure => "structure",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "typo" => Some(Self::Typo),
            "style" => Some(Self::Style),
            "terminology" => Some(Self::Terminology),
            "structure" => Some(Self::Structure),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CorrectionPattern {
    pub id: i64,
    pub pattern_type: PatternType,
    pub original_regex: String,
    pub replacement: String,
    pub scope: String,
    pub confidence: f64,
    pub observation_count: i64,
    pub accept_count: i64,
    pub reject_count: i64,
    pub is_active: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub device_id: String,
}

pub struct CorrectionStore {
    conn: Arc<Mutex<Connection>>,
    device_id: String,
}

impl CorrectionStore {
    pub fn new(conn: Arc<Mutex<Connection>>, device_id: String) -> Self {
        Self { conn, device_id }
    }

    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        super::schema::migrate(&conn).context("correction schema migration failed")
    }

    // ── Observations ───────────────────────────────────────────────────

    /// Record a correction observation.
    pub fn record_observation(&self, obs: &CorrectionObservation) -> Result<i64> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO correction_observations
             (uuid, original_text, corrected_text, context_before, context_after,
              document_type, category, source, grammar_valid, session_id, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                obs.uuid,
                obs.original_text,
                obs.corrected_text,
                obs.context_before,
                obs.context_after,
                obs.document_type,
                obs.category,
                obs.source,
                if obs.grammar_valid { 1 } else { 0 },
                obs.session_id,
                self.device_id,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get all observations not yet linked to a pattern.
    pub fn unmined_observations(&self) -> Result<Vec<CorrectionObservation>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, uuid, original_text, corrected_text, context_before, context_after,
                    document_type, category, source, grammar_valid, observed_at, session_id
             FROM correction_observations
             WHERE id NOT IN (SELECT observation_id FROM pattern_observations)
               AND grammar_valid = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(CorrectionObservation {
                id: Some(row.get(0)?),
                uuid: row.get(1)?,
                original_text: row.get(2)?,
                corrected_text: row.get(3)?,
                context_before: row.get(4)?,
                context_after: row.get(5)?,
                document_type: row.get(6)?,
                category: row.get(7)?,
                source: row.get(8)?,
                grammar_valid: {
                    let v: i64 = row.get(9)?;
                    v != 0
                },
                observed_at: row.get(10)?,
                session_id: row.get(11)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to query unmined observations")
    }

    // ── Patterns ───────────────────────────────────────────────────────

    /// Create a new pattern.
    ///
    /// Rejects empty original or replacement — an empty pattern would never
    /// match (or match the entire document with substring search), polluting
    /// the store with noise patterns.
    pub fn create_pattern(
        &self,
        pattern_type: PatternType,
        original_regex: &str,
        replacement: &str,
        scope: &str,
    ) -> Result<i64> {
        if original_regex.is_empty() {
            anyhow::bail!("correction pattern original_regex must not be empty");
        }
        if replacement.is_empty() {
            anyhow::bail!("correction pattern replacement must not be empty");
        }
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO correction_patterns
             (pattern_type, original_regex, replacement, scope, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                pattern_type.as_str(),
                original_regex,
                replacement,
                scope,
                self.device_id
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Find an existing pattern matching this original/replacement pair.
    pub fn find_pattern(
        &self,
        original_regex: &str,
        replacement: &str,
    ) -> Result<Option<CorrectionPattern>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, pattern_type, original_regex, replacement, scope, confidence,
                    observation_count, accept_count, reject_count, is_active,
                    created_at, updated_at, device_id
             FROM correction_patterns
             WHERE original_regex = ?1 AND replacement = ?2",
        )?;
        let mut rows = stmt.query_map(params![original_regex, replacement], |row| {
            let pt_str: String = row.get(1)?;
            Ok(CorrectionPattern {
                id: row.get(0)?,
                pattern_type: PatternType::from_str(&pt_str).unwrap_or(PatternType::Style),
                original_regex: row.get(2)?,
                replacement: row.get(3)?,
                scope: row.get(4)?,
                confidence: row.get(5)?,
                observation_count: row.get(6)?,
                accept_count: row.get(7)?,
                reject_count: row.get(8)?,
                is_active: {
                    let v: i64 = row.get(9)?;
                    v != 0
                },
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
                device_id: row.get(12)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    /// List all active patterns applicable to a scope.
    pub fn active_patterns_for_scope(&self, scope: &str) -> Result<Vec<CorrectionPattern>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, pattern_type, original_regex, replacement, scope, confidence,
                    observation_count, accept_count, reject_count, is_active,
                    created_at, updated_at, device_id
             FROM correction_patterns
             WHERE is_active = 1 AND (scope = 'all' OR scope = ?1)
             ORDER BY confidence DESC",
        )?;
        let rows = stmt.query_map(params![scope], |row| {
            let pt_str: String = row.get(1)?;
            Ok(CorrectionPattern {
                id: row.get(0)?,
                pattern_type: PatternType::from_str(&pt_str).unwrap_or(PatternType::Style),
                original_regex: row.get(2)?,
                replacement: row.get(3)?,
                scope: row.get(4)?,
                confidence: row.get(5)?,
                observation_count: row.get(6)?,
                accept_count: row.get(7)?,
                reject_count: row.get(8)?,
                is_active: {
                    let v: i64 = row.get(9)?;
                    v != 0
                },
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
                device_id: row.get(12)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to list active patterns")
    }

    /// Link an observation to a pattern (for evidence tracking).
    ///
    /// Silently skips the link when the observation has not been persisted
    /// yet (foreign-key constraint) — evidence tracking is best-effort.
    pub fn link_observation(&self, pattern_id: i64, observation_id: i64) -> Result<()> {
        let conn = self.conn.lock();
        // Verify both targets exist before linking; skip if either is absent.
        let obs_exists: bool = conn
            .query_row(
                "SELECT 1 FROM correction_observations WHERE id = ?1",
                params![observation_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        let pattern_exists: bool = conn
            .query_row(
                "SELECT 1 FROM correction_patterns WHERE id = ?1",
                params![pattern_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !obs_exists || !pattern_exists {
            return Ok(());
        }
        conn.execute(
            "INSERT OR IGNORE INTO pattern_observations (pattern_id, observation_id)
             VALUES (?1, ?2)",
            params![pattern_id, observation_id],
        )?;
        Ok(())
    }

    /// Bump confidence and observation count when a new matching observation arrives.
    pub fn bump_confidence(&self, pattern_id: i64, delta: f64) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();
        conn.execute(
            "UPDATE correction_patterns
             SET confidence = MIN(0.95, confidence + ?1),
                 observation_count = observation_count + 1,
                 updated_at = ?2
             WHERE id = ?3",
            params![delta, now, pattern_id],
        )?;
        Ok(())
    }

    /// Increment accept count (user accepted a recommendation).
    pub fn increment_accept(&self, pattern_id: i64) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();
        conn.execute(
            "UPDATE correction_patterns
             SET confidence = MIN(0.95, confidence + 0.05),
                 accept_count = accept_count + 1,
                 updated_at = ?1
             WHERE id = ?2",
            params![now, pattern_id],
        )?;
        Ok(())
    }

    /// Increment reject count (user rejected a recommendation).
    /// Also deactivates the pattern if reject_count > accept_count * 2.
    pub fn increment_reject(&self, pattern_id: i64) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();
        conn.execute(
            "UPDATE correction_patterns
             SET confidence = MAX(0.0, confidence - 0.1),
                 reject_count = reject_count + 1,
                 updated_at = ?1,
                 is_active = CASE
                     WHEN (reject_count + 1) > accept_count * 2 AND reject_count + 1 >= 3 THEN 0
                     ELSE is_active
                 END
             WHERE id = ?2",
            params![now, pattern_id],
        )?;
        Ok(())
    }

    /// Deactivate a pattern (user explicitly removed it).
    pub fn deactivate(&self, pattern_id: i64) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();
        conn.execute(
            "UPDATE correction_patterns SET is_active = 0, updated_at = ?1 WHERE id = ?2",
            params![now, pattern_id],
        )?;
        Ok(())
    }

    /// Apply monthly confidence decay to stale patterns (Dream Cycle).
    pub fn apply_decay(&self, max_age_days: i64, decay: f64) -> Result<usize> {
        let conn = self.conn.lock();
        let cutoff = now_epoch() - (max_age_days * 86400);
        let affected = conn.execute(
            "UPDATE correction_patterns
             SET confidence = MAX(0.0, confidence - ?1)
             WHERE updated_at < ?2 AND is_active = 1",
            params![decay, cutoff],
        )?;
        Ok(affected)
    }

    /// Upsert pattern from sync delta.
    pub fn upsert_from_sync(
        &self,
        pattern_type: &str,
        original_regex: &str,
        replacement: &str,
        scope: &str,
        confidence: f64,
        observation_count: i64,
        accept_count: i64,
        reject_count: i64,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM correction_patterns WHERE original_regex = ?1 AND replacement = ?2",
                params![original_regex, replacement],
                |row| row.get(0),
            )
            .ok();

        if let Some(id) = existing {
            // Take max of counts, max of confidence
            conn.execute(
                "UPDATE correction_patterns
                 SET confidence = MAX(confidence, ?1),
                     observation_count = MAX(observation_count, ?2),
                     accept_count = MAX(accept_count, ?3),
                     reject_count = MAX(reject_count, ?4),
                     updated_at = ?5
                 WHERE id = ?6",
                params![confidence, observation_count, accept_count, reject_count, now, id],
            )?;
        } else {
            conn.execute(
                "INSERT INTO correction_patterns
                 (pattern_type, original_regex, replacement, scope, confidence,
                  observation_count, accept_count, reject_count, device_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    pattern_type,
                    original_regex,
                    replacement,
                    scope,
                    confidence,
                    observation_count,
                    accept_count,
                    reject_count,
                    self.device_id,
                    now
                ],
            )?;
        }
        Ok(())
    }
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> CorrectionStore {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        CorrectionStore::new(Arc::new(Mutex::new(conn)), "test-device".into())
    }

    #[test]
    fn create_and_find_pattern() {
        let store = test_store();
        let id = store
            .create_pattern(PatternType::Style, "하였다", "합니다", "legal_brief")
            .unwrap();
        let p = store.find_pattern("하였다", "합니다").unwrap().unwrap();
        assert_eq!(p.id, id);
        assert_eq!(p.pattern_type, PatternType::Style);
        assert!((p.confidence - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn bump_confidence_increases() {
        let store = test_store();
        let id = store.create_pattern(PatternType::Typo, "됬다", "됐다", "all").unwrap();
        store.bump_confidence(id, 0.25).unwrap();
        let p = store.find_pattern("됬다", "됐다").unwrap().unwrap();
        assert!(p.confidence > 0.5);
        assert_eq!(p.observation_count, 2);
    }

    #[test]
    fn reject_deactivates_after_threshold() {
        let store = test_store();
        let id = store.create_pattern(PatternType::Style, "a", "b", "all").unwrap();
        // accept once, reject 3 times
        store.increment_accept(id).unwrap();
        store.increment_reject(id).unwrap();
        store.increment_reject(id).unwrap();
        store.increment_reject(id).unwrap();

        let p = store.find_pattern("a", "b").unwrap().unwrap();
        // reject 3 > accept 1 * 2, and reject >= 3 — should be deactivated
        assert!(!p.is_active);
    }

    #[test]
    fn active_patterns_filter_by_scope() {
        let store = test_store();
        store.create_pattern(PatternType::Style, "a", "b", "legal_brief").unwrap();
        store.create_pattern(PatternType::Style, "c", "d", "all").unwrap();
        store.create_pattern(PatternType::Style, "e", "f", "email").unwrap();

        let legal = store.active_patterns_for_scope("legal_brief").unwrap();
        // Should include "legal_brief" scope + "all" scope
        assert_eq!(legal.len(), 2);
        let emails = store.active_patterns_for_scope("email").unwrap();
        assert_eq!(emails.len(), 2);
    }

    #[test]
    fn create_pattern_rejects_empty_inputs() {
        let store = test_store();
        assert!(store.create_pattern(PatternType::Style, "", "b", "all").is_err());
        assert!(store.create_pattern(PatternType::Style, "a", "", "all").is_err());
    }
}
