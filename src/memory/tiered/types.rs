//! Core types for the three-tier memory system.
//!
//! Every other tiered-memory module imports from here.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

// ── MemoryAgentConfig ────────────────────────────────────────────────────────

/// Configuration for a single memory agent (one per tier).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAgentConfig {
    pub model: String,
    pub max_tokens: usize,
    pub temperature: f32,
    pub timeout_secs: u64,
}

impl Default for MemoryAgentConfig {
    fn default() -> Self {
        Self {
            model: "google/gemini-3-flash-preview".to_string(),
            max_tokens: 4096,
            temperature: 0.1,
            timeout_secs: 30,
        }
    }
}

/// Configuration for all memory agents across the three tiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAgentsConfig {
    pub openrouter_api_key: Option<String>,
    pub openrouter_base_url: String,
    pub stm: MemoryAgentConfig,
    pub mtm: MemoryAgentConfig,
    pub ltm: MemoryAgentConfig,
}

impl Default for MemoryAgentsConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: None,
            openrouter_base_url: "https://openrouter.ai/api/v1".to_string(),
            stm: MemoryAgentConfig {
                max_tokens: 400,
                timeout_secs: 10,
                ..MemoryAgentConfig::default()
            },
            mtm: MemoryAgentConfig {
                max_tokens: 8192,
                timeout_secs: 60,
                ..MemoryAgentConfig::default()
            },
            ltm: MemoryAgentConfig {
                max_tokens: 8192,
                timeout_secs: 60,
                ..MemoryAgentConfig::default()
            },
        }
    }
}

// ── TierConfig ────────────────────────────────────────────────────────────────

/// Configuration for the three-tier memory system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    /// Hour at which STM is rolled over (0-23), default 23.
    pub stm_eod_hour: u8,
    /// Minute at which STM is rolled over (0-59), default 59.
    pub stm_eod_minute: u8,
    /// Sliding window for STM entries, default 24 h.
    #[serde(with = "duration_secs")]
    pub stm_window: Duration,

    /// Human-readable location used to resolve a timezone, e.g. "Cebu, Philippines".
    pub user_location: Option<String>,
    /// IANA timezone identifier, e.g. "Asia/Manila", default "UTC".
    pub resolved_timezone: String,
    /// How the timezone was determined: "user_location" | "system_fallback" | "manual".
    pub timezone_provenance: String,

    /// Maximum token budget for MTM layer, default 2000.
    pub mtm_token_budget: usize,
    /// Hysteresis band around the token budget, default 200.
    pub mtm_budget_hysteresis: usize,
    /// Maximum number of days to batch for MTM compression, default 7.
    pub mtm_max_batch_days: usize,

    /// Maximum compression retry attempts, default 3.
    pub compression_retry_max: u32,
    /// Base backoff in seconds between compression retries, default 30.
    pub compression_retry_base_backoff_secs: u64,

    /// Top-K results fetched from each tier during recall, default 10.
    pub recall_top_k_per_tier: usize,
    /// Final top-K merged results returned to the caller, default 5.
    pub recall_final_top_k: usize,

    /// Weight assigned to STM results when ranking, default 0.45.
    pub weight_stm: f32,
    /// Weight assigned to MTM results when ranking, default 0.35.
    pub weight_mtm: f32,
    /// Weight assigned to LTM results when ranking, default 0.20.
    pub weight_ltm: f32,

    /// Minimum relevance score a result must exceed to be returned, default 0.4.
    pub min_relevance_threshold: f32,

    /// Per-tier memory agent configuration.
    pub memory_agents: MemoryAgentsConfig,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            stm_eod_hour: 23,
            stm_eod_minute: 59,
            stm_window: Duration::from_secs(24 * 60 * 60),
            user_location: None,
            resolved_timezone: "UTC".to_string(),
            timezone_provenance: "system_fallback".to_string(),
            mtm_token_budget: 2000,
            mtm_budget_hysteresis: 200,
            mtm_max_batch_days: 7,
            compression_retry_max: 3,
            compression_retry_base_backoff_secs: 30,
            recall_top_k_per_tier: 10,
            recall_final_top_k: 5,
            weight_stm: 0.45,
            weight_mtm: 0.35,
            weight_ltm: 0.20,
            min_relevance_threshold: 0.4,
            memory_agents: MemoryAgentsConfig::default(),
        }
    }
}

