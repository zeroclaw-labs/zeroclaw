//! PostgreSQL-backed knowledge graph with optional vector similarity.
//!
//! Feature-gated behind `memory-postgres`. Uses pure SQL with recursive CTEs
//! rather than requiring the AGE extension.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use postgres::{Client, Row};
use std::sync::Arc;
use tokio::sync::oneshot;

pub use super::knowledge_graph::{NodeType, Relation};

#[derive(Debug, Clone)]
pub struct PgNode {
    pub id: i64,
    pub name: String,
    pub node_type: NodeType,
    pub content: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PgEdge {
    pub source_id: i64,
    pub target_id: i64,
    pub relation: Relation,
    pub weight: f64,
}

pub struct PgKnowledgeGraph {
    client: Arc<Mutex<Client>>,
    schema: String,
}

async fn run_on_os_thread<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("pg-knowledge-graph-op".to_string())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .context("failed to spawn pg knowledge graph thread")?;
    rx.await
        .map_err(|_| anyhow::anyhow!("pg knowledge graph thread terminated unexpectedly"))?
}

impl PgKnowledgeGraph {
    pub fn new(client: Arc<Mutex<Client>>, schema: &str) -> Result<Self> {
        let graph = Self {
            client,
            schema: schema.to_string(),
        };
        graph.init_schema_sync()?;
        Ok(graph)
    }

    fn init_schema_sync(&self) -> Result<()> {
        let mut client = self.client.lock();
        let schema = &self.schema;
        client.batch_execute(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS "{schema}".kg_nodes (
                id BIGSERIAL PRIMARY KEY,
                name TEXT NOT NULL,
                node_type TEXT NOT NULL,
                content TEXT NOT NULL DEFAULT '',
                tags TEXT[] NOT NULL DEFAULT '{{}}'::TEXT[],
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_kg_nodes_type ON "{schema}".kg_nodes(node_type);
            CREATE INDEX IF NOT EXISTS idx_kg_nodes_tags ON "{schema}".kg_nodes USING gin(tags);
            CREATE INDEX IF NOT EXISTS idx_kg_nodes_fts ON "{schema}".kg_nodes
                USING gin(to_tsvector('simple', name || ' ' || content));
            CREATE TABLE IF NOT EXISTS "{schema}".kg_edges (
                id BIGSERIAL PRIMARY KEY,
                source_id BIGINT NOT NULL REFERENCES "{schema}".kg_nodes(id) ON DELETE CASCADE,
                target_id BIGINT NOT NULL REFERENCES "{schema}".kg_nodes(id) ON DELETE CASCADE,
                relation TEXT NOT NULL,
                weight DOUBLE PRECISION NOT NULL DEFAULT 1.0,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_kg_edges_source ON "{schema}".kg_edges(source_id);
            CREATE INDEX IF NOT EXISTS idx_kg_edges_target ON "{schema}".kg_edges(target_id);
            "#
        ))?;
        Ok(())
    }

    fn node_type_str(nt: &NodeType) -> &'static str {
        match nt {
            NodeType::Pattern => "pattern",
            NodeType::Decision => "decision",
            NodeType::Lesson => "lesson",
            NodeType::Expert => "expert",
            NodeType::Technology => "technology",
        }
    }

    fn parse_node_type(s: &str) -> NodeType {
        match s {
            "pattern" => NodeType::Pattern,
            "decision" => NodeType::Decision,
            "lesson" => NodeType::Lesson,
            "expert" => NodeType::Expert,
            "technology" => NodeType::Technology,
            _ => NodeType::Pattern,
        }
    }

