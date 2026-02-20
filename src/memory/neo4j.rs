use super::embeddings::EmbeddingProvider;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use super::vector;
use anyhow::Context;
use async_trait::async_trait;
use chrono::Local;
use neo4rs::{Graph, Node, Query, QueryTimeout};
use std::sync::Arc;
use uuid::Uuid;

pub struct Neo4jMemory {
    graph: Arc<Graph>,
    embedder: Arc<dyn EmbeddingProvider>,
    vector_weight: f32,
    keyword_weight: f32,
}

impl Neo4jMemory {
    pub async fn new(
        uri: &str,
        user: &str,
        password: &str,
        database: &str,
        embedder: Arc<dyn EmbeddingProvider>,
        vector_weight: f32,
        keyword_weight: f32,
    ) -> anyhow::Result<Self> {
        let graph = Graph::new(uri, user, password, database)
            .await
            .context("Failed to connect to Neo4j")?;

        Self::init_schema(&graph).await?;

        Ok(Self {
            graph: Arc::new(graph),
            embedder,
            vector_weight,
            keyword_weight,
        })
    }

    async fn init_schema(graph: &Graph) -> anyhow::Result<()> {
        let _ = graph
            .run(Query::new(
                "CREATE CONSTRAINT memory_key_unique IF NOT EXISTS FOR (m:Memory) REQUIRE m.key IS UNIQUE",
            ))
            .await
            .ok();

        let _ = graph
            .run(Query::new(
                "CREATE INDEX memory_category_index IF NOT EXISTS FOR (m:Memory) ON (m.category)",
            ))
            .await
            .ok();

        let _ = graph
            .run(Query::new(
                "CREATE INDEX memory_updated_at_index IF NOT EXISTS FOR (m:Memory) ON (m.updated_at)",
            ))
            .await
            .ok();

        let _ = graph
            .run(Query::new(
                "CREATE INDEX session_id_index IF NOT EXISTS FOR (s:Session) ON (s.id)",
            ))
            .await
            .ok();

        Ok(())
    }

    fn category_to_str(cat: &MemoryCategory) -> String {
        match cat {
            MemoryCategory::Core => "core".into(),
            MemoryCategory::Daily => "daily".into(),
            MemoryCategory::Conversation => "conversation".into(),
            MemoryCategory::Custom(name) => name.clone(),
        }
    }

    fn str_to_category(s: &str) -> MemoryCategory {
        match s {
            "core" => MemoryCategory::Core,
            "daily" => MemoryCategory::Daily,
            "conversation" => MemoryCategory::Conversation,
            other => MemoryCategory::Custom(other.to_string()),
        }
    }

    async fn get_or_compute_embedding(&self, text: &str) -> anyhow::Result<Option<Vec<f32>>> {
        if self.embedder.dimensions() == 0 {
            return Ok(None);
        }
        let embedding = self.embedder.embed_one(text).await?;
        Ok(Some(embedding))
    }

    fn node_to_entry(node: Node) -> anyhow::Result<MemoryEntry> {
        let id = node
            .get::<String>("id")
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let key = node.get::<String>("key").context("Missing key")?;
        let content = node.get::<String>("content").context("Missing content")?;
        let category_str = node.get::<String>("category").unwrap_or_else(|| "core".to_string());
        let timestamp = node
            .get::<String>("created_at")
            .unwrap_or_else(|| Local::now().to_rfc3339());
        let session_id: Option<String> = node.get("session_id").ok();

        Ok(MemoryEntry {
            id,
            key,
            content,
            category: Self::str_to_category(&category_str),
            timestamp,
            session_id,
            score: None,
        })
    }

    async fn search_by_keyword(
        graph: &Graph,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        let keywords: Vec<String> = query
            .split_whitespace()
            .map(|w| format!("(?i).*{}.*", regex::escape(w)))
            .collect();

        if keywords.is_empty() {
            return Ok(Vec::new());
        }

        let where_clause = if let Some(sid) = session_id {
            format!(
                "WHERE (m.key CONTAINS $keyword OR m.content CONTAINS $keyword) AND s.id = '{}'",
                sid
            )
        } else {
            "WHERE m.key CONTAINS $keyword OR m.content CONTAINS $keyword".to_string()
        };

        let sql = format!(
            "MATCH (m:Memory) {}
             OPTIONAL MATCH (m)-[:BELONGS_TO]->(s:Session)
             RETURN m.id as id, m.updated_at as updated_at
             ORDER BY m.updated_at DESC
             LIMIT {}",
            where_clause,
            limit
        );

        let mut results = Vec::new();
        for keyword in &keywords {
            let mut query = Query::new(&sql)
                .param("keyword", keyword.as_str())
                .timeout(QueryTimeout::from_secs(5));

            if let Ok(mut rows) = graph.execute(query).await {
                while let Ok(Some(row)) = rows.next().await {
                    if let (Ok(id), Ok(updated_at)) = (
                        row.get::<String>("id"),
                        row.get::<String>("updated_at"),
                    ) {
                        let score = 1.0_f32;
                        results.push((id, score));
                    }
                }
            }
        }

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.dedup_by(|a, b| a.0 == b.0);
        results.truncate(limit);

        Ok(results)
    }