/// Serde helper: serialise/deserialise `std::time::Duration` as whole seconds.
mod duration_secs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_secs().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}

// ── MemoryTier ────────────────────────────────────────────────────────────────

/// Identifies which storage tier a memory item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryTier {
    /// Short-term memory — always injected into prompts.
    Stm,
    /// Medium-term memory — compressed daily summaries.
    Mtm,
    /// Long-term memory — existing zeroclaw persistent storage.
    Ltm,
}

// ── IndexEntry ────────────────────────────────────────────────────────────────

/// A cross-tier index record that links a topic/day to MTM and LTM references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Unique identifier in the format `idx-{topic}-{YYYY-MM-DD}`.
    pub id: String,
    /// Primary group label for the entry.
    pub topic: String,
    /// 2-6 lowercase kebab-case tags.
    pub tags: Vec<String>,
    /// Calendar date this entry covers.
    pub day: NaiveDate,
    /// When this index record was created.
    pub created_at: DateTime<Utc>,
    /// Reference ID into the MTM layer, if one exists.
    pub mtm_ref_id: Option<String>,
    /// Reference ID into the LTM layer, if one exists.
    pub ltm_ref_id: Option<String>,
    /// IDs of the raw STM entries that were summarised into this record.
    pub source_entry_ids: Vec<String>,
    /// Confidence score for the extraction, 0.0–1.0.
    pub confidence: f32,
    /// Version of the extractor that produced this entry.
    pub extractor_version: String,
}

impl IndexEntry {
    /// Create a new `IndexEntry` for the given topic and day.
    pub fn new(topic: &str, day: NaiveDate) -> Self {
        let id = format!("idx-{}-{}", topic, day.format("%Y-%m-%d"));
        Self {
            id,
            topic: topic.to_string(),
            tags: Vec::new(),
            day,
            created_at: Utc::now(),
            mtm_ref_id: None,
            ltm_ref_id: None,
            source_entry_ids: Vec::new(),
            confidence: 0.0,
            extractor_version: "1.0".to_string(),
        }
    }

    /// Returns a human-readable summary: `[tags] date → MTM:id, LTM:id`.
    pub fn to_display(&self) -> String {
        let tags = self.tags.join(", ");
        let date = self.day.format("%Y-%m-%d");
        let mtm = self
            .mtm_ref_id
            .as_deref()
            .unwrap_or("none");
        let ltm = self
            .ltm_ref_id
            .as_deref()
            .unwrap_or("none");
        format!("[{tags}] {date} → MTM:{mtm}, LTM:{ltm}")
    }
}

// ── CompressionJobKind ────────────────────────────────────────────────────────

/// Describes what kind of compression a job performs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompressionJobKind {
    /// Roll one day of STM entries into an MTM compressed summary.
    StmDayToMtm { day: NaiveDate },
    /// Spill MTM entries that exceed the token budget into LTM.
    MtmOverflowToLtm { mtm_ids: Vec<String> },
}

// ── CompressionJobStatus ──────────────────────────────────────────────────────

/// Lifecycle status of a `CompressionJob`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionJobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

// ── CompressionJob ────────────────────────────────────────────────────────────

