//! Smart recall: entity extraction + Datalog graph traversal.
//!
//! The retrieval pipeline:
//! 1. Extract entities from the query using heuristic extractor
//! 2. Look up matching nodes in the graph (exact + fuzzy)
//! 3. Traverse 2-hop neighborhoods from matched nodes
//! 4. Combine with hot nodes (high heat)
//! 5. Score and rank results
//! 6. Return as Vec<MemoryEntry>

use crate::memory::traits::{MemoryCategory, MemoryEntry};
use chrono::Utc;
use std::collections::HashSet;

/// Build a Datalog query to find nodes matching entity names.
///
/// Returns matching concept/fact/episode IDs with their content and heat.
pub fn build_entity_lookup_query(entity_names: &[String]) -> String {
    if entity_names.is_empty() {
        // Fallback: return recent hot nodes
        return r#"
            ?[id, content, heat, node_type, last_accessed] :=
                *concept{id, name: content, heat, last_accessed},
                heat > 0.1,
                node_type = 'concept'
            ?[id, content, heat, node_type, last_accessed] :=
                *fact{id, content, heat, last_accessed},
                heat > 0.1,
                node_type = 'fact'
            :order -heat
            :limit 20
        "#
        .to_string();
    }

    // Build OR conditions for entity name matching
    let name_conditions: Vec<String> = entity_names
        .iter()
        .map(|name| {
            let escaped = name.replace('\'', "\\'");
            format!("name == '{escaped}'")
        })
        .collect();

    let content_conditions: Vec<String> = entity_names
        .iter()
        .map(|name| {
            let escaped = name.replace('\'', "\\'");
            format!("contains(content, '{escaped}')")
        })
        .collect();

    let name_filter = name_conditions.join(" or ");
    let content_filter = content_conditions.join(" or ");

    format!(
        r#"
        # Direct concept matches by name
        matched[id, content, heat, node_type, last_accessed] :=
            *concept{{id, name: content, heat, last_accessed}},
            ({name_filter}),
            node_type = 'concept'

        # Facts containing search terms
        matched[id, content, heat, node_type, last_accessed] :=
            *fact{{id, content, heat, last_accessed}},
            ({content_filter}),
            node_type = 'fact'

        # Episodes containing search terms
        matched[id, content, heat, node_type, last_accessed] :=
            *episode{{id, content, heat, last_accessed}},
            ({content_filter}),
            node_type = 'episode'

        # 1-hop neighbors via relates_to
        hop1[id, content, heat, node_type, last_accessed] :=
            matched[mid, _, _, _, _],
            *relates_to{{from_id: mid, to_id: id}},
            *concept{{id, name: content, heat, last_accessed}},
            node_type = 'concept'

        hop1[id, content, heat, node_type, last_accessed] :=
            matched[mid, _, _, _, _],
            *relates_to{{to_id: mid, from_id: id}},
            *concept{{id, name: content, heat, last_accessed}},
            node_type = 'concept'

        # Combine matched + 1-hop
        ?[id, content, heat, node_type, last_accessed] :=
            matched[id, content, heat, node_type, last_accessed]
        ?[id, content, heat, node_type, last_accessed] :=
            hop1[id, content, heat, node_type, last_accessed]

        :order -heat
        :limit 30
        "#,
        name_filter = name_filter,
        content_filter = content_filter,
    )
}

/// Build a query to get hot nodes (high-heat nodes across all types).
pub fn build_hot_nodes_query(threshold: f64, limit: usize) -> String {
    format!(
        r#"
        hot[id, content, heat, node_type] :=
            *concept{{id, name: content, heat}}, heat >= {threshold},
            node_type = 'concept'
        hot[id, content, heat, node_type] :=
            *fact{{id, content, heat}}, heat >= {threshold},
            node_type = 'fact'
        hot[id, content, heat, node_type] :=
            *episode{{id, content, heat}}, heat >= {threshold},
            node_type = 'episode'
        ?[id, content, heat, node_type] := hot[id, content, heat, node_type]
        :order -heat
        :limit {limit}
        "#,
        threshold = threshold,
        limit = limit,
    )
}

/// Convert CozoDB query result rows into MemoryEntry values.
///
/// Expected columns: id, content, heat, node_type, (optional) last_accessed
pub fn rows_to_memory_entries(
    rows: &[Vec<serde_json::Value>],
    session_id: Option<&str>,
) -> Vec<MemoryEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();

    for row in rows {
        let id = row.first().and_then(|v| v.as_str()).unwrap_or_default();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }

        let content = row.get(1).and_then(|v| v.as_str()).unwrap_or_default();
        let heat = row.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let node_type = row.get(3).and_then(|v| v.as_str()).unwrap_or("unknown");
        let last_accessed = row
            .get(4)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let category = match node_type {
            "concept" | "fact" | "preference" | "skill" => MemoryCategory::Core,
            "episode" => MemoryCategory::Conversation,
            _ => MemoryCategory::Custom(node_type.to_string()),
        };

        entries.push(MemoryEntry {
            id: id.to_string(),
            key: format!("graph_{node_type}_{id}"),
            content: content.to_string(),
            category,
            timestamp: if last_accessed.is_empty() {
                Utc::now().to_rfc3339()
            } else {
                last_accessed
            },
            session_id: session_id.map(String::from),
            score: Some(heat),
        });
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_entities_returns_hot_nodes_query() {
        let q = build_entity_lookup_query(&[]);
        assert!(q.contains("heat > 0.1"));
        assert!(q.contains(":limit 20"));
    }

    #[test]
    fn entity_lookup_query_includes_names() {
        let names = vec!["Rust".to_string(), "tokio".to_string()];
        let q = build_entity_lookup_query(&names);
        assert!(q.contains("Rust"));
        assert!(q.contains("tokio"));
        assert!(q.contains("matched"));
    }

    #[test]
    fn hot_nodes_query_uses_threshold() {
        let q = build_hot_nodes_query(0.5, 10);
        assert!(q.contains("0.5"));
        assert!(q.contains(":limit 10"));
    }

    #[test]
    fn rows_to_entries_deduplicates() {
        let rows = vec![
            vec![
                serde_json::json!("id1"),
                serde_json::json!("content1"),
                serde_json::json!(0.8),
                serde_json::json!("concept"),
                serde_json::json!("2026-01-01T00:00:00Z"),
            ],
            vec![
                serde_json::json!("id1"),
                serde_json::json!("content1_dup"),
                serde_json::json!(0.7),
                serde_json::json!("concept"),
                serde_json::json!("2026-01-01T00:00:00Z"),
            ],
        ];

        let entries = rows_to_memory_entries(&rows, None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "id1");
    }
}