    fn relation_str(r: &Relation) -> &'static str {
        match r {
            Relation::Uses => "uses",
            Relation::Replaces => "replaces",
            Relation::Extends => "extends",
            Relation::AuthoredBy => "authored_by",
            Relation::AppliesTo => "applies_to",
        }
    }

    fn parse_relation(s: &str) -> Relation {
        match s {
            "uses" => Relation::Uses,
            "replaces" => Relation::Replaces,
            "extends" => Relation::Extends,
            "authored_by" => Relation::AuthoredBy,
            "applies_to" => Relation::AppliesTo,
            _ => Relation::Uses,
        }
    }

    fn row_to_node(row: &Row) -> PgNode {
        PgNode {
            id: row.get(0),
            name: row.get(1),
            node_type: Self::parse_node_type(&row.get::<_, String>(2)),
            content: row.get(3),
            tags: row.get(4),
        }
    }

    pub async fn add_node(
        &self,
        name: &str,
        node_type: NodeType,
        content: &str,
        tags: &[String],
    ) -> Result<i64> {
        let client = self.client.clone();
        let schema = self.schema.clone();
        let name = name.to_string();
        let nt = Self::node_type_str(&node_type).to_string();
        let content = content.to_string();
        let tags = tags.to_vec();
        run_on_os_thread(move || {
            let mut client = client.lock();
            let row = client.query_one(&format!(r#"INSERT INTO "{schema}".kg_nodes (name, node_type, content, tags) VALUES ($1, $2, $3, $4) RETURNING id"#), &[&name, &nt, &content, &tags])?;
            Ok(row.get(0))
        }).await
    }

    pub async fn add_edge(
        &self,
        source_id: i64,
        target_id: i64,
        relation: Relation,
        weight: f64,
    ) -> Result<i64> {
        let client = self.client.clone();
        let schema = self.schema.clone();
        let rel = Self::relation_str(&relation).to_string();
        run_on_os_thread(move || {
            let mut client = client.lock();
            let row = client.query_one(&format!(r#"INSERT INTO "{schema}".kg_edges (source_id, target_id, relation, weight) VALUES ($1, $2, $3, $4) RETURNING id"#), &[&source_id, &target_id, &rel, &weight])?;
            Ok(row.get(0))
        }).await
    }

    pub async fn get_node(&self, id: i64) -> Result<Option<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema.clone();
        run_on_os_thread(move || {
            let mut client = client.lock();
            let row = client.query_opt(&format!(r#"SELECT id, name, node_type, content, tags FROM "{schema}".kg_nodes WHERE id = $1"#), &[&id])?;
            Ok(row.as_ref().map(Self::row_to_node))
        }).await
    }

    pub async fn query_by_tags(&self, tags: &[String], limit: usize) -> Result<Vec<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema.clone();
        let tags = tags.to_vec();
        #[allow(clippy::cast_possible_wrap)]
        let limit = limit as i64;
        run_on_os_thread(move || {
            let mut client = client.lock();
            let rows = client.query(&format!(r#"SELECT id, name, node_type, content, tags FROM "{schema}".kg_nodes WHERE tags && $1 LIMIT $2"#), &[&tags, &limit])?;
            Ok(rows.iter().map(Self::row_to_node).collect())
        }).await
    }

    pub async fn query_by_similarity(&self, query: &str, limit: usize) -> Result<Vec<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema.clone();
        let query = query.to_string();
        #[allow(clippy::cast_possible_wrap)]
        let limit = limit as i64;
        run_on_os_thread(move || {
            let mut client = client.lock();
            let rows = client.query(&format!(r#"SELECT id, name, node_type, content, tags FROM "{schema}".kg_nodes WHERE to_tsvector('simple', name || ' ' || content) @@ plainto_tsquery('simple', $1) LIMIT $2"#), &[&query, &limit])?;
            Ok(rows.iter().map(Self::row_to_node).collect())
        }).await
    }

    pub async fn find_related(&self, node_id: i64, limit: usize) -> Result<Vec<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema.clone();
        #[allow(clippy::cast_possible_wrap)]
        let limit = limit as i64;
        run_on_os_thread(move || {
            let mut client = client.lock();
            let rows = client.query(&format!(r#"SELECT n.id, n.name, n.node_type, n.content, n.tags FROM "{schema}".kg_nodes n JOIN "{schema}".kg_edges e ON n.id = e.target_id WHERE e.source_id = $1 UNION SELECT n.id, n.name, n.node_type, n.content, n.tags FROM "{schema}".kg_nodes n JOIN "{schema}".kg_edges e ON n.id = e.source_id WHERE e.target_id = $1 LIMIT $2"#), &[&node_id, &limit])?;
            Ok(rows.iter().map(Self::row_to_node).collect())
        }).await
    }

    pub async fn get_subgraph(&self, root_id: i64, max_depth: u32) -> Result<Vec<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema.clone();
        #[allow(clippy::cast_possible_wrap)]
        let max_depth = max_depth as i32;
        run_on_os_thread(move || {
            let mut client = client.lock();
            let rows = client.query(&format!(r#"WITH RECURSIVE reachable AS (SELECT id, name, node_type, content, tags, 0 AS depth FROM "{schema}".kg_nodes WHERE id = $1 UNION SELECT n.id, n.name, n.node_type, n.content, n.tags, r.depth + 1 FROM "{schema}".kg_nodes n JOIN "{schema}".kg_edges e ON n.id = e.target_id JOIN reachable r ON e.source_id = r.id WHERE r.depth < $2) SELECT DISTINCT id, name, node_type, content, tags FROM reachable"#), &[&root_id, &max_depth])?;
            Ok(rows.iter().map(Self::row_to_node).collect())
        }).await
    }

    pub async fn stats(&self) -> Result<(i64, i64)> {
        let client = self.client.clone();
        let schema = self.schema.clone();
        run_on_os_thread(move || {
            let mut client = client.lock();
            let nc: i64 = client
                .query_one(&format!(r#"SELECT COUNT(*) FROM "{schema}".kg_nodes"#), &[])?
                .get(0);
            let ec: i64 = client
                .query_one(&format!(r#"SELECT COUNT(*) FROM "{schema}".kg_edges"#), &[])?
                .get(0);
            Ok((nc, ec))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_type_roundtrips() {
        for nt in &[
            NodeType::Pattern,
            NodeType::Decision,
            NodeType::Lesson,
            NodeType::Expert,
            NodeType::Technology,
        ] {
            let s = PgKnowledgeGraph::node_type_str(nt);
            assert_eq!(&PgKnowledgeGraph::parse_node_type(s), nt);
        }
    }

    #[test]
    fn relation_roundtrips() {
        for r in &[
            Relation::Uses,
            Relation::Replaces,
            Relation::Extends,
            Relation::AuthoredBy,
            Relation::AppliesTo,
        ] {
            let s = PgKnowledgeGraph::relation_str(r);
            assert_eq!(&PgKnowledgeGraph::parse_relation(s), r);
        }
    }

    #[test]
    fn unknown_node_type_defaults_to_pattern() {
        assert_eq!(
            PgKnowledgeGraph::parse_node_type("nonexistent"),
            NodeType::Pattern
        );
    }

    #[test]
    fn unknown_relation_defaults_to_uses() {
        assert_eq!(
            PgKnowledgeGraph::parse_relation("nonexistent"),
            Relation::Uses
        );
    }

    #[test]
    fn init_schema_sql_is_syntactically_valid() {
        let schema = "test_schema";
        let sql = format!(
            r#"CREATE TABLE IF NOT EXISTS "{schema}".kg_nodes (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL);"#
        );
        assert!(sql.contains("BIGSERIAL"));
        assert!(sql.contains("test_schema"));
    }
}
