//! Knowledge graph for capturing, organizing, and reusing expertise.
//!
//! SQLite-backed storage for knowledge nodes (patterns, decisions, lessons,
//! experts, technologies) and directed edges (uses, replaces, extends,
//! authored_by, applies_to). Supports full-text search, tag filtering,
//! and relation traversal.

use anyhow::Context;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ── Domain types ────────────────────────────────────────────────

/// The kind of knowledge captured in a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    Pattern,
    Decision,
    Lesson,
    Expert,
    Technology,
}

impl NodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Decision => "decision",
            Self::Lesson => "lesson",
            Self::Expert => "expert",
            Self::Technology => "technology",
        }
    }

    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "pattern" => Ok(Self::Pattern),
            "decision" => Ok(Self::Decision),
            "lesson" => Ok(Self::Lesson),
            "expert" => Ok(Self::Expert),
            "technology" => Ok(Self::Technology),
            other => anyhow::bail!("unknown node type: {other}"),
        }
    }
}

/// Directed relationship between two knowledge nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Uses,
    Replaces,
    Extends,
    AuthoredBy,
    AppliesTo,
}

impl Relation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uses => "uses",
            Self::Replaces => "replaces",
            Self::Extends => "extends",
            Self::AuthoredBy => "authored_by",
            Self::AppliesTo => "applies_to",
        }
    }

    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "uses" => Ok(Self::Uses),
            "replaces" => Ok(Self::Replaces),
            "extends" => Ok(Self::Extends),
            "authored_by" => Ok(Self::AuthoredBy),
            "applies_to" => Ok(Self::AppliesTo),
            other => anyhow::bail!("unknown relation: {other}"),
        }
    }
}

/// A node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeNode {
    pub id: String,
    pub node_type: NodeType,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_project: Option<String>,
}

/// A directed edge in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEdge {
    pub from_id: String,
    pub to_id: String,
    pub relation: Relation,
}

/// A search result with relevance score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub node: KnowledgeNode,
    pub score: f64,
}

/// Summary statistics for the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub nodes_by_type: HashMap<String, usize>,
    pub top_tags: Vec<(String, usize)>,
}

// ── Knowledge graph ─────────────────────────────────────────────

/// SQLite-backed knowledge graph.
pub struct KnowledgeGraph {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
    max_nodes: usize,
}

