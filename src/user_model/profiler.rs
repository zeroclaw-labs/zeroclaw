//! UserProfiler — session-end observation → conclusion accumulation.
//!
//! At the end of each session, the profiler analyzes conversation patterns
//! and updates persistent conclusions about the user's preferences and style.
//! Only high-confidence conclusions (≥ 0.7) are injected into system prompts.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A persisted conclusion about the user.
#[derive(Debug, Clone)]
pub struct ProfileConclusion {
    pub id: i64,
    pub dimension: String,
    pub conclusion: String,
    pub confidence: f64,
    pub evidence_count: i64,
    pub first_observed: i64,
    pub last_updated: i64,
    pub device_id: String,
}

/// An observation extracted from a session by the LLM.
#[derive(Debug, Clone)]
pub struct UserObservation {
    pub dimension: String,
    pub observation: String,
}

/// Known profiling dimensions.
pub mod dimensions {
    pub const RESPONSE_STYLE: &str = "response_style";
    pub const EXPERTISE: &str = "expertise";
    pub const WORK_PATTERN: &str = "work_pattern";
    pub const DECISION_STYLE: &str = "decision_style";
    pub const TOOL_PREFERENCE: &str = "tool_preference";
    pub const FEEDBACK_PATTERN: &str = "feedback_pattern";
}

/// System prompt for extracting user traits from a conversation session.
pub const TRAIT_EXTRACT_PROMPT: &str = r#"Analyze this conversation session and extract user behavioral observations.

For each observation, classify into one of these dimensions:
- response_style: How the user prefers responses (length, format, tone, language)
- expertise: User's knowledge level in specific domains
- work_pattern: When and how the user works (time patterns, workflow habits)
- decision_style: How the user makes choices (asks for options, wants details, quick decisions)
- tool_preference: Which tools/features the user frequently requests
- feedback_pattern: How the user gives feedback (direct corrections, approval style)

Respond in JSON array format:
[
  {"dimension": "response_style", "observation": "간결한 3줄 이내 응답 선호"},
  {"dimension": "expertise", "observation": "Rust 코딩 중급 (바이브코딩 스타일)"}
]

Only include observations with clear evidence from the conversation.
Skip obvious or trivial observations."#;

/// Minimum confidence threshold for injecting conclusions into system prompt.
const PROMPT_INJECTION_THRESHOLD: f64 = 0.7;

/// Confidence increment when a new observation confirms an existing conclusion.
const CONFIRM_BOOST: f64 = 0.1;

/// Maximum confidence value.
const MAX_CONFIDENCE: f64 = 0.95;

/// Monthly decay applied by Dream Cycle to stale conclusions.
pub const MONTHLY_DECAY: f64 = 0.05;

pub struct UserProfiler {
    conn: Arc<Mutex<Connection>>,
    device_id: String,
}

impl UserProfiler {
    pub fn new(conn: Arc<Mutex<Connection>>, device_id: String) -> Self {
        Self { conn, device_id }
    }

