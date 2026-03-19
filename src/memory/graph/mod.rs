//! Graph-based memory backend using CozoDB.
//!
//! Implements the `Memory` trait with a knowledge graph storing concepts, facts,
//! episodes, entities, topics, preferences, skills, and hypotheses.
//! Uses Datalog queries for intelligent retrieval with graph traversal.

pub mod budget;
pub mod config;
pub mod emotion;
pub mod extractor;
pub mod heat;
pub mod researcher;
pub mod retriever;
pub mod schema;
pub mod synthesizer;

pub use config::GraphConfig;

#[cfg(feature = "memory-graph")]
mod backend {
    use super::budget::BudgetController;
    use super::config::GraphConfig;
    use super::emotion;
    use super::extractor;
    use super::heat;
    use super::retriever;
    use super::schema;
    use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};
    use async_trait::async_trait;
    use chrono::Utc;
    use cozo::{DbInstance, ScriptMutability};
    use std::collections::{BTreeMap, HashSet};
    use std::path::Path;
    use std::sync::Arc;

    /// Extract a `&str` from a `cozo::DataValue::Str` variant.
    fn datavalue_as_str(v: &cozo::DataValue) -> Option<&str> {
        if let cozo::DataValue::Str(s) = v {
            Some(s.as_str())
        } else {
            None
        }
    }

    /// Extract an `i64` from a `cozo::DataValue::Num(Int(_))` variant.
    fn datavalue_as_i64(v: &cozo::DataValue) -> Option<i64> {
        if let cozo::DataValue::Num(cozo::Num::Int(i)) = v {
            Some(*i)
        } else {
            None
        }
    }

    /// Convert CozoDB rows (`Vec<Vec<DataValue>>`) to `Vec<Vec<serde_json::Value>>`.
    fn convert_rows(rows: &[Vec<cozo::DataValue>]) -> Vec<Vec<serde_json::Value>> {
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|v| match v {
                        cozo::DataValue::Null => serde_json::Value::Null,
                        cozo::DataValue::Bool(b) => serde_json::json!(*b),
                        cozo::DataValue::Num(cozo::Num::Int(i)) => serde_json::json!(*i),
                        cozo::DataValue::Num(cozo::Num::Float(f)) => serde_json::json!(*f),
                        cozo::DataValue::Str(s) => serde_json::json!(s.as_str()),
                        _ => serde_json::json!(format!("{v:?}")),
                    })
                    .collect()
            })
            .collect()
    }

    /// Graph memory backend powered by CozoDB.
    pub struct GraphMemoryBackend {
        db: Arc<DbInstance>,
        config: GraphConfig,
        budget: Arc<BudgetController>,
    }

    impl GraphMemoryBackend {
        /// Create a new graph memory backend.
        ///
        /// Initializes the CozoDB database and creates the schema if needed.
        pub fn new(workspace_dir: &Path, config: GraphConfig) -> anyhow::Result<Self> {
            let db_path = workspace_dir.join(&config.db_path);

            // Ensure parent directory exists
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let db = DbInstance::new("sled", db_path.to_string_lossy().as_ref(), "")
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            // Initialize schema (idempotent — CozoDB skips existing relations)
            let init_script = schema::schema_init_script();
            if let Err(e) =
                db.run_script(init_script, BTreeMap::default(), ScriptMutability::Mutable)
            {
                // Schema might already exist, log but don't fail
                tracing::debug!("Graph schema init (may already exist): {e}");
            }

            // Create HNSW indexes
            for idx_script in schema::hnsw_index_scripts() {
                if let Err(e) =
                    db.run_script(idx_script, BTreeMap::default(), ScriptMutability::Mutable)
                {
                    tracing::debug!("HNSW index creation (may already exist): {e}");
                }
            }

            let budget = Arc::new(BudgetController::new(
                config.rem_daily_budget_tokens,
                config.daily_cost_cap_usd,
            ));

            tracing::info!(
                "🧠 Graph memory backend initialized ({})",
                db_path.display()
            );

            Ok(Self {
                db: Arc::new(db),
                config,
                budget,
            })
        }

        /// Get a reference to the underlying CozoDB instance.
        pub fn db(&self) -> &Arc<DbInstance> {
            &self.db
        }

        /// Get a reference to the budget controller.
        pub fn budget(&self) -> &Arc<BudgetController> {
            &self.budget
        }

        /// Get known concept names for entity extraction.
        fn known_concepts(&self) -> HashSet<String> {
            let query = "?[name] := *concept{name}";
            match self
                .db
                .run_script(query, BTreeMap::default(), ScriptMutability::Immutable)
            {
                Ok(result) => result
                    .rows
                    .iter()
                    .filter_map(|row| row.first().and_then(datavalue_as_str).map(String::from))
                    .collect(),
                Err(_) => HashSet::new(),
            }
        }

        /// Store content as an episode and extract entities in the background.
        fn store_episode(&self, _key: &str, content: &str, session_id: Option<&str>) {
            let now = Utc::now().to_rfc3339();
            let id = format!("ep_{}", uuid::Uuid::new_v4());
            let session = session_id.unwrap_or_default();

            // Emotional analysis
            let vad = emotion::analyze_emotion(content);

            let put_script = format!(
                r#":put episode {{
                    id: '{id}',
                    content: $content,
                    session_id: '{session}',
                    heat: {heat},
                    emotion_valence: {v},
                    emotion_arousal: {a},
                    emotion_dominance: {d},
                    last_accessed: '{now}',
                    created_at: '{now}'
                }}"#,
                id = id,
                session = session,
                heat = self.config.initial_heat,
                v = vad.valence,
                a = vad.arousal,
                d = vad.dominance,
                now = now,
            );

            let mut params = std::collections::BTreeMap::new();
            params.insert("content".to_string(), cozo::DataValue::Str(content.into()));

            if let Err(e) = self
                .db
                .run_script(&put_script, params, ScriptMutability::Mutable)
            {
                tracing::warn!("Failed to store episode: {e}");
                return;
            }

            // Phase 2: async entity extraction and linking
            let known = self.known_concepts();
            let entities = extractor::extract_entities(content, &known);
            let db = Arc::clone(&self.db);
            let config_heat = self.config.initial_heat;
            let now_clone = now.clone();
            let id_clone = id.clone();

            // Fire-and-forget entity linking
            tokio::spawn(async move {
                for entity in entities {
                    let ent_id = format!("ent_{}", entity.name.to_lowercase().replace(' ', "_"));

                    // Upsert entity
                    let upsert = format!(
                        r#":put entity {{
                            id: '{ent_id}',
                            name: $name,
                            entity_type: '{etype}',
                            heat: {heat},
                            last_accessed: '{now}',
                            created_at: '{now}'
                        }}"#,
                        ent_id = ent_id,
                        etype = entity.entity_type,
                        heat = config_heat,
                        now = now_clone,
                    );

                    let mut params = std::collections::BTreeMap::new();
                    params.insert(
                        "name".to_string(),
                        cozo::DataValue::Str(entity.name.clone().into()),
                    );

                    if let Err(e) = db.run_script(&upsert, params, ScriptMutability::Mutable) {
                        tracing::debug!("Entity upsert failed for '{}': {e}", entity.name);
                    }

                    // Link entity to episode
                    let link = format!(
                        r#":put mentioned_in {{
                            entity_id: '{ent_id}',
                            episode_id: '{ep_id}',
                            created_at: '{now}'
                        }}"#,
                        ent_id = ent_id,
                        ep_id = id_clone,
                        now = now_clone,
                    );

                    if let Err(e) =
                        db.run_script(&link, BTreeMap::default(), ScriptMutability::Mutable)
                    {
                        tracing::debug!("Entity link failed: {e}");
                    }
                }
            });
        }

        /// Store content as a fact/preference depending on category.
        fn store_core(&self, key: &str, content: &str) {
            let now = Utc::now().to_rfc3339();
            let id = format!("fact_{}", key.replace(' ', "_"));

            let put_script = format!(
                r#":put fact {{
                    id: '{id}',
                    content: $content,
                    source: 'user',
                    confidence: 1.0,
                    heat: {heat},
                    last_accessed: '{now}',
                    created_at: '{now}'
                }}"#,
                id = id,
                heat = self.config.initial_heat,
                now = now,
            );

            let mut params = std::collections::BTreeMap::new();
            params.insert("content".to_string(), cozo::DataValue::Str(content.into()));

            if let Err(e) = self
                .db
                .run_script(&put_script, params, ScriptMutability::Mutable)
            {
                tracing::warn!("Failed to store core fact '{key}': {e}");
            }
        }

        /// Store a preference.
        fn store_preference(&self, key: &str, content: &str) {
            let now = Utc::now().to_rfc3339();
            let id = format!("pref_{}", key.replace(' ', "_"));

            let put_script = format!(
                r#":put preference {{
                    id: '{id}',
                    key: $key,
                    value: $value,
                    heat: {heat},
                    last_accessed: '{now}',
                    created_at: '{now}'
                }}"#,
                id = id,
                heat = self.config.initial_heat,
                now = now,
            );

            let mut params = std::collections::BTreeMap::new();
            params.insert("key".to_string(), cozo::DataValue::Str(key.into()));
            params.insert("value".to_string(), cozo::DataValue::Str(content.into()));

            if let Err(e) = self
                .db
                .run_script(&put_script, params, ScriptMutability::Mutable)
            {
                tracing::warn!("Failed to store preference '{key}': {e}");
            }
        }
    }

    #[async_trait]
    impl Memory for GraphMemoryBackend {
        fn name(&self) -> &str {
            "graph"
        }

        async fn store(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            match &category {
                MemoryCategory::Core => {
                    if key.starts_with("pref_") || key.starts_with("preference_") {
                        self.store_preference(key, content);
                    } else {
                        self.store_core(key, content);
                    }
                }
                MemoryCategory::Conversation | MemoryCategory::Daily => {
                    self.store_episode(key, content, session_id);
                }
                MemoryCategory::Custom(cat) => {
                    if cat == "preference" {
                        self.store_preference(key, content);
                    } else {
                        self.store_episode(key, content, session_id);
                    }
                }
            }
            Ok(())
        }

        async fn recall(
            &self,
            query: &str,
            limit: usize,
            session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            let known = self.known_concepts();
            let entity_names = extractor::extract_query_terms(query, &known);

            // Build and execute the lookup query
            let datalog = retriever::build_entity_lookup_query(&entity_names);
            let result =
                self.db
                    .run_script(&datalog, BTreeMap::default(), ScriptMutability::Immutable);

            let mut entries = match result {
                Ok(result) => {
                    let json_rows = convert_rows(&result.rows);
                    retriever::rows_to_memory_entries(&json_rows, session_id)
                }
                Err(e) => {
                    tracing::warn!("Graph recall query failed: {e}");
                    Vec::new()
                }
            };

            // Supplement with hot nodes if we don't have enough results
            if entries.len() < limit {
                let hot_query = retriever::build_hot_nodes_query(self.config.hot_threshold, limit);
                if let Ok(hot_result) =
                    self.db
                        .run_script(&hot_query, BTreeMap::default(), ScriptMutability::Immutable)
                {
                    let json_rows = convert_rows(&hot_result.rows);
                    let hot_entries = retriever::rows_to_memory_entries(&json_rows, session_id);
                    let existing_ids: HashSet<String> =
                        entries.iter().map(|e| e.id.clone()).collect();
                    for entry in hot_entries {
                        if !existing_ids.contains(&entry.id) {
                            entries.push(entry);
                        }
                    }
                }
            }

            // Apply lazy heat decay on accessed nodes
            let now = Utc::now().to_rfc3339();
            for entry in &entries {
                // Reactivate accessed nodes
                let reactivate_script = format!(
                    r#"
                    ?[id, heat, last_accessed] := id = '{id}', heat = {heat}, last_accessed = '{now}'
                    :put concept {{id => heat, last_accessed}}
                    "#,
                    id = entry.id,
                    heat =
                        heat::reactivate(entry.score.unwrap_or(0.5), 0.1, self.config.initial_heat),
                    now = now,
                );
                let _ = self.db.run_script(
                    &reactivate_script,
                    BTreeMap::default(),
                    ScriptMutability::Mutable,
                );
            }

            entries.truncate(limit);
            Ok(entries)
        }

        async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            // Search across fact, preference, and concept relations
            let query = format!(
                r#"
                ?[id, content, heat, node_type, last_accessed] :=
                    *fact{{id, content, heat, last_accessed}},
                    id == '{key}',
                    node_type = 'fact'
                ?[id, content, heat, node_type, last_accessed] :=
                    *preference{{id, key: content, value: _, heat, last_accessed}},
                    id == '{key}',
                    node_type = 'preference'
                ?[id, content, heat, node_type, last_accessed] :=
                    *concept{{id, name: content, heat, last_accessed}},
                    id == '{key}',
                    node_type = 'concept'
                :limit 1
                "#,
                key = key.replace('\'', "\\'"),
            );

            match self
                .db
                .run_script(&query, BTreeMap::default(), ScriptMutability::Immutable)
            {
                Ok(result) => {
                    let json_rows = convert_rows(&result.rows);
                    let entries = retriever::rows_to_memory_entries(&json_rows, None);
                    Ok(entries.into_iter().next())
                }
                Err(e) => {
                    tracing::debug!("Graph get failed for key '{key}': {e}");
                    Ok(None)
                }
            }
        }

        async fn list(
            &self,
            category: Option<&MemoryCategory>,
            session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            let query = match category {
                Some(MemoryCategory::Core) => r#"
                    ?[id, content, heat, node_type, last_accessed] :=
                        *fact{id, content, heat, last_accessed}, node_type = 'fact'
                    ?[id, content, heat, node_type, last_accessed] :=
                        *concept{id, name: content, heat, last_accessed}, node_type = 'concept'
                    ?[id, content, heat, node_type, last_accessed] :=
                        *preference{id, key: content, heat, last_accessed}, node_type = 'preference'
                    :order -heat
                    :limit 100
                    "#
                .to_string(),
                Some(MemoryCategory::Conversation | MemoryCategory::Daily) => {
                    let session_filter = if let Some(sid) = session_id {
                        format!(", session_id == '{}'", sid.replace('\'', "\\'"))
                    } else {
                        String::new()
                    };
                    format!(
                        r#"
                        ?[id, content, heat, node_type, last_accessed] :=
                            *episode{{id, content, heat, last_accessed{session_filter}}},
                            node_type = 'episode'
                        :order -heat
                        :limit 100
                        "#
                    )
                }
                _ => r#"
                    ?[id, content, heat, node_type, last_accessed] :=
                        *concept{id, name: content, heat, last_accessed}, node_type = 'concept'
                    ?[id, content, heat, node_type, last_accessed] :=
                        *fact{id, content, heat, last_accessed}, node_type = 'fact'
                    ?[id, content, heat, node_type, last_accessed] :=
                        *episode{id, content, heat, last_accessed}, node_type = 'episode'
                    :order -heat
                    :limit 100
                    "#
                .to_string(),
            };

            match self
                .db
                .run_script(&query, BTreeMap::default(), ScriptMutability::Immutable)
            {
                Ok(result) => {
                    let json_rows = convert_rows(&result.rows);
                    Ok(retriever::rows_to_memory_entries(&json_rows, session_id))
                }
                Err(e) => {
                    tracing::warn!("Graph list query failed: {e}");
                    Ok(Vec::new())
                }
            }
        }

        async fn forget(&self, key: &str) -> anyhow::Result<bool> {
            let escaped = key.replace('\'', "\\'");
            // Try deleting from all node types
            let delete_queries = [
                format!("?[id] <- [['{escaped}']] :rm fact {{id}}"),
                format!("?[id] <- [['{escaped}']] :rm concept {{id}}"),
                format!("?[id] <- [['{escaped}']] :rm episode {{id}}"),
                format!("?[id] <- [['{escaped}']] :rm preference {{id}}"),
                format!("?[id] <- [['{escaped}']] :rm hypothesis {{id}}"),
                format!("?[id] <- [['{escaped}']] :rm entity {{id}}"),
            ];

            let mut deleted = false;
            for query in &delete_queries {
                if self
                    .db
                    .run_script(query, BTreeMap::default(), ScriptMutability::Mutable)
                    .is_ok()
                {
                    deleted = true;
                }
            }

            Ok(deleted)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            let query = schema::count_all_nodes_query();
            match self
                .db
                .run_script(query, BTreeMap::default(), ScriptMutability::Immutable)
            {
                Ok(result) => {
                    let total = result
                        .rows
                        .first()
                        .and_then(|row| row.first())
                        .and_then(datavalue_as_i64)
                        .unwrap_or(0);
                    Ok(usize::try_from(total).unwrap_or(0))
                }
                Err(e) => {
                    tracing::warn!("Graph count query failed: {e}");
                    Ok(0)
                }
            }
        }

        async fn health_check(&self) -> bool {
            self.db
                .run_script(
                    "?[x] <- [[1]]",
                    BTreeMap::default(),
                    ScriptMutability::Immutable,
                )
                .is_ok()
        }
    }
}

// Re-export the backend when the feature is enabled
#[cfg(feature = "memory-graph")]
pub use backend::GraphMemoryBackend;

// Stub when feature is disabled — factory will use fallback
#[cfg(not(feature = "memory-graph"))]
pub struct GraphMemoryBackend;

#[cfg(not(feature = "memory-graph"))]
impl GraphMemoryBackend {
    pub fn new(_workspace_dir: &std::path::Path, _config: GraphConfig) -> anyhow::Result<Self> {
        anyhow::bail!(
            "memory backend 'graph' requested but this build was compiled without `memory-graph`; rebuild with `--features memory-graph`"
        )
    }
}
