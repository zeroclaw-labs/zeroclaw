//! CozoDB Datalog schema definitions for the knowledge graph.
//!
//! Defines 8 node types, 15 relation types, and 3 HNSW vector indexes.
//! All schema operations use CozoDB Datalog syntax.

/// Complete schema initialization script for CozoDB.
/// Creates all stored relations (tables) and HNSW indexes.
pub fn schema_init_script() -> &'static str {
    r#"
{
    # ── Node relations (8 types) ──────────────────────────────────────

    # Concepts: core knowledge entities
    :create concept {
        id: String =>
        name: String,
        description: String,
        category: String default 'general',
        heat: Float default 1.0,
        last_accessed: String,
        created_at: String,
        embedding: [Float; 384] default [0.0; 384]
    }

    # Facts: verified atomic statements
    :create fact {
        id: String =>
        content: String,
        source: String default 'user',
        confidence: Float default 1.0,
        heat: Float default 1.0,
        last_accessed: String,
        created_at: String,
        embedding: [Float; 384] default [0.0; 384]
    }

    # Episodes: conversation/event memories
    :create episode {
        id: String =>
        content: String,
        session_id: String default '',
        heat: Float default 1.0,
        emotion_valence: Float default 0.0,
        emotion_arousal: Float default 0.0,
        emotion_dominance: Float default 0.0,
        last_accessed: String,
        created_at: String,
        embedding: [Float; 384] default [0.0; 384]
    }

    # Hypotheses: unverified propositions awaiting validation
    :create hypothesis {
        id: String =>
        claim: String,
        evidence_for: String default '',
        evidence_against: String default '',
        confidence: Float default 0.5,
        status: String default 'open',
        heat: Float default 1.0,
        last_accessed: String,
        created_at: String,
        embedding: [Float; 384] default [0.0; 384]
    }

    # Entities: named entities (people, places, orgs)
    :create entity {
        id: String =>
        name: String,
        entity_type: String default 'unknown',
        heat: Float default 1.0,
        last_accessed: String,
        created_at: String
    }

    # Topics: high-level topic clusters
    :create topic {
        id: String =>
        name: String,
        description: String default '',
        heat: Float default 1.0,
        last_accessed: String,
        created_at: String
    }

    # Preferences: user preferences and settings
    :create preference {
        id: String =>
        key: String,
        value: String,
        heat: Float default 1.0,
        last_accessed: String,
        created_at: String
    }

    # Skills: learned capabilities and procedures
    :create skill {
        id: String =>
        name: String,
        description: String default '',
        proficiency: Float default 0.5,
        heat: Float default 1.0,
        last_accessed: String,
        created_at: String
    }

    # ── Relation edges (15 types) ─────────────────────────────────────

    # Epistemic relations
    :create relates_to {
        from_id: String,
        to_id: String =>
        relation_type: String default 'related',
        weight: Float default 1.0,
        created_at: String
    }

    :create supports {
        fact_id: String,
        hypothesis_id: String =>
        strength: Float default 1.0,
        created_at: String
    }

    :create contradicts {
        fact_id: String,
        hypothesis_id: String =>
        strength: Float default 1.0,
        created_at: String
    }

    :create derived_from {
        child_id: String,
        parent_id: String =>
        derivation_type: String default 'inference',
        created_at: String
    }

    # Semantic relations
    :create is_a {
        child_id: String,
        parent_id: String =>
        created_at: String
    }

    :create part_of {
        part_id: String,
        whole_id: String =>
        created_at: String
    }

    :create has_property {
        entity_id: String,
        property_id: String =>
        value: String default '',
        created_at: String
    }

    # Temporal and contextual relations
    :create mentioned_in {
        entity_id: String,
        episode_id: String =>
        created_at: String
    }

    :create belongs_to_topic {
        node_id: String,
        topic_id: String =>
        created_at: String
    }

    :create precedes {
        before_id: String,
        after_id: String =>
        created_at: String
    }

    :create causes {
        cause_id: String,
        effect_id: String =>
        confidence: Float default 0.5,
        created_at: String
    }

    # User-centric relations
    :create prefers {
        user_pref_id: String,
        concept_id: String =>
        created_at: String
    }

    :create knows {
        skill_id: String,
        concept_id: String =>
        created_at: String
    }

    :create similar_to {
        id_a: String,
        id_b: String =>
        similarity: Float default 0.0,
        created_at: String
    }

    :create evolves_into {
        old_id: String,
        new_id: String =>
        reason: String default '',
        created_at: String
    }
}
"#
}

/// HNSW index creation scripts (run separately after schema init).
pub fn hnsw_index_scripts() -> Vec<&'static str> {
    vec![
        r#"::hnsw create concept:semantic_idx {
            dim: 384,
            m: 16,
            dtype: F32,
            fields: [embedding],
            distance: Cosine,
            ef_construction: 200,
            filter: id
        }"#,
        r#"::hnsw create fact:semantic_idx {
            dim: 384,
            m: 16,
            dtype: F32,
            fields: [embedding],
            distance: Cosine,
            ef_construction: 200,
            filter: id
        }"#,
        r#"::hnsw create episode:semantic_idx {
            dim: 384,
            m: 16,
            dtype: F32,
            fields: [embedding],
            distance: Cosine,
            ef_construction: 200,
            filter: id
        }"#,
    ]
}

/// Query to count all nodes across all types.
pub fn count_all_nodes_query() -> &'static str {
    r#"
    c_count[count(id)] := *concept{id}
    f_count[count(id)] := *fact{id}
    e_count[count(id)] := *episode{id}
    h_count[count(id)] := *hypothesis{id}
    en_count[count(id)] := *entity{id}
    t_count[count(id)] := *topic{id}
    p_count[count(id)] := *preference{id}
    s_count[count(id)] := *skill{id}
    ?[total] := c_count[c], f_count[f], e_count[e], h_count[h],
                en_count[en], t_count[t], p_count[p], s_count[s],
                total = c + f + e + h + en + t + p + s
    "#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_script_is_not_empty() {
        assert!(!schema_init_script().is_empty());
    }

    #[test]
    fn hnsw_scripts_has_three_indexes() {
        assert_eq!(hnsw_index_scripts().len(), 3);
    }

    #[test]
    fn count_query_is_valid_datalog_shape() {
        let q = count_all_nodes_query();
        assert!(q.contains("?[total]"));
        assert!(q.contains("*concept"));
    }
}
