//! PostgreSQL-backed knowledge graph with optional vector similarity.
//! Feature-gated behind `memory-postgres`. Uses ordinary PostgreSQL tables and
//! queries rather than requiring the AGE extension.

use super::postgres::{quote_identifier, validate_identifier};
use anyhow::Result;
use parking_lot::Mutex;
use postgres::{Client, Row};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Semaphore;

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
    schema_ident: String,
    operation_gate: Arc<Semaphore>,
}

async fn run_bounded<F, T>(operation_gate: Arc<Semaphore>, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let permit = operation_gate.acquire_owned().await.map_err(|error| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "pg knowledge graph operation gate closed"
        );
        anyhow::Error::msg(format!("pg knowledge graph operation gate closed: {error}"))
    })?;

    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|error| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "pg knowledge graph blocking task terminated unexpectedly"
        );
        anyhow::Error::msg(format!(
            "pg knowledge graph blocking task terminated unexpectedly: {error}"
        ))
    })?
}

fn validated_schema_identifier(schema: &str) -> Result<String> {
    validate_identifier(schema, "knowledge graph schema")?;
    Ok(quote_identifier(schema))
}

impl PgKnowledgeGraph {
    pub fn new(client: Arc<Mutex<Client>>, schema: &str) -> Result<Self> {
        let graph = Self {
            client,
            schema_ident: validated_schema_identifier(schema)?,
            operation_gate: Arc::new(Semaphore::new(1)),
        };
        graph.init_schema_sync()?;
        Ok(graph)
    }

    fn init_schema_sync(&self) -> Result<()> {
        let mut client = self.client.lock();
        let schema = &self.schema_ident;
        client.batch_execute(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS {schema}.kg_nodes (
                id BIGSERIAL PRIMARY KEY,
                name TEXT NOT NULL,
                node_type TEXT NOT NULL,
                content TEXT NOT NULL DEFAULT '',
                tags TEXT[] NOT NULL DEFAULT '{{}}'::TEXT[],
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_kg_nodes_type ON {schema}.kg_nodes(node_type);
            CREATE INDEX IF NOT EXISTS idx_kg_nodes_tags ON {schema}.kg_nodes USING gin(tags);
            CREATE INDEX IF NOT EXISTS idx_kg_nodes_fts ON {schema}.kg_nodes
                USING gin(to_tsvector('simple', name || ' ' || content));
            CREATE TABLE IF NOT EXISTS {schema}.kg_edges (
                id BIGSERIAL PRIMARY KEY,
                source_id BIGINT NOT NULL REFERENCES {schema}.kg_nodes(id) ON DELETE CASCADE,
                target_id BIGINT NOT NULL REFERENCES {schema}.kg_nodes(id) ON DELETE CASCADE,
                relation TEXT NOT NULL,
                weight DOUBLE PRECISION NOT NULL DEFAULT 1.0,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE INDEX IF NOT EXISTS idx_kg_edges_source ON {schema}.kg_edges(source_id);
            CREATE INDEX IF NOT EXISTS idx_kg_edges_target ON {schema}.kg_edges(target_id);
            "#
        ))?;
        Ok(())
    }

    fn node_type_str(nt: &NodeType) -> &'static str {
        nt.as_str()
    }

    fn parse_node_type(s: &str) -> Result<NodeType> {
        NodeType::parse(s)
    }

