//! Structured fact extraction types and key taxonomy for the tiered memory system.
//!
//! [`FactEntry`] captures a single extracted fact with provenance, confidence,
//! versioning, and lifecycle metadata. The key taxonomy functions produce
//! deterministic, human-readable keys used to store and deduplicate facts across
//! STM/MTM/LTM tiers.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// ── Enums ────────────────────────────────────────────────────────────────────

/// Confidence level assigned to an extracted fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FactConfidence {
    Low,
    Medium,
    High,
}

/// Lifecycle status of a fact entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FactStatus {
    Active,
    Superseded,
    Retracted,
}

/// Role of the source that produced or conveyed the fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceRole {
    User,
    Agent,
    System,
}

/// How quickly a fact is expected to change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VolatilityClass {
    Stable,
    SemiStable,
    Volatile,
}

// ── SourceTurnRef ────────────────────────────────────────────────────────────

/// Provenance pointer back to the conversation turn where a fact was extracted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceTurnRef {
    pub conversation_id: String,
    pub turn_index: u64,
    pub message_id: Option<String>,
    pub role: SourceRole,
    pub timestamp_unix_ms: i64,
}

// ── FactEntry ────────────────────────────────────────────────────────────────

/// A single structured fact extracted from a conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactEntry {
    pub fact_id: String,
    pub fact_key: String,
    pub category: String,
    pub subject: String,
    pub attribute: String,
    pub value: String,
    pub context_narrative: String,
    pub source_turn: SourceTurnRef,
    pub confidence: FactConfidence,
    pub related_facts: Vec<String>,
    pub extracted_by_tier: String,
    pub extracted_at_unix_ms: i64,
    pub source_role: SourceRole,
    pub status: FactStatus,
    pub revision: u32,
    pub supersedes_fact_id: Option<String>,
    pub tags: Vec<String>,
    pub volatility_class: VolatilityClass,
    pub ttl_days: Option<u16>,
    pub expires_at_unix_ms: Option<i64>,
    pub last_verified_unix_ms: Option<i64>,
}

impl FactEntry {
    /// Produce a single searchable text blob for embedding or full-text search.
    pub fn to_searchable_text(&self) -> String {
        format!(
            "{} {} {}: {}. {}",
            self.category, self.subject, self.attribute, self.value, self.context_narrative,
        )
    }

    /// Mark this fact as superseded by a newer revision.
    pub fn mark_superseded(&mut self) {
        self.status = FactStatus::Superseded;
    }

    /// Poisoning guard: agent-sourced facts must not carry `High` confidence.
    ///
    /// If the fact was produced by the agent and currently claims `High`
    /// confidence, demote it to `Medium` to prevent self-reinforcing loops.
    pub fn apply_poisoning_guard(&mut self) {
        if self.source_role == SourceRole::Agent && self.confidence == FactConfidence::High {
            self.confidence = FactConfidence::Medium;
        }
    }
}

// ── Key taxonomy ─────────────────────────────────────────────────────────────

/// Convert an arbitrary string into a URL-safe slug.
///
/// Lowercases the input, replaces non-alphanumeric characters with hyphens,
/// collapses consecutive hyphens, and trims leading/trailing hyphens.
fn slugify(s: &str) -> String {
    let lowered = s.to_lowercase();
    let mut slug = String::with_capacity(lowered.len());

    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else {
            slug.push('-');
        }
    }

    // Collapse consecutive hyphens.
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_hyphen = false;
    for ch in slug.chars() {
        if ch == '-' {
            if !prev_hyphen {
                collapsed.push('-');
            }
            prev_hyphen = true;
        } else {
            collapsed.push(ch);
            prev_hyphen = false;
        }
    }

    // Trim leading/trailing hyphens.
    collapsed.trim_matches('-').to_string()
}

/// Build a deterministic fact key from category, subject, and attribute.
///
/// Format: `fact:{category_slug}:{subject_slug}:{attribute_slug}`
pub fn build_fact_key(category: &str, subject: &str, attribute: &str) -> String {
    format!(
        "fact:{}:{}:{}",
        slugify(category),
        slugify(subject),
        slugify(attribute),
    )
}

/// Build a fact key for multi-valued facts, appending an 8-character hash of the value.
///
/// Format: `fact:{category_slug}:{subject_slug}:{attribute_slug}:{value_hash}`
pub fn build_fact_key_multivalued(
    category: &str,
    subject: &str,
    attribute: &str,
    value: &str,
) -> String {
    let base = build_fact_key(category, subject, attribute);
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    let hash = hasher.finish();
    let hash_str = format!("{:016x}", hash);
    format!("{}:{}", base, &hash_str[..8])
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a minimal FactEntry for testing.
    fn make_test_fact() -> FactEntry {
        FactEntry {
            fact_id: "test-fact-001".to_string(),
            fact_key: build_fact_key("personal", "user", "birthday"),
            category: "personal".to_string(),
            subject: "user".to_string(),
            attribute: "birthday".to_string(),
            value: "1990-05-15".to_string(),
            context_narrative: "User mentioned their birthday in passing.".to_string(),
            source_turn: SourceTurnRef {
                conversation_id: "conv-abc".to_string(),
                turn_index: 3,
                message_id: Some("msg-xyz".to_string()),
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
        }
    }

    #[test]
    fn fact_key_builds_correctly() {
        let key = build_fact_key("personal", "user", "birthday");
        assert_eq!(key, "fact:personal:user:birthday");
    }

    #[test]
    fn fact_key_slugifies_inputs() {
        let key = build_fact_key("Personal Info", "User Name", "Birth Date");
        assert_eq!(key, "fact:personal-info:user-name:birth-date");
    }

    #[test]
    fn fact_key_with_vhash_for_multivalued() {
        let key = build_fact_key_multivalued("hobby", "user", "likes", "rust programming");
        let prefix = "fact:hobby:user:likes:";
        assert!(
            key.starts_with(prefix),
            "expected key to start with `{prefix}`, got `{key}`"
        );
        // After the prefix there should be exactly 8 hex characters.
        let suffix = &key[prefix.len()..];
        assert_eq!(
            suffix.len(),
            8,
            "expected 8-char hash suffix, got `{suffix}` (len={})",
            suffix.len()
        );
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "hash suffix should be hex, got `{suffix}`"
        );
    }

    #[test]
    fn fact_entry_to_searchable_text() {
        let fact = make_test_fact();
        let text = fact.to_searchable_text();
        assert!(text.contains("personal"), "should contain category");
        assert!(text.contains("1990-05-15"), "should contain value");
        assert!(
            text.contains("User mentioned their birthday"),
            "should contain context narrative"
        );
    }

    #[test]
    fn supersede_marks_old_fact() {
        let mut fact = make_test_fact();
        assert_eq!(fact.status, FactStatus::Active);
        fact.mark_superseded();
        assert_eq!(fact.status, FactStatus::Superseded);
    }

    #[test]
    fn agent_fact_capped_at_medium_confidence() {
        let mut fact = make_test_fact();
        fact.source_role = SourceRole::Agent;
        fact.confidence = FactConfidence::High;
        fact.apply_poisoning_guard();
        assert_eq!(
            fact.confidence,
            FactConfidence::Medium,
            "agent-sourced High confidence should be demoted to Medium"
        );
    }
}