impl KnowledgeGraph {
    /// Open (or create) a knowledge graph database at the given path.
    pub fn new(db_path: &Path, max_nodes: usize) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path).context("failed to open knowledge graph database")?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS nodes (
                id TEXT PRIMARY KEY,
                node_type TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                source_project TEXT
            );

            CREATE TABLE IF NOT EXISTS edges (
                from_id TEXT NOT NULL,
                to_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                PRIMARY KEY (from_id, to_id, relation),
                FOREIGN KEY (from_id) REFERENCES nodes(id) ON DELETE CASCADE,
                FOREIGN KEY (to_id) REFERENCES nodes(id) ON DELETE CASCADE
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
                title, content, tags, content='nodes', content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
                INSERT INTO nodes_fts(rowid, title, content, tags)
                VALUES (new.rowid, new.title, new.content, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
                INSERT INTO nodes_fts(nodes_fts, rowid, title, content, tags)
                VALUES ('delete', old.rowid, old.title, old.content, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
                INSERT INTO nodes_fts(nodes_fts, rowid, title, content, tags)
                VALUES ('delete', old.rowid, old.title, old.content, old.tags);
                INSERT INTO nodes_fts(rowid, title, content, tags)
                VALUES (new.rowid, new.title, new.content, new.tags);
            END;

            CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(node_type);
            CREATE INDEX IF NOT EXISTS idx_nodes_source ON nodes(source_project);
            CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_id);
            CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_id);",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
            db_path: db_path.to_path_buf(),
            max_nodes,
        })
    }

    /// Add a node to the graph. Returns the generated node id.
    pub fn add_node(
        &self,
        node_type: NodeType,
        title: &str,
        content: &str,
        tags: &[String],
        source_project: Option<&str>,
    ) -> anyhow::Result<String> {
        let conn = self.conn.lock();

        // Enforce max_nodes limit.
        let count: usize = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        if count >= self.max_nodes {
            anyhow::bail!(
                "knowledge graph node limit reached ({}/{})",
                count,
                self.max_nodes
            );
        }

        // Reject tags containing commas since comma is the separator in storage.
        for tag in tags {
            if tag.contains(',') {
                anyhow::bail!(
                    "tag '{}' contains a comma, which is used as the tag separator",
                    tag
                );
            }
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tags_str = tags.join(",");

        conn.execute(
            "INSERT INTO nodes (id, node_type, title, content, tags, created_at, updated_at, source_project)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                node_type.as_str(),
                title,
                content,
                tags_str,
                now,
                now,
                source_project,
            ],
        )?;

        Ok(id)
    }

    /// Add a directed edge between two nodes.
    pub fn add_edge(&self, from_id: &str, to_id: &str, relation: Relation) -> anyhow::Result<()> {
        let conn = self.conn.lock();

        // Verify both endpoints exist.
        let exists = |id: &str| -> anyhow::Result<bool> {
            let c: usize = conn.query_row(
                "SELECT COUNT(*) FROM nodes WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )?;
            Ok(c > 0)
        };

        if !exists(from_id)? {
            anyhow::bail!("source node not found: {from_id}");
        }
        if !exists(to_id)? {
            anyhow::bail!("target node not found: {to_id}");
        }

        conn.execute(
            "INSERT OR IGNORE INTO edges (from_id, to_id, relation) VALUES (?1, ?2, ?3)",
            params![from_id, to_id, relation.as_str()],
        )?;

        Ok(())
    }

    /// Retrieve a node by id.
    pub fn get_node(&self, id: &str) -> anyhow::Result<Option<KnowledgeNode>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, node_type, title, content, tags, created_at, updated_at, source_project
             FROM nodes WHERE id = ?1",
        )?;

        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_node(row)?)),
            None => Ok(None),
        }
    }

    /// Query nodes by tags (all listed tags must be present).
    pub fn query_by_tags(&self, tags: &[String]) -> anyhow::Result<Vec<KnowledgeNode>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, node_type, title, content, tags, created_at, updated_at, source_project
             FROM nodes ORDER BY updated_at DESC",
        )?;

        let mut results = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            if tags.iter().all(|t| node.tags.contains(t)) {
                results.push(node);
            }
        }
        Ok(results)
    }

    /// Full-text search across node titles, content, and tags.
    pub fn query_by_similarity(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let conn = self.conn.lock();

        // Sanitize FTS query: escape double quotes, wrap tokens in quotes.
        let sanitized: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");

        if sanitized.is_empty() {
            return Ok(Vec::new());
        }

        let mut stmt = conn.prepare(
            "SELECT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project,
                    rank
             FROM nodes_fts f
             JOIN nodes n ON n.rowid = f.rowid
             WHERE nodes_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;

        let mut results = Vec::new();
        let mut rows = stmt.query(params![sanitized, limit as i64])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            let rank: f64 = row.get(8)?;
            results.push(SearchResult {
                node,
                score: -rank, // FTS5 rank is negative (lower = better), invert for intuitive scoring
            });
        }
        Ok(results)
    }

    /// Find nodes directly related to the given node (both outbound and inbound edges).
    pub fn find_related(&self, node_id: &str) -> anyhow::Result<Vec<(KnowledgeNode, Relation)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project,
                    e.relation
             FROM edges e
             JOIN nodes n ON n.id = e.to_id
             WHERE e.from_id = ?1
             UNION ALL
             SELECT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project,
                    e.relation
             FROM edges e
             JOIN nodes n ON n.id = e.from_id
             WHERE e.to_id = ?1",
        )?;

        let mut results = Vec::new();
        let mut rows = stmt.query(params![node_id])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            let relation_str: String = row.get(8)?;
            let relation = Relation::parse(&relation_str)?;
            results.push((node, relation));
        }
        Ok(results)
    }

    /// Maximum allowed subgraph traversal depth.
    const MAX_SUBGRAPH_DEPTH: usize = 100;

    /// Extract a subgraph starting from `root_id` up to `depth` hops.
    ///
    /// `depth` must be between 1 and `MAX_SUBGRAPH_DEPTH` (100).
    /// Uses a recursive CTE for efficient single-query bidirectional traversal.
    pub fn get_subgraph(
        &self,
        root_id: &str,
        depth: usize,
    ) -> anyhow::Result<(Vec<KnowledgeNode>, Vec<KnowledgeEdge>)> {
        if depth == 0 {
            anyhow::bail!("subgraph depth must be greater than 0");
        }
        let depth = depth.min(Self::MAX_SUBGRAPH_DEPTH);
        let conn = self.conn.lock();

        // Collect reachable node IDs via recursive CTE (bidirectional traversal).
        let mut node_stmt = conn.prepare(
            "WITH RECURSIVE reachable(id, depth) AS (
                SELECT ?1, 0
                UNION
                SELECT CASE WHEN e.from_id = r.id THEN e.to_id ELSE e.from_id END, r.depth + 1
                FROM reachable r
                JOIN edges e ON e.from_id = r.id OR e.to_id = r.id
                WHERE r.depth < ?2
             )
             SELECT DISTINCT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project
             FROM reachable rc
             JOIN nodes n ON n.id = rc.id",
        )?;

        let mut nodes = Vec::new();
        let mut node_ids: HashSet<String> = HashSet::new();
        let mut rows = node_stmt.query(params![root_id, depth as i64])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            node_ids.insert(node.id.clone());
            nodes.push(node);
        }
        drop(rows);

        // Collect all edges where both endpoints are in the subgraph.
        let mut edge_stmt = conn.prepare("SELECT from_id, to_id, relation FROM edges")?;

        let mut edges = Vec::new();
        let mut edge_rows = edge_stmt.query([])?;
        while let Some(row) = edge_rows.next()? {
            let from_id: String = row.get(0)?;
            let to_id: String = row.get(1)?;
            if node_ids.contains(&from_id) && node_ids.contains(&to_id) {
                let relation_str: String = row.get(2)?;
                let relation = Relation::parse(&relation_str)?;
                edges.push(KnowledgeEdge {
                    from_id,
                    to_id,
                    relation,
                });
            }
        }

        Ok((nodes, edges))
    }

    /// Find experts associated with the given tags via `authored_by` edges.
    pub fn find_experts(&self, tags: &[String]) -> anyhow::Result<Vec<SearchResult>> {
        // Find nodes matching the tags, then follow authored_by edges to experts.
        let matching = self.query_by_tags(tags)?;
        let mut expert_scores: HashMap<String, f64> = HashMap::new();

        let conn = self.conn.lock();
        for node in &matching {
            let mut stmt = conn.prepare(
                "SELECT to_id FROM edges WHERE from_id = ?1 AND relation = 'authored_by'",
            )?;
            let mut rows = stmt.query(params![node.id])?;
            while let Some(row) = rows.next()? {
                let expert_id: String = row.get(0)?;
                *expert_scores.entry(expert_id).or_default() += 1.0;
            }
        }
        drop(conn);

        let mut results: Vec<SearchResult> = Vec::new();
        for (eid, score) in expert_scores {
            if let Some(node) = self.get_node(&eid)?
                && node.node_type == NodeType::Expert
            {
                results.push(SearchResult { node, score });
            }
        }

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(results)
    }

    /// Return summary statistics for the graph.
    pub fn stats(&self) -> anyhow::Result<GraphStats> {
        let conn = self.conn.lock();

        let total_nodes: usize = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        let total_edges: usize = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;

        let mut by_type = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT node_type, COUNT(*) FROM nodes GROUP BY node_type")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let t: String = row.get(0)?;
                let c: usize = row.get(1)?;
                by_type.insert(t, c);
            }
        }

        // Top 10 tags by frequency.
        let mut tag_counts: HashMap<String, usize> = HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT tags FROM nodes WHERE tags != ''")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let tags_str: String = row.get(0)?;
                for tag in tags_str.split(',') {
                    let tag = tag.trim();
                    if !tag.is_empty() {
                        *tag_counts.entry(tag.to_string()).or_default() += 1;
                    }
                }
            }
        }
        let mut top_tags: Vec<(String, usize)> = tag_counts.into_iter().collect();
        top_tags.sort_by_key(|tag| std::cmp::Reverse(tag.1));
        top_tags.truncate(10);

        Ok(GraphStats {
            total_nodes,
            total_edges,
            nodes_by_type: by_type,
            top_tags,
        })
    }
}

