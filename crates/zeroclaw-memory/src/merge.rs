use crate::traits::MemoryEntry;

#[derive(Debug, Clone, PartialEq)]
pub struct MergedFact {
    pub content: String,
    pub importance: Option<f64>,
}

pub fn merge_into_survivor(survivor: &MemoryEntry, incoming: &str) -> MergedFact {
    let survivor_content = survivor.content.trim();
    let incoming = incoming.trim();

    let content = if incoming.is_empty()
        || survivor_content == incoming
        || survivor_content.contains(incoming)
    {
        survivor_content.to_string()
    } else if incoming.contains(survivor_content) {
        incoming.to_string()
    } else {
        let mut facts = [survivor_content, incoming];
        facts.sort_unstable();
        facts.join("\n")
    };

    MergedFact {
        content,
        importance: survivor.importance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryCategory;

    fn survivor(content: &str) -> MemoryEntry {
        MemoryEntry {
            id: "id".into(),
            key: "key".into(),
            content: content.into(),
            category: MemoryCategory::Core,
            timestamp: "now".into(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: Some(0.8),
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }
    }

    #[test]
    fn merge_is_deterministic() {
        let merged = merge_into_survivor(&survivor("B fact"), "A fact");
        assert_eq!(merged.content, "A fact\nB fact");
        assert_eq!(merged.importance, Some(0.8));
    }
}
