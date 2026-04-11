pub use zeroclaw_api::memory_traits::*;

pub use async_trait::async_trait;

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
    fn memory_category_serde_uses_snake_case() {
        let core = serde_json::to_string(&MemoryCategory::Core).unwrap();
        let daily = serde_json::to_string(&MemoryCategory::Daily).unwrap();
        let conversation = serde_json::to_string(&MemoryCategory::Conversation).unwrap();

        assert_eq!(core, "\"core\"");
        assert_eq!(daily, "\"daily\"");
        assert_eq!(conversation, "\"conversation\"");
    }

    #[test]
    fn memory_category_custom_roundtrip() {
        let custom = MemoryCategory::Custom("project_notes".into());
        let json = serde_json::to_string(&custom).unwrap();
        assert_eq!(json, "\"project_notes\"");
        let parsed: MemoryCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, custom);
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
            namespace: "default".into(),
            importance: Some(0.7),
            superseded_by: None,
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, "id-1");
        assert_eq!(parsed.key, "favorite_language");
        assert_eq!(parsed.content, "Rust");
        assert_eq!(parsed.category, MemoryCategory::Core);
        assert_eq!(parsed.session_id.as_deref(), Some("session-abc"));
        assert_eq!(parsed.score, Some(0.98));
        assert_eq!(parsed.namespace, "default");
        assert_eq!(parsed.importance, Some(0.7));
        assert!(parsed.superseded_by.is_none());
    }
}