    async fn search_by_embedding(
        graph: &Graph,
        query_embedding: &[f32],
        limit: usize,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        let query = Query::new(
            "MATCH (m:Memory)
             WHERE m.embedding IS NOT NULL
             RETURN m.id as id, m.embedding as embedding
             LIMIT $limit",
        )
        .param("limit", limit * 2)
        .timeout(QueryTimeout::from_secs(10));

        let mut rows = graph.execute(query).await?;
        let mut scored: Vec<(String, f32)> = Vec::new();

        while let Ok(Some(row)) = rows.next().await {
            if let (Ok(id), Ok(embedding_bytes)) = (
                row.get::<String>("id"),
                row.get::<Vec<u8>>("embedding"),
            ) {
                let emb = vector::bytes_to_vec(&embedding_bytes);
                let sim = vector::cosine_similarity(query_embedding, &emb);
                if sim > 0.0 {
                    scored.push((id, sim));
                }
            }
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored)
    }

    async fn fetch_entries_by_ids(
        graph: &Graph,
        ids: Vec<String>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "MATCH (m:Memory) WHERE m.id IN [{}] RETURN m",
            placeholders
        );

        let mut query = Query::new(&sql);
        for (i, id) in ids.iter().enumerate() {
            query = query.param(&format!("id{}", i), id.as_str());
        }

        let mut results = Vec::new();
        let mut rows = graph.execute(query).await?;

        while let Ok(Some(row)) = rows.next().await {
            if let Ok(node) = row.get::<Node>("m") {
                if let Ok(entry) = Self::node_to_entry(node) {
                    results.push(entry);
                }
            }
        }

        Ok(results)
    }
}