/// Parse a database row into a `KnowledgeNode`.
fn row_to_node(row: &rusqlite::Row<'_>) -> anyhow::Result<KnowledgeNode> {
    let id: String = row.get(0)?;
    let node_type_str: String = row.get(1)?;
    let title: String = row.get(2)?;
    let content: String = row.get(3)?;
    let tags_str: String = row.get(4)?;
    let created_at_str: String = row.get(5)?;
    let updated_at_str: String = row.get(6)?;
    let source_project: Option<String> = row.get(7)?;

    let tags: Vec<String> = tags_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let created_at = DateTime::parse_from_rfc3339(&created_at_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    Ok(KnowledgeNode {
        id,
        node_type: NodeType::parse(&node_type_str)?,
        title,
        content,
        tags,
        created_at,
        updated_at,
        source_project,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_graph() -> (TempDir, KnowledgeGraph) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        let graph = KnowledgeGraph::new(&db_path, 1000).unwrap();
        (tmp, graph)
    }

    #[test]
    fn add_node_returns_unique_id() {
        let (_tmp, graph) = test_graph();
        let id1 = graph
            .add_node(
                NodeType::Pattern,
                "Caching",
                "Use Redis for caching",
                &["redis".into()],
                None,
            )
            .unwrap();
        let id2 = graph
            .add_node(NodeType::Lesson, "Lesson A", "Content A", &[], None)
            .unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn get_node_returns_stored_data() {
        let (_tmp, graph) = test_graph();
        let id = graph
            .add_node(
                NodeType::Decision,
                "Use Postgres",
                "Chose Postgres over MySQL",
                &["database".into(), "postgres".into()],
                Some("project_alpha"),
            )
            .unwrap();

        let node = graph.get_node(&id).unwrap().unwrap();
        assert_eq!(node.title, "Use Postgres");
        assert_eq!(node.node_type, NodeType::Decision);
        assert_eq!(node.tags, vec!["database", "postgres"]);
        assert_eq!(node.source_project.as_deref(), Some("project_alpha"));
    }

    #[test]
    fn get_node_missing_returns_none() {
        let (_tmp, graph) = test_graph();
        assert!(graph.get_node("nonexistent").unwrap().is_none());
    }

    #[test]
    fn add_edge_creates_relationship() {
        let (_tmp, graph) = test_graph();
        let id1 = graph
            .add_node(NodeType::Pattern, "P1", "Pattern one", &[], None)
            .unwrap();
        let id2 = graph
            .add_node(NodeType::Technology, "T1", "Tech one", &[], None)
            .unwrap();

        graph.add_edge(&id1, &id2, Relation::Uses).unwrap();

        // Outbound: from id1 → id2
        let related = graph.find_related(&id1).unwrap();
        assert!(
            related
                .iter()
                .any(|(n, r)| n.id == id2 && *r == Relation::Uses)
        );

        // Inbound: id2 sees id1 via the same edge
        let related = graph.find_related(&id2).unwrap();
        assert!(
            related
                .iter()
                .any(|(n, r)| n.id == id1 && *r == Relation::Uses)
        );
    }

    #[test]
    fn add_edge_rejects_missing_node() {
        let (_tmp, graph) = test_graph();
        let id = graph
            .add_node(NodeType::Lesson, "L1", "Lesson", &[], None)
            .unwrap();
        let err = graph
            .add_edge(&id, "nonexistent", Relation::Extends)
            .unwrap_err();
        assert!(err.to_string().contains("target node not found"));
    }

    #[test]
    fn query_by_tags_filters_correctly() {
        let (_tmp, graph) = test_graph();
        graph
            .add_node(
                NodeType::Pattern,
                "P1",
                "Content",
                &["rust".into(), "async".into()],
                None,
            )
            .unwrap();
        graph
            .add_node(NodeType::Pattern, "P2", "Content", &["rust".into()], None)
            .unwrap();
        graph
            .add_node(NodeType::Pattern, "P3", "Content", &["python".into()], None)
            .unwrap();

        let results = graph.query_by_tags(&["rust".into()]).unwrap();
        assert_eq!(results.len(), 2);

        let results = graph
            .query_by_tags(&["rust".into(), "async".into()])
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "P1");
    }

    #[test]
    fn query_by_similarity_returns_ranked_results() {
        let (_tmp, graph) = test_graph();
        graph
            .add_node(
                NodeType::Decision,
                "Choose Rust for performance",
                "Rust gives memory safety and speed",
                &["rust".into()],
                None,
            )
            .unwrap();
        graph
            .add_node(
                NodeType::Lesson,
                "Python scaling issues",
                "Python had GIL bottleneck",
                &["python".into()],
                None,
            )
            .unwrap();

        let results = graph.query_by_similarity("Rust performance", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn subgraph_traversal_collects_connected_nodes() {
        let (_tmp, graph) = test_graph();
        let a = graph
            .add_node(NodeType::Pattern, "A", "Node A", &[], None)
            .unwrap();
        let b = graph
            .add_node(NodeType::Pattern, "B", "Node B", &[], None)
            .unwrap();
        let c = graph
            .add_node(NodeType::Pattern, "C", "Node C", &[], None)
            .unwrap();
        graph.add_edge(&a, &b, Relation::Extends).unwrap();
        graph.add_edge(&b, &c, Relation::Uses).unwrap();

        // Forward traversal from A reaches all 3 nodes.
        let (nodes, edges) = graph.get_subgraph(&a, 2).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);

        // Bidirectional: starting from C with depth 2 also reaches A.
        let (nodes, edges) = graph.get_subgraph(&c, 2).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn expert_ranking_by_authored_contributions() {
        let (_tmp, graph) = test_graph();
        let expert = graph
            .add_node(
                NodeType::Expert,
                "zeroclaw_user",
                "Backend expert",
                &[],
                None,
            )
            .unwrap();
        let p1 = graph
            .add_node(
                NodeType::Pattern,
                "Cache pattern",
                "Redis caching",
                &["caching".into()],
                None,
            )
            .unwrap();
        let p2 = graph
            .add_node(
                NodeType::Pattern,
                "Queue pattern",
                "Message queue",
                &["caching".into()],
                None,
            )
            .unwrap();

        graph.add_edge(&p1, &expert, Relation::AuthoredBy).unwrap();
        graph.add_edge(&p2, &expert, Relation::AuthoredBy).unwrap();

        let experts = graph.find_experts(&["caching".into()]).unwrap();
        assert_eq!(experts.len(), 1);
        assert_eq!(experts[0].node.title, "zeroclaw_user");
        assert!((experts[0].score - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn max_nodes_limit_enforced() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        let graph = KnowledgeGraph::new(&db_path, 2).unwrap();

        graph
            .add_node(NodeType::Lesson, "L1", "C1", &[], None)
            .unwrap();
        graph
            .add_node(NodeType::Lesson, "L2", "C2", &[], None)
            .unwrap();
        let err = graph
            .add_node(NodeType::Lesson, "L3", "C3", &[], None)
            .unwrap_err();
        assert!(err.to_string().contains("node limit reached"));
    }

    #[test]
    fn stats_reports_correct_counts() {
        let (_tmp, graph) = test_graph();
        graph
            .add_node(NodeType::Pattern, "P", "C", &["rust".into()], None)
            .unwrap();
        graph
            .add_node(
                NodeType::Lesson,
                "L",
                "C",
                &["rust".into(), "async".into()],
                None,
            )
            .unwrap();

        let stats = graph.stats().unwrap();
        assert_eq!(stats.total_nodes, 2);
        assert_eq!(stats.nodes_by_type.get("pattern"), Some(&1));
        assert_eq!(stats.nodes_by_type.get("lesson"), Some(&1));
        assert!(!stats.top_tags.is_empty());
    }

    #[test]
    fn node_type_roundtrip() {
        for nt in &[
            NodeType::Pattern,
            NodeType::Decision,
            NodeType::Lesson,
            NodeType::Expert,
            NodeType::Technology,
        ] {
            assert_eq!(&NodeType::parse(nt.as_str()).unwrap(), nt);
        }
    }

    #[test]
    fn relation_roundtrip() {
        for r in &[
            Relation::Uses,
            Relation::Replaces,
            Relation::Extends,
            Relation::AuthoredBy,
            Relation::AppliesTo,
        ] {
            assert_eq!(&Relation::parse(r.as_str()).unwrap(), r);
        }
    }
}
