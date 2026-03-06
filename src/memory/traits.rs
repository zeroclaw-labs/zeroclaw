use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A single memory entry
#[derive(Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub score: Option<f64>,
}

impl std::fmt::Debug for MemoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEntry")
            .field("id", &self.id)
            .field("key", &self.key)
            .field("content", &self.content)
            .field("category", &self.category)
            .field("timestamp", &self.timestamp)
            .field("score", &self.score)
            .finish_non_exhaustive()
    }
}

/// Memory categories for organization
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCategory {
    /// Long-term facts, preferences, decisions
    Core,
    /// Daily session logs
    Daily,
    /// Conversation context
    Conversation,
    /// User-defined custom category
    Custom(String),
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Daily => write!(f, "daily"),
            Self::Conversation => write!(f, "conversation"),
            Self::Custom(name) => write!(f, "{name}"),
        }
    }
}

impl serde::Serialize for MemoryCategory {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Core => serializer.serialize_str("core"),
            Self::Daily => serializer.serialize_str("daily"),
            Self::Conversation => serializer.serialize_str("conversation"),
            Self::Custom(name) => {
                // Prefix reserved names and values already starting with "custom:" to prevent
                // round-trip ambiguity:
                //   Custom("core")       → "custom:core"       (avoids colliding with Core)
                //   Custom("custom:foo") → "custom:custom:foo" (avoids stripping the prefix on
                //                                               deserialization)
                let needs_prefix = matches!(name.as_str(), "core" | "daily" | "conversation")
                    || name.starts_with("custom:");
                let encoded = if needs_prefix {
                    format!("custom:{name}")
                } else {
                    name.clone()
                };
                serializer.serialize_str(&encoded)
            }
        }
    }
}

// Wire type for MemoryCategory deserialization.
// Accepts two forms:
//   plain string → built-in variants or Custom
//   {"custom": "value"} → Custom (legacy format produced by the old serde derive)
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum MemoryCategoryWire {
    Name(String),
    LegacyCustom { custom: String },
}

impl<'de> serde::Deserialize<'de> for MemoryCategory {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match MemoryCategoryWire::deserialize(deserializer)? {
            MemoryCategoryWire::Name(s) => match s.as_str() {
                "core" => Self::Core,
                "daily" => Self::Daily,
                "conversation" => Self::Conversation,
                other if other.starts_with("custom:") => {
                    Self::Custom(other["custom:".len()..].to_string())
                }
                other => Self::Custom(other.to_string()),
            },
            MemoryCategoryWire::LegacyCustom { custom } => Self::Custom(custom),
        })
    }
}

/// Core memory trait — implement for any persistence backend
#[async_trait]
pub trait Memory: Send + Sync {
    /// Backend name
    fn name(&self) -> &str;

    /// Store a memory entry, optionally scoped to a session
    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()>;

    /// Recall memories matching a query (keyword search), optionally scoped to a session
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Get a specific memory by key
    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>>;

    /// List all memory keys, optionally filtered by category and/or session
    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Remove a memory by key
    async fn forget(&self, key: &str) -> anyhow::Result<bool>;

    /// Count total memories
    async fn count(&self) -> anyhow::Result<usize>;

    /// Health check
    async fn health_check(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_category_display_outputs_expected_values() {
        assert_eq!(MemoryCategory::Core.to_string(), "core");
        assert_eq!(MemoryCategory::Daily.to_string(), "daily");
        assert_eq!(MemoryCategory::Conversation.to_string(), "conversation");
        assert_eq!(
            MemoryCategory::Custom("project_notes".into()).to_string(),
            "project_notes"
        );
    }

    #[test]
    fn memory_category_serde_roundtrip_uses_plain_strings() {
        let core = serde_json::to_string(&MemoryCategory::Core).unwrap();
        let daily = serde_json::to_string(&MemoryCategory::Daily).unwrap();
        let conversation = serde_json::to_string(&MemoryCategory::Conversation).unwrap();
        let custom = serde_json::to_string(&MemoryCategory::Custom("travel".into())).unwrap();

        assert_eq!(core, "\"core\"");
        assert_eq!(daily, "\"daily\"");
        assert_eq!(conversation, "\"conversation\"");
        assert_eq!(custom, "\"travel\"");

        assert_eq!(
            serde_json::from_str::<MemoryCategory>("\"core\"").unwrap(),
            MemoryCategory::Core
        );
        assert_eq!(
            serde_json::from_str::<MemoryCategory>("\"daily\"").unwrap(),
            MemoryCategory::Daily
        );
        assert_eq!(
            serde_json::from_str::<MemoryCategory>("\"conversation\"").unwrap(),
            MemoryCategory::Conversation
        );
        assert_eq!(
            serde_json::from_str::<MemoryCategory>("\"travel\"").unwrap(),
            MemoryCategory::Custom("travel".into())
        );
    }

    #[test]
    fn memory_category_custom_reserved_name_roundtrips() {
        // Custom("core") must not deserialize into Core
        let v = MemoryCategory::Custom("core".into());
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"custom:core\"");
        let parsed: MemoryCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, MemoryCategory::Custom("core".into()));
    }

    #[test]
    fn memory_category_custom_prefixed_name_roundtrips() {
        // Custom("custom:foo") must not lose the prefix during round-trip
        let v = MemoryCategory::Custom("custom:foo".into());
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"custom:custom:foo\"");
        let parsed: MemoryCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, MemoryCategory::Custom("custom:foo".into()));
    }

    #[test]
    fn memory_category_legacy_object_format_deserializes() {
        // Legacy {"custom": "value"} produced by old serde derive
        let legacy_json = r#"{"custom": "project_notes"}"#;
        let parsed: MemoryCategory = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(parsed, MemoryCategory::Custom("project_notes".into()));
    }

    #[test]
    fn memory_entry_roundtrip_preserves_optional_fields() {
        let entry = MemoryEntry {
            id: "id-1".into(),
            key: "favorite_language".into(),
            content: "Rust".into(),
            category: MemoryCategory::Core,
            timestamp: "2026-02-16T00:00:00Z".into(),
            session_id: Some("session-abc".into()),
            score: Some(0.98),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, "id-1");
        assert_eq!(parsed.key, "favorite_language");
        assert_eq!(parsed.content, "Rust");
        assert_eq!(parsed.category, MemoryCategory::Core);
        assert_eq!(parsed.session_id.as_deref(), Some("session-abc"));
        assert_eq!(parsed.score, Some(0.98));
    }

    #[test]
    fn memory_entry_serializes_custom_category_as_plain_string() {
        let entry = MemoryEntry {
            id: "id-2".into(),
            key: "trip".into(),
            content: "booked a flight".into(),
            category: MemoryCategory::Custom("travel".into()),
            timestamp: "2026-03-04T00:00:00Z".into(),
            session_id: None,
            score: None,
        };

        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json.get("category").unwrap(), "travel");
    }
}