#[async_trait]
impl Memory for Neo4jMemory {
    fn name(&self) -> &str {
        "neo4j"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let embedding = self.get_or_compute_embedding(content).await?;
        let embedding_bytes = embedding.map(|emb| vector::vec_to_bytes(&emb));

        let now = Local::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let cat_str = Self::category_to_str(&category);

        let query = if session_id.is_some() {
            Query::new(
                r#"
                MERGE (m:Memory {key: $key})
                SET m.id = $id,
                    m.content = $content,
                    m.category = $category,
                    m.created_at = COALESCE(m.created_at, $now),
                    m.updated_at = $now,
                    m.embedding = $embedding
                WITH m
                OPTIONAL MATCH (m)-[r:BELONGS_TO]->(s:Session)
                DELETE r
                WITH m
                MERGE (s:Session {id: $session_id})
                MERGE (m)-[:BELONGS_TO]->(s)
                RETURN m
                "#,
            )
        } else {
            Query::new(
                r#"
                MERGE (m:Memory {key: $key})
                SET m.id = $id,
                    m.content = $content,
                    m.category = $category,
                    m.created_at = COALESCE(m.created_at, $now),
                    m.updated_at = $now,
                    m.embedding = $embedding
                OPTIONAL MATCH (m)-[r:BELONGS_TO]->(s:Session)
                DELETE r
                RETURN m
                "#,
            )
        };

        let mut query = query
            .param("key", key)
            .param("id", &id)
            .param("content", content)
            .param("category", &cat_str)
            .param("now", &now)
            .param("embedding", embedding_bytes);

        if let Some(sid) = session_id {
            query = query.param("session_id", sid);
        }

        self.graph.run(query).await.context("Failed to store memory")?;
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let query_embedding = self.get_or_compute_embedding(query).await?;
        let graph = self.graph.clone();

        let keyword_results = if query.trim().contains(' ') || query.len() > 3 {
            Self::search_by_keyword(&graph, query, limit * 2, session_id).await?
        } else {
            Vec::new()
        };

        let vector_results = if let Some(ref emb) = query_embedding {
            Self::search_by_embedding(&graph, emb, limit * 2, session_id).await?
        } else {
            Vec::new()
        };

        let merged = if vector_results.is_empty() && keyword_results.is_empty() {
            Vec::new()
        } else if vector_results.is_empty() {
            keyword_results
                .iter()
                .map(|(id, score)| vector::ScoredResult {
                    id: id.clone(),
                    vector_score: None,
                    keyword_score: Some(*score),
                    final_score: *score,
                })
                .collect()
        } else if keyword_results.is_empty() {
            vector_results
                .iter()
                .map(|(id, score)| vector::ScoredResult {
                    id: id.clone(),
                    vector_score: Some(*score),
                    keyword_score: None,
                    final_score: *score,
                })
                .collect()
        } else {
            vector::hybrid_merge(
                &vector_results,
                &keyword_results,
                self.vector_weight,
                self.keyword_weight,
                limit * 2,
            )
        };

        if merged.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<String> = merged.iter().map(|r| r.id.clone()).collect();
        let mut entries = Self::fetch_entries_by_ids(&graph, ids).await?;

        for (entry, scored) in entries.iter_mut().zip(merged.iter()) {
            entry.score = Some(f64::from(scored.final_score));
        }

        entries.truncate(limit);
        Ok(entries)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let query = Query::new("MATCH (m:Memory {key: $key}) RETURN m")
            .param("key", key)
            .timeout(QueryTimeout::from_secs(5));

        let mut rows = self.graph.execute(query).await?;

        if let Ok(Some(record)) = rows.next().await {
            if let Ok(node) = record.get::<Node>("m") {
                let entry = Self::node_to_entry(node)?;
                return Ok(Some(entry));
            }
        }

        Ok(None)
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let (where_clause, limit_clause) = if category.is_some() && session_id.is_some() {
            ("WHERE m.category = $category AND s.id = $session_id", "LIMIT 1000")
        } else if category.is_some() {
            ("WHERE m.category = $category", "LIMIT 1000")
        } else if session_id.is_some() {
            ("WHERE s.id = $session_id", "LIMIT 1000")
        } else {
            ("", "LIMIT 1000")
        };

        let sql = format!(
            "MATCH (m:Memory) {} OPTIONAL MATCH (m)-[:BELONGS_TO]->(s:Session) RETURN m ORDER BY m.updated_at DESC {}",
            where_clause, limit_clause
        );

        let mut query = Query::new(&sql);

        if let Some(cat) = category {
            query = query.param("category", Self::category_to_str(cat));
        }

        if let Some(sid) = session_id {
            query = query.param("session_id", sid);
        }

        let mut results = Vec::new();
        let mut rows = self.graph.execute(query).await?;

        while let Ok(Some(record)) = rows.next().await {
            if let Ok(node) = record.get::<Node>("m") {
                if let Ok(entry) = Self::node_to_entry(node) {
                    results.push(entry);
                }
            }
        }

        Ok(results)
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let query = Query::new(
            r#"
            MATCH (m:Memory {key: $key})
            DETACH DELETE m
            RETURN count(m) as deleted
            "#,
        )
        .param("key", key);

        let mut rows = self.graph.execute(query).await?;

        if let Ok(Some(record)) = rows.next().await {
            if let Ok(deleted) = record.get::<i64>("deleted") {
                return Ok(deleted > 0);
            }
        }

        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let query = Query::new("MATCH (m:Memory) RETURN count(m) as count")
            .timeout(QueryTimeout::from_secs(5));

        let mut rows = self.graph.execute(query).await?;

        if let Ok(Some(record)) = rows.next().await {
            if let Ok(count) = record.get::<i64>("count") {
                return Ok(count as usize);
            }
        }

        Ok(0)
    }

    async fn health_check(&self) -> bool {
        let query = Query::new("RETURN 1 as result").timeout(QueryTimeout::from_secs(2));
        self.graph.execute(query).await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_conversion_roundtrip() {
        let categories = [
            MemoryCategory::Core,
            MemoryCategory::Daily,
            MemoryCategory::Conversation,
            MemoryCategory::Custom("custom".into()),
        ];

        for cat in &categories {
            let str_repr = Self::category_to_str(cat);
            let back = Self::str_to_category(&str_repr);
            assert_eq!(*cat, back);
        }
    }

    #[test]
    fn category_str_conversion() {
        assert_eq!(Self::category_to_str(&MemoryCategory::Core), "core");
        assert_eq!(Self::category_to_str(&MemoryCategory::Daily), "daily");
        assert_eq!(
            Self::category_to_str(&MemoryCategory::Conversation),
            "conversation"
        );
        assert_eq!(
            Self::category_to_str(&MemoryCategory::Custom("test".into())),
            "test"
        );
    }

    #[test]
    fn str_to_category_conversion() {
        assert_eq!(Self::str_to_category("core"), MemoryCategory::Core);
        assert_eq!(Self::str_to_category("daily"), MemoryCategory::Daily);
        assert_eq!(
            Self::str_to_category("conversation"),
            MemoryCategory::Conversation
        );
        assert_eq!(
            Self::str_to_category("unknown"),
            MemoryCategory::Custom("unknown".into())
        );
    }
}