    /// Run schema migration.
    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        super::schema::migrate(&conn).context("user profile schema migration failed")
    }

    /// Find an existing conclusion in the same dimension with similar content.
    pub fn find_existing(&self, dimension: &str) -> Result<Vec<ProfileConclusion>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, dimension, conclusion, confidence, evidence_count,
                    first_observed, last_updated, device_id
             FROM user_profile_conclusions
             WHERE dimension = ?1
             ORDER BY confidence DESC",
        )?;
        let rows = stmt.query_map(params![dimension], |row| {
            Ok(ProfileConclusion {
                id: row.get(0)?,
                dimension: row.get(1)?,
                conclusion: row.get(2)?,
                confidence: row.get(3)?,
                evidence_count: row.get(4)?,
                first_observed: row.get(5)?,
                last_updated: row.get(6)?,
                device_id: row.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to query profile conclusions")
    }

    /// Insert a new conclusion.
    pub fn insert_new(&self, obs: &UserObservation) -> Result<i64> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO user_profile_conclusions (dimension, conclusion, confidence, device_id)
             VALUES (?1, ?2, 0.5, ?3)",
            params![obs.dimension, obs.observation, self.device_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Merge a new observation with an existing conclusion.
    ///
    /// If the observation confirms the existing conclusion, boost confidence.
    /// If it contradicts, create a new conclusion (the LLM decides which wins
    /// during the next Dream Cycle consolidation).
    pub fn merge_or_update(
        &self,
        existing: &ProfileConclusion,
        obs: &UserObservation,
        is_confirming: bool,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();

        if is_confirming {
            // Increment relative to the row's current confidence so repeated
            // calls with the same stale snapshot still accumulate.
            conn.execute(
                "UPDATE user_profile_conclusions
                 SET confidence = MIN(?1, confidence + ?2),
                     evidence_count = evidence_count + 1,
                     last_updated = ?3
                 WHERE id = ?4",
                params![MAX_CONFIDENCE, CONFIRM_BOOST, now, existing.id],
            )?;
        } else {
            // Contradicting — insert as separate conclusion, let Dream Cycle resolve
            conn.execute(
                "INSERT INTO user_profile_conclusions (dimension, conclusion, confidence, device_id)
                 VALUES (?1, ?2, 0.5, ?3)",
                params![obs.dimension, obs.observation, self.device_id],
            )?;
        }
        Ok(())
    }

    /// Build the prompt injection string from high-confidence conclusions.
    pub fn build_prompt_injection(&self) -> Result<String> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT dimension, conclusion, confidence
             FROM user_profile_conclusions
             WHERE confidence >= ?1
             ORDER BY dimension, confidence DESC",
        )?;
        let rows: Vec<(String, String, f64)> = stmt
            .query_map(params![PROMPT_INJECTION_THRESHOLD], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        if rows.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::with_capacity(rows.len() * 100);
        out.push_str("이 사용자에 대해 알려진 것:\n");

        for (dim, conclusion, confidence) in &rows {
            let dim_label = match dim.as_str() {
                "response_style" => "응답 스타일",
                "expertise" => "전문성",
                "work_pattern" => "작업 패턴",
                "decision_style" => "결정 스타일",
                "tool_preference" => "도구 선호",
                "feedback_pattern" => "피드백 패턴",
                _ => dim.as_str(),
            };
            out.push_str(&format!(
                "  - {}: {} (확신도: {:.0}%)\n",
                dim_label,
                conclusion,
                confidence * 100.0
            ));
        }

        Ok(out)
    }

    /// Apply monthly confidence decay to stale conclusions.
    /// Called by Dream Cycle.
    pub fn apply_decay(&self, max_age_days: i64) -> Result<usize> {
        let conn = self.conn.lock();
        let cutoff = now_epoch() - (max_age_days * 86400);
        // `<=` so that age 0 (i.e. "decay everything regardless of freshness")
        // still matches rows that were updated this very second.
        let affected = conn.execute(
            "UPDATE user_profile_conclusions
             SET confidence = MAX(0.0, confidence - ?1)
             WHERE last_updated <= ?2 AND confidence > 0.0",
            params![MONTHLY_DECAY, cutoff],
        )?;
        // Archive conclusions that decayed below threshold
        conn.execute(
            "DELETE FROM user_profile_conclusions WHERE confidence <= 0.1",
            [],
        )?;
        Ok(affected)
    }

    /// Upsert from sync delta.
    pub fn upsert_from_sync(
        &self,
        dimension: &str,
        conclusion: &str,
        confidence: f64,
        evidence_count: i64,
        device_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();
        // Try to find matching conclusion in same dimension
        let existing_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM user_profile_conclusions
                 WHERE dimension = ?1 AND conclusion = ?2",
                params![dimension, conclusion],
                |row| row.get(0),
            )
            .ok();

        if let Some(id) = existing_id {
            // Update if incoming has higher confidence
            conn.execute(
                "UPDATE user_profile_conclusions
                 SET confidence = MAX(confidence, ?1),
                     evidence_count = MAX(evidence_count, ?2),
                     last_updated = ?3
                 WHERE id = ?4",
                params![confidence, evidence_count, now, id],
            )?;
        } else {
            conn.execute(
                "INSERT INTO user_profile_conclusions
                 (dimension, conclusion, confidence, evidence_count, device_id, last_updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![dimension, conclusion, confidence, evidence_count, device_id, now],
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

    fn test_profiler() -> UserProfiler {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        UserProfiler::new(Arc::new(Mutex::new(conn)), "test-device".into())
    }

    #[test]
    fn insert_and_query_conclusion() {
        let p = test_profiler();
        let obs = UserObservation {
            dimension: "response_style".into(),
            observation: "간결한 3줄 이내 응답 선호".into(),
        };
        p.insert_new(&obs).unwrap();
        let results = p.find_existing("response_style").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].conclusion, "간결한 3줄 이내 응답 선호");
        assert!((results[0].confidence - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn confirm_boosts_confidence() {
        let p = test_profiler();
        let obs = UserObservation {
            dimension: "expertise".into(),
            observation: "Rust 중급".into(),
        };
        p.insert_new(&obs).unwrap();
        let existing = p.find_existing("expertise").unwrap();
        p.merge_or_update(&existing[0], &obs, true).unwrap();
        let updated = p.find_existing("expertise").unwrap();
        assert!((updated[0].confidence - 0.6).abs() < f64::EPSILON);
        assert_eq!(updated[0].evidence_count, 2);
    }

    #[test]
    fn prompt_injection_filters_by_threshold() {
        let p = test_profiler();
        // Low confidence — should not appear
        let low = UserObservation {
            dimension: "tool_preference".into(),
            observation: "Maybe likes web search".into(),
        };
        p.insert_new(&low).unwrap();
        assert!(p.build_prompt_injection().unwrap().is_empty());

        // Boost above threshold
        let existing = p.find_existing("tool_preference").unwrap();
        for _ in 0..3 {
            p.merge_or_update(&existing[0], &low, true).unwrap();
        }
        let injection = p.build_prompt_injection().unwrap();
        assert!(injection.contains("Maybe likes web search"));
    }

    #[test]
    fn decay_reduces_confidence() {
        let p = test_profiler();
        let obs = UserObservation {
            dimension: "response_style".into(),
            observation: "Old preference".into(),
        };
        p.insert_new(&obs).unwrap();
        // Boost to 0.8
        let existing = p.find_existing("response_style").unwrap();
        for _ in 0..3 {
            p.merge_or_update(&existing[0], &obs, true).unwrap();
        }

        // Decay with 0-day cutoff (everything is "stale")
        let affected = p.apply_decay(0).unwrap();
        assert!(affected > 0);
    }
}