    fn relation_str(r: &Relation) -> &'static str {
        r.as_str()
    }

    #[cfg(test)]
    fn parse_relation(s: &str) -> Result<Relation> {
        Relation::parse(s)
    }

    fn row_to_node(row: &Row) -> Result<PgNode> {
        Ok(PgNode {
            id: row.get(0),
            name: row.get(1),
            node_type: Self::parse_node_type(&row.get::<_, String>(2))?,
            content: row.get(3),
            tags: row.get(4),
        })
    }

    fn similarity_sql(schema: &str) -> String {
        format!(
            "SELECT id, name, node_type, content, tags \
             FROM {schema}.kg_nodes \
             WHERE to_tsvector('simple', name || ' ' || content) \
                 @@ plainto_tsquery('simple', $1) \
             ORDER BY ts_rank_cd(\
                 to_tsvector('simple', name || ' ' || content), \
                 plainto_tsquery('simple', $1)\
             ) DESC, id ASC \
             LIMIT $2"
        )
    }

    fn subgraph_step_sql(schema: &str) -> String {
        format!(
            "SELECT n.id, n.name, n.node_type, n.content, n.tags \
             FROM {schema}.kg_nodes n \
             JOIN (\
                 SELECT DISTINCT target_id \
                 FROM {schema}.kg_edges \
                 WHERE source_id = ANY($1) \
                   AND NOT (target_id = ANY($2))\
             ) next ON n.id = next.target_id \
             ORDER BY n.id ASC"
        )
    }

    pub async fn add_node(
        &self,
        name: &str,
        node_type: NodeType,
        content: &str,
        tags: &[String],
    ) -> Result<i64> {
        let client = self.client.clone();
        let schema = self.schema_ident.clone();
        let operation_gate = self.operation_gate.clone();
        let name = name.to_string();
        let nt = Self::node_type_str(&node_type).to_string();
        let content = content.to_string();
        let tags = tags.to_vec();
        run_bounded(operation_gate, move || {
            let mut client = client.lock();
            let row = client.query_one(&format!("INSERT INTO {schema}.kg_nodes (name, node_type, content, tags) VALUES ($1, $2, $3, $4) RETURNING id"), &[&name, &nt, &content, &tags])?;
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
        let schema = self.schema_ident.clone();
        let operation_gate = self.operation_gate.clone();
        let rel = Self::relation_str(&relation).to_string();
        run_bounded(operation_gate, move || {
            let mut client = client.lock();
            let row = client.query_one(&format!("INSERT INTO {schema}.kg_edges (source_id, target_id, relation, weight) VALUES ($1, $2, $3, $4) RETURNING id"), &[&source_id, &target_id, &rel, &weight])?;
            Ok(row.get(0))
        }).await
    }

    pub async fn get_node(&self, id: i64) -> Result<Option<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema_ident.clone();
        let operation_gate = self.operation_gate.clone();
        run_bounded(operation_gate, move || {
            let mut client = client.lock();
            let row = client.query_opt(
                &format!(
                    "SELECT id, name, node_type, content, tags FROM {schema}.kg_nodes WHERE id = $1"
                ),
                &[&id],
            )?;
            row.as_ref().map(Self::row_to_node).transpose()
        })
        .await
    }

    pub async fn query_by_tags(&self, tags: &[String], limit: usize) -> Result<Vec<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema_ident.clone();
        let operation_gate = self.operation_gate.clone();
        let tags = tags.to_vec();
        #[allow(clippy::cast_possible_wrap)]
        let limit = limit as i64;
        run_bounded(operation_gate, move || {
            let mut client = client.lock();
            let rows = client.query(&format!("SELECT id, name, node_type, content, tags FROM {schema}.kg_nodes WHERE tags && $1 LIMIT $2"), &[&tags, &limit])?;
            rows.iter().map(Self::row_to_node).collect()
        }).await
    }

    pub async fn query_by_similarity(&self, query: &str, limit: usize) -> Result<Vec<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema_ident.clone();
        let operation_gate = self.operation_gate.clone();
        let query = query.to_string();
        #[allow(clippy::cast_possible_wrap)]
        let limit = limit as i64;
        run_bounded(operation_gate, move || {
            let mut client = client.lock();
            let rows = client.query(&Self::similarity_sql(&schema), &[&query, &limit])?;
            rows.iter().map(Self::row_to_node).collect()
        })
        .await
    }

    pub async fn find_related(&self, node_id: i64, limit: usize) -> Result<Vec<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema_ident.clone();
        let operation_gate = self.operation_gate.clone();
        #[allow(clippy::cast_possible_wrap)]
        let limit = limit as i64;
        run_bounded(operation_gate, move || {
            let mut client = client.lock();
            let rows = client.query(&format!("SELECT n.id, n.name, n.node_type, n.content, n.tags FROM {schema}.kg_nodes n JOIN {schema}.kg_edges e ON n.id = e.target_id WHERE e.source_id = $1 UNION SELECT n.id, n.name, n.node_type, n.content, n.tags FROM {schema}.kg_nodes n JOIN {schema}.kg_edges e ON n.id = e.source_id WHERE e.target_id = $1 LIMIT $2"), &[&node_id, &limit])?;
            rows.iter().map(Self::row_to_node).collect()
        }).await
    }

    pub async fn get_subgraph(&self, root_id: i64, max_depth: u32) -> Result<Vec<PgNode>> {
        let client = self.client.clone();
        let schema = self.schema_ident.clone();
        let operation_gate = self.operation_gate.clone();
        run_bounded(operation_gate, move || {
            let mut client = client.lock();
            let Some(root) = client.query_opt(
                &format!(
                    "SELECT id, name, node_type, content, tags FROM {schema}.kg_nodes WHERE id = $1"
                ),
                &[&root_id],
            )?
            else {
                return Ok(Vec::new());
            };

            let mut nodes = vec![Self::row_to_node(&root)?];
            let mut visited = HashSet::from([root_id]);
            let mut visited_ids = vec![root_id];
            let mut frontier = vec![root_id];
            let step_sql = Self::subgraph_step_sql(&schema);

            for _ in 0..max_depth {
                let rows = client.query(&step_sql, &[&frontier, &visited_ids])?;
                let mut next_frontier = Vec::with_capacity(rows.len());

                for row in rows {
                    let node = Self::row_to_node(&row)?;
                    if visited.insert(node.id) {
                        next_frontier.push(node.id);
                        visited_ids.push(node.id);
                        nodes.push(node);
                    }
                }

                if next_frontier.is_empty() {
                    break;
                }
                frontier = next_frontier;
            }

            Ok(nodes)
        })
        .await
    }

    pub async fn stats(&self) -> Result<(i64, i64)> {
        let client = self.client.clone();
        let schema = self.schema_ident.clone();
        let operation_gate = self.operation_gate.clone();
        run_bounded(operation_gate, move || {
            let mut client = client.lock();
            let nc: i64 = client
                .query_one(&format!("SELECT COUNT(*) FROM {schema}.kg_nodes"), &[])?
                .get(0);
            let ec: i64 = client
                .query_one(&format!("SELECT COUNT(*) FROM {schema}.kg_edges"), &[])?
                .get(0);
            Ok((nc, ec))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use postgres::NoTls;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use tokio::sync::oneshot;

    #[test]
    fn node_type_roundtrips() {
        for nt in NodeType::ALL {
            let s = PgKnowledgeGraph::node_type_str(nt);
            assert_eq!(PgKnowledgeGraph::parse_node_type(s).unwrap(), *nt);
        }
    }

    #[test]
    fn relation_roundtrips() {
        for r in Relation::ALL {
            let s = PgKnowledgeGraph::relation_str(r);
            assert_eq!(PgKnowledgeGraph::parse_relation(s).unwrap(), *r);
        }
    }

    #[test]
    fn unknown_node_type_errors() {
        assert!(PgKnowledgeGraph::parse_node_type("nonexistent").is_err());
    }

    #[test]
    fn unknown_relation_errors() {
        assert!(PgKnowledgeGraph::parse_relation("nonexistent").is_err());
    }

    #[test]
    fn schema_identifier_is_validated_and_quoted_once() {
        assert_eq!(
            validated_schema_identifier("test_schema").unwrap(),
            "\"test_schema\""
        );
        assert!(validated_schema_identifier("bad\"schema").is_err());
    }

    #[test]
    fn similarity_orders_rank_and_id_before_limit() {
        let sql = PgKnowledgeGraph::similarity_sql("\"test_schema\"");
        let order = sql.find("ORDER BY ts_rank_cd").unwrap();
        let tie_break = sql.find("DESC, id ASC").unwrap();
        let limit = sql.find("LIMIT $2").unwrap();
        assert!(order < tie_break && tie_break < limit);
    }

    #[test]
    fn subgraph_step_deduplicates_and_excludes_visited_nodes() {
        let sql = PgKnowledgeGraph::subgraph_step_sql("\"test_schema\"");
        assert!(sql.contains("SELECT DISTINCT target_id"));
        assert!(sql.contains("source_id = ANY($1)"));
        assert!(sql.contains("NOT (target_id = ANY($2))"));
        assert!(sql.contains("ORDER BY n.id ASC"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_executor_admits_one_operation_per_graph_instance() {
        let gate = Arc::new(Semaphore::new(1));
        let second_entered = Arc::new(AtomicBool::new(false));
        let (first_started_tx, first_started_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();

        let first_gate = Arc::clone(&gate);
        let first = zeroclaw_spawn::spawn!(run_bounded(first_gate, move || {
            first_started_tx.send(()).unwrap();
            release_first_rx.recv().unwrap();
            Ok(())
        }));
        first_started_rx.await.unwrap();
        assert_eq!(gate.available_permits(), 0);

        let second_entered_in_task = second_entered.clone();
        let second_gate = Arc::clone(&gate);
        let second = zeroclaw_spawn::spawn!(run_bounded(second_gate, move || {
            second_entered_in_task.store(true, Ordering::SeqCst);
            Ok(())
        }));

        tokio::task::yield_now().await;
        assert!(!second_entered.load(Ordering::SeqCst));
        release_first_tx.send(()).unwrap();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert!(second_entered.load(Ordering::SeqCst));
        assert_eq!(gate.available_permits(), 1);
    }

    #[tokio::test]
    async fn bounded_executor_preserves_operation_errors() {
        let result: Result<()> = run_bounded(Arc::new(Semaphore::new(1)), || {
            anyhow::bail!("sentinel database operation error")
        })
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("sentinel database operation error")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_executor_holds_permit_after_awaiting_task_is_cancelled() {
        let gate = Arc::new(Semaphore::new(1));
        let (first_started_tx, first_started_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();

        let first_gate = Arc::clone(&gate);
        let first = zeroclaw_spawn::spawn!(run_bounded(first_gate, move || {
            first_started_tx.send(()).unwrap();
            release_first_rx.recv().unwrap();
            Ok(())
        }));
        first_started_rx.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(gate.available_permits(), 0);

        let second_entered = Arc::new(AtomicBool::new(false));
        let second_entered_in_task = second_entered.clone();
        let second_gate = Arc::clone(&gate);
        let second = zeroclaw_spawn::spawn!(run_bounded(second_gate, move || {
            second_entered_in_task.store(true, Ordering::SeqCst);
            Ok(())
        }));
        tokio::task::yield_now().await;
        assert!(!second_entered.load(Ordering::SeqCst));

        release_first_tx.send(()).unwrap();
        second.await.unwrap().unwrap();
        assert!(second_entered.load(Ordering::SeqCst));
        assert_eq!(gate.available_permits(), 1);
    }

    #[tokio::test]
    async fn bounded_executor_releases_permit_after_blocking_task_panics() {
        let gate = Arc::new(Semaphore::new(1));
        let error = run_bounded(gate.clone(), || -> Result<()> {
            panic!("sentinel blocking task panic")
        })
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("pg knowledge graph blocking task terminated unexpectedly")
        );
        assert_eq!(gate.available_permits(), 1);
        run_bounded(gate, || Ok(())).await.unwrap();
    }

    static TEST_SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestSchema {
        admin: Client,
        name: String,
    }

    impl TestSchema {
        fn create(database_url: &str, purpose: &str) -> Self {
            let sequence = TEST_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = format!("zc_kg_{purpose}_{}_{sequence}", std::process::id());
            let mut admin = Client::connect(database_url, NoTls).unwrap();
            admin
                .batch_execute(&format!("CREATE SCHEMA {}", quote_identifier(&name)))
                .unwrap();
            Self { admin, name }
        }
    }

    impl Drop for TestSchema {
        fn drop(&mut self) {
            let _ = self.admin.batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {} CASCADE",
                quote_identifier(&self.name)
            ));
        }
    }

    fn postgres_test_url() -> String {
        std::env::var("ZEROCLAW_TEST_POSTGRES_URL")
            .expect("set ZEROCLAW_TEST_POSTGRES_URL to run ignored PostgreSQL tests")
    }

    fn postgres_graph(database_url: &str, schema: &str) -> PgKnowledgeGraph {
        let client = Client::connect(database_url, NoTls).unwrap();
        PgKnowledgeGraph::new(Arc::new(Mutex::new(client)), schema).unwrap()
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_POSTGRES_URL"]
    fn postgres_similarity_ranks_before_limit_with_stable_ties() {
        let database_url = postgres_test_url();
        let schema = TestSchema::create(&database_url, "rank");
        let graph = postgres_graph(&database_url, &schema.name);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let first_tie = graph
                .add_node("first tie", NodeType::Pattern, "alpha", &[])
                .await
                .unwrap();
            let highest = graph
                .add_node("highest", NodeType::Pattern, "alpha alpha alpha", &[])
                .await
                .unwrap();
            let second_tie = graph
                .add_node("second tie", NodeType::Pattern, "alpha", &[])
                .await
                .unwrap();

            let result = graph.query_by_similarity("alpha", 3).await.unwrap();
            let ids: Vec<i64> = result.into_iter().map(|node| node.id).collect();
            assert_eq!(ids, vec![highest, first_tie, second_tie]);
        });
        drop(runtime);
        drop(graph);
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_POSTGRES_URL"]
    fn postgres_subgraph_is_cycle_safe_unique_and_depth_bounded() {
        let database_url = postgres_test_url();
        let schema = TestSchema::create(&database_url, "walk");
        let graph = postgres_graph(&database_url, &schema.name);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let root = graph
                .add_node("root", NodeType::Pattern, "root", &[])
                .await
                .unwrap();
            let left = graph
                .add_node("left", NodeType::Pattern, "left", &[])
                .await
                .unwrap();
            let converged = graph
                .add_node("converged", NodeType::Pattern, "converged", &[])
                .await
                .unwrap();
            let right = graph
                .add_node("right", NodeType::Pattern, "right", &[])
                .await
                .unwrap();

            graph
                .add_edge(root, left, Relation::Uses, 1.0)
                .await
                .unwrap();
            graph
                .add_edge(root, right, Relation::Uses, 1.0)
                .await
                .unwrap();
            graph
                .add_edge(left, converged, Relation::Uses, 1.0)
                .await
                .unwrap();
            graph
                .add_edge(right, converged, Relation::Uses, 1.0)
                .await
                .unwrap();
            graph
                .add_edge(converged, root, Relation::Uses, 1.0)
                .await
                .unwrap();

            let depth_zero = graph.get_subgraph(root, 0).await.unwrap();
            assert_eq!(
                depth_zero.iter().map(|node| node.id).collect::<Vec<_>>(),
                vec![root]
            );

            let depth_one = graph.get_subgraph(root, 1).await.unwrap();
            assert_eq!(
                depth_one.iter().map(|node| node.id).collect::<Vec<_>>(),
                vec![root, left, right]
            );

            let result = graph.get_subgraph(root, 3).await.unwrap();
            let ids: Vec<i64> = result.iter().map(|node| node.id).collect();
            assert_eq!(ids, vec![root, left, right, converged]);
        });
        drop(runtime);
        drop(graph);
    }
}
