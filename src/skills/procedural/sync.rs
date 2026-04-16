//! Sync integration for procedural skills.
//!
//! Extends the DeltaOperation enum pattern with SkillUpsert for cross-device
//! replication of learned skills.

use serde::{Deserialize, Serialize};

/// Sync delta payload for skill replication.
///
/// Designed to be serialized into a `DeltaEntry::operation` alongside
/// the existing `DeltaOperation` variants. The receiving peer applies
/// version-LWW: higher version wins, ties broken by device_id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillUpsertDelta {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub description: String,
    pub content_md: String,
    pub version: i64,
    pub created_by: String,
}

/// Sync delta payload for user profile conclusion replication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserProfileConclusionDelta {
    pub dimension: String,
    pub conclusion: String,
    pub confidence: f64,
    pub evidence_count: i64,
}

/// Sync delta payload for correction pattern replication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CorrectionPatternDelta {
    pub pattern_type: String,
    pub original_regex: String,
    pub replacement: String,
    pub scope: String,
    pub confidence: f64,
    pub observation_count: i64,
    pub accept_count: i64,
    pub reject_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_delta_roundtrip() {
        let delta = SkillUpsertDelta {
            id: "s1".into(),
            name: "test-skill".into(),
            category: Some("coding".into()),
            description: "Test".into(),
            content_md: "# Content".into(),
            version: 3,
            created_by: "agent".into(),
        };
        let json = serde_json::to_string(&delta).unwrap();
        let parsed: SkillUpsertDelta = serde_json::from_str(&json).unwrap();
        assert_eq!(delta, parsed);
    }

    #[test]
    fn profile_delta_roundtrip() {
        let delta = UserProfileConclusionDelta {
            dimension: "response_style".into(),
            conclusion: "간결한 응답 선호".into(),
            confidence: 0.85,
            evidence_count: 5,
        };
        let json = serde_json::to_string(&delta).unwrap();
        let parsed: UserProfileConclusionDelta = serde_json::from_str(&json).unwrap();
        assert_eq!(delta, parsed);
    }
}
