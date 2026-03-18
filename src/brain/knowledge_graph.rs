//! Knowledge Graph layer — relational world model on top of RVF+SQLite.
//!
//! Uses ruvector-graph (in-memory GraphDB backed by redb) to store structural
//! relationships extracted from ingested memories. Enables 1-hop context
//! expansion on recall.

use anyhow::Result;
use ruvector_graph::{Edge, EdgeBuilder, GraphDB, Node, NodeBuilder, types::PropertyValue};

/// Simple entity-relation-entity triple extracted from text.
#[derive(Debug, Clone)]
pub struct Triple {
    pub subject: String,
    pub relation: String,
    pub object: String,
}

pub struct KnowledgeGraph {
    db: GraphDB,
}

impl KnowledgeGraph {
    pub fn new() -> Self {
        Self { db: GraphDB::new() }
    }

    pub fn open_persistent(path: &str) -> Self {
        match GraphDB::with_storage(path) {
            Ok(db) => {
                tracing::info!(
                    path,
                    nodes = db.node_count(),
                    edges = db.edge_count(),
                    "knowledge graph opened"
                );
                Self { db }
            }
            Err(e) => {
                tracing::warn!(path, error = %e, "knowledge graph persistent open failed, using in-memory");
                Self { db: GraphDB::new() }
            }
        }
    }

    pub fn record_memory(
        &self,
        memory_id: &str,
        content: &str,
        sender: &str,
        channel: &str,
        tags: &[&str],
        timestamp: i64,
    ) -> Result<String> {
        let mem_nid = format!("mem:{}", memory_id);

        let mem_node = NodeBuilder::new()
            .id(&mem_nid)
            .label("Memory")
            .property("content", PropertyValue::string(truncate(content, 200)))
            .property("timestamp", PropertyValue::Integer(timestamp))
            .build();
        let _ = self.db.create_node(mem_node);

        // Sender
        let sender_nid = format!("sender:{}", sender);
        let _ = self.db.create_node(
            NodeBuilder::new().id(&sender_nid).label("Sender")
                .property("name", PropertyValue::string(sender)).build(),
        );
        let _ = self.db.create_edge(
            EdgeBuilder::new(mem_nid.clone(), sender_nid, "AUTHORED_BY").build(),
        );

        // Channel
        let channel_nid = format!("channel:{}", channel);
        let _ = self.db.create_node(
            NodeBuilder::new().id(&channel_nid).label("Channel")
                .property("name", PropertyValue::string(channel)).build(),
        );
        let _ = self.db.create_edge(
            EdgeBuilder::new(mem_nid.clone(), channel_nid, "IN_CHANNEL").build(),
        );

        // Tags
        for tag in tags {
            if tag.is_empty() { continue; }
            let tag_nid = format!("tag:{}", tag.to_lowercase());
            let _ = self.db.create_node(
                NodeBuilder::new().id(&tag_nid).label("Tag")
                    .property("name", PropertyValue::string(*tag)).build(),
            );
            let _ = self.db.create_edge(
                EdgeBuilder::new(mem_nid.clone(), tag_nid, "TAGGED").build(),
            );
        }

        // Entity extraction
        for triple in extract_triples(content) {
            let subj_nid = entity_id(&triple.subject);
            let obj_nid = entity_id(&triple.object);
            let _ = self.db.create_node(
                NodeBuilder::new().id(&subj_nid).label("Entity")
                    .property("name", PropertyValue::string(&triple.subject)).build(),
            );
            let _ = self.db.create_node(
                NodeBuilder::new().id(&obj_nid).label("Entity")
                    .property("name", PropertyValue::string(&triple.object)).build(),
            );
            let _ = self.db.create_edge(
                EdgeBuilder::new(mem_nid.clone(), subj_nid.clone(), "RELATES_TO").build(),
            );
            let _ = self.db.create_edge(
                EdgeBuilder::new(subj_nid, obj_nid, &triple.relation)
                    .property("source_memory", PropertyValue::string(&mem_nid))
                    .build(),
            );
        }

        Ok(mem_nid)
    }

    pub fn expand_context(&self, hit_memory_ids: &[String]) -> Vec<String> {
        let mut related = std::collections::HashSet::new();
        for mem_nid in hit_memory_ids {
            for edge in self.db.get_outgoing_edges(mem_nid) {
                let neighbour = &edge.to;
                if neighbour.starts_with("tag:") || neighbour.starts_with("entity:") {
                    for be in self.db.get_incoming_edges(neighbour) {
                        if be.from.starts_with("mem:") && !hit_memory_ids.contains(&be.from) {
                            related.insert(be.from.clone());
                        }
                    }
                }
            }
        }
        related.into_iter().collect()
    }

    pub fn node_count(&self) -> usize { self.db.node_count() }
    pub fn edge_count(&self) -> usize { self.db.edge_count() }

    pub fn export_subgraph(&self, limit: usize) -> (Vec<Node>, Vec<Edge>) {
        let mut seen_ids = std::collections::HashSet::new();
        let mut nodes = Vec::new();
        for label in ["Memory", "Sender", "Channel", "Tag", "Entity"] {
            for node in self.db.get_nodes_by_label(label) {
                if seen_ids.insert(node.id.clone()) {
                    nodes.push(node);
                }
            }
        }
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        if limit > 0 && nodes.len() > limit {
            nodes.truncate(limit);
        }

        let included: std::collections::HashSet<String> =
            nodes.iter().map(|n| n.id.clone()).collect();
        let mut seen_edges = std::collections::HashSet::new();
        let mut edges = Vec::new();
        for node in &nodes {
            for edge in self.db.get_outgoing_edges(&node.id) {
                if included.contains(&edge.from)
                    && included.contains(&edge.to)
                    && seen_edges.insert(edge.id.clone())
                {
                    edges.push(edge);
                }
            }
        }
        edges.sort_by(|a, b| a.id.cmp(&b.id));
        (nodes, edges)
    }
}

impl Default for KnowledgeGraph {
    fn default() -> Self { Self::new() }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn entity_id(name: &str) -> String {
    format!("entity:{}", name.to_lowercase().replace(' ', "_"))
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect::<String>() + "…"
    }
}

fn extract_triples(text: &str) -> Vec<Triple> {
    const PATTERNS: &[(&str, &str)] = &[
        (" prefers ", "PREFERS"),
        (" likes ", "LIKES"),
        (" loves ", "LOVES"),
        (" uses ", "USES"),
        (" hates ", "DISLIKES"),
        (" dislikes ", "DISLIKES"),
        (" is ", "IS_A"),
        (" has ", "HAS"),
        (" knows ", "KNOWS"),
        (" works at ", "WORKS_AT"),
        (" works for ", "WORKS_FOR"),
        (" lives in ", "LIVES_IN"),
    ];

    let mut triples = Vec::new();
    let lower = text.to_lowercase();
    for (pat, rel) in PATTERNS {
        if let Some(pos) = lower.find(pat) {
            let before = &text[..pos];
            let subject = last_n_words(before, 3);
            let after = &text[pos + pat.len()..];
            let object = first_n_words(after, 3);
            if !subject.is_empty() && !object.is_empty() {
                triples.push(Triple {
                    subject,
                    relation: rel.to_string(),
                    object,
                });
            }
        }
    }
    triples
}

fn last_n_words(s: &str, n: usize) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    let start = words.len().saturating_sub(n);
    words[start..].join(" ")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

fn first_n_words(s: &str, n: usize) -> String {
    s.split_whitespace()
        .take(n)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}