/// A unit of work that compresses memory from one tier to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionJob {
    /// Unique job identifier.
    pub id: String,
    /// What this job does.
    pub kind: CompressionJobKind,
    /// Current lifecycle status.
    pub status: CompressionJobStatus,
    /// Number of attempts made so far.
    pub attempts: u32,
    /// Error message from the most recent failed attempt.
    pub last_error: Option<String>,
    /// Earliest time the job may be retried.
    pub next_retry_at: Option<DateTime<Utc>>,
    /// When the job was created.
    pub created_at: DateTime<Utc>,
    /// When the job was last updated.
    pub updated_at: DateTime<Utc>,
}

impl CompressionJob {
    fn new(kind: CompressionJobKind) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            kind,
            status: CompressionJobStatus::Pending,
            attempts: 0,
            last_error: None,
            next_retry_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Create a job that compresses one day of STM into MTM.
    pub fn new_stm_to_mtm(day: NaiveDate) -> Self {
        Self::new(CompressionJobKind::StmDayToMtm { day })
    }

    /// Create a job that spills overflowing MTM entries into LTM.
    pub fn new_mtm_overflow(mtm_ids: Vec<String>) -> Self {
        Self::new(CompressionJobKind::MtmOverflowToLtm { mtm_ids })
    }

    /// Returns `true` when the job has reached a final state (success or failure).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            CompressionJobStatus::Succeeded | CompressionJobStatus::Failed
        )
    }
}

// ── TierCommand ───────────────────────────────────────────────────────────────

/// Control commands sent to the tiered-memory coordinator.
#[derive(Debug, Clone)]
pub enum TierCommand {
    /// Trigger end-of-day STM compression for a specific date.
    ForceEodCompression { day: NaiveDate },
    /// Check whether the MTM layer has exceeded its token budget.
    ForceMtmBudgetCheck,
    /// Gracefully shut down the coordinator.
    Shutdown,
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_config_defaults_are_sane() {
        let cfg = TierConfig::default();
        assert_eq!(cfg.stm_eod_hour, 23);
        assert_eq!(cfg.stm_eod_minute, 59);
        assert_eq!(cfg.mtm_token_budget, 2000);
        assert!(cfg.min_relevance_threshold > 0.0);
        let sum = cfg.weight_stm + cfg.weight_mtm + cfg.weight_ltm;
        assert!((sum - 1.0).abs() < 1e-5, "weights sum={}", sum);
    }

    #[test]
    fn compression_job_initial_state_is_pending() {
        let job = CompressionJob::new_stm_to_mtm(
            chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
        );
        assert_eq!(job.status, CompressionJobStatus::Pending);
        assert_eq!(job.attempts, 0);
        assert!(job.last_error.is_none());
        assert!(!job.is_terminal());
    }

    #[test]
    fn default_memory_agent_config_has_sane_values() {
        let cfg = MemoryAgentConfig::default();
        assert_eq!(cfg.model, "google/gemini-3-flash-preview");
        assert_eq!(cfg.temperature, 0.1);
        assert!(cfg.timeout_secs > 0);
        assert!(cfg.max_tokens > 0);
    }

    #[test]
    fn default_tier_config_has_memory_agents() {
        let cfg = TierConfig::default();
        assert_eq!(cfg.memory_agents.openrouter_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(cfg.memory_agents.stm.model, "google/gemini-3-flash-preview");
        assert_eq!(cfg.memory_agents.stm.timeout_secs, 10);
        assert_eq!(cfg.memory_agents.mtm.timeout_secs, 60);
        assert_eq!(cfg.memory_agents.ltm.timeout_secs, 60);
    }

    #[test]
    fn index_entry_display_includes_refs() {
        let mut entry =
            IndexEntry::new("auth", chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap());
        entry.tags = vec!["auth".to_string(), "middleware".to_string()];
        entry.mtm_ref_id = Some("mtm-001".to_string());
        entry.ltm_ref_id = Some("ltm-001".to_string());
        let display = entry.to_display();
        assert!(display.contains("auth"), "display: {}", display);
        assert!(display.contains("mtm-001"), "display: {}", display);
        assert!(display.contains("ltm-001"), "display: {}", display);
    }
}
