use async_trait::async_trait;
use qdrant_client::{
    Qdrant,
    qdrant::{
        Condition, CreateCollection, DeletePoints, Distance, FieldCondition, Filter, GetPoints,
        Match, PointId, PointStruct, PointsIdsList, PointsSelector, Range, ScoredPoint,
        ScrollPoints, SearchPoints, UpsertPoints, VectorParams, VectorsConfig, condition, r#match,
        point_id, points_selector, vector_output, vectors_config, vectors_output,
    },
};
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

use crate::{
    config::QdrantConfig,
    error::{Error, Result},
    types::{Filters, Memory, MemoryMetadata, ScoredMemory},
    vector_store::VectorStore,
};

/// Qdrant vector store implementation
pub struct QdrantVectorStore {
    client: Qdrant,
    collection_name: String,
    embedding_dim: Option<usize>,
}

impl QdrantVectorStore {
    /// Create a new Qdrant vector store
    ///
    /// If `embedding_dim` is set in config, this will automatically ensure
    /// the collection exists (creating it if necessary).
    ///
    /// If `tenant_id` is set in config, the collection name will be suffixed
    /// with "_<tenant_id>" for tenant isolation.
    pub async fn new(config: &QdrantConfig) -> Result<Self> {
        let client = Qdrant::from_url(&config.url)
            .api_key(
                config
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("QDRANT_API_KEY").ok()),
            )
            .build()
            .map_err(|e| Error::VectorStore(e))?;

        // Use tenant-aware collection name
        let collection_name = config.get_collection_name();

        let store = Self {
            client,
            collection_name,
            embedding_dim: config.embedding_dim,
        };

        // Auto-create collection if embedding_dim is set
        if store.embedding_dim.is_some() {
            store.ensure_collection().await?;
        }

        Ok(store)
    }

    /// Create a new Qdrant vector store with auto-detected embedding dimension
    ///
    /// Supports tenant isolation through config.tenant_id
    pub async fn new_with_llm_client(
        config: &QdrantConfig,
        _llm_client: &dyn crate::llm::LLMClient,
    ) -> Result<Self> {
        let client = Qdrant::from_url(&config.url)
            .api_key(
                config
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("QDRANT_API_KEY").ok()),
            )
            .build()
            .map_err(|e| Error::VectorStore(e))?;

        // Use tenant-aware collection name
        let collection_name = config.get_collection_name();

        let store = Self {
            client,
            collection_name,
            embedding_dim: config.embedding_dim,
        };

        // Auto-detect embedding dimension if not specified
        if store.embedding_dim.is_none() {
            info!("Auto-detecting embedding dimension...");

            // Use LLMClient's embed method if available
            // For now, we'll require embedding_dim to be set in config
            return Err(Error::Config(
                "Embedding dimension must be specified in config when using new_with_llm_client. \
                Auto-detection from LLMClient is not yet implemented."
                    .to_string(),
            ));
        }

        // Ensure collection exists with correct dimension
        store.ensure_collection().await?;

        Ok(store)
    }

    /// Ensure the collection exists, create if not
    async fn ensure_collection(&self) -> Result<()> {
        let collections = self
            .client
            .list_collections()
            .await
            .map_err(|e| Error::VectorStore(e))?;

        let collection_exists = collections
            .collections
            .iter()
            .any(|c| c.name == self.collection_name);

        if !collection_exists {
            let embedding_dim = self.embedding_dim.ok_or_else(|| {
                Error::Config(
                    "Embedding dimension not set. Use new_with_llm_client for auto-detection."
                        .to_string(),
                )
            })?;

            info!(
                "Creating collection: {} with dimension: {}",
                self.collection_name, embedding_dim
            );

            let vectors_config = VectorsConfig {
                config: Some(vectors_config::Config::Params(VectorParams {
                    size: embedding_dim as u64,
                    distance: Distance::Cosine.into(),
                    ..Default::default()
                })),
            };

            self.client
                .create_collection(CreateCollection {
                    collection_name: self.collection_name.clone(),
                    vectors_config: Some(vectors_config),
                    ..Default::default()
                })
                .await
                .map_err(|e| Error::VectorStore(e))?;

            info!("Collection created successfully: {}", self.collection_name);
        } else {
            debug!("Collection already exists: {}", self.collection_name);

            // Verify dimension compatibility if collection exists
            if let Some(expected_dim) = self.embedding_dim {
                self.verify_collection_dimension(expected_dim).await?;
            }
        }

        Ok(())
    }

    /// Verify that the existing collection has the expected dimension
    async fn verify_collection_dimension(&self, expected_dim: usize) -> Result<()> {
        let collection_info = self
            .client
            .collection_info(&self.collection_name)
            .await
            .map_err(|e| Error::VectorStore(e))?;

        if let Some(collection_config) = collection_info.result {
            if let Some(config) = collection_config.config {
                if let Some(params) = config.params {
                    if let Some(vectors_config) = params.vectors_config {
                        if let Some(vectors_config::Config::Params(vector_params)) =
                            vectors_config.config
                        {
                            let actual_dim = vector_params.size as usize;
                            if actual_dim != expected_dim {
                                return Err(Error::Config(format!(
                                    "Collection '{}' has dimension {} but expected {}. Please delete the collection or use a compatible embedding model.",
                                    self.collection_name, actual_dim, expected_dim
                                )));
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Convert Memory to Qdrant PointStruct
    fn memory_to_point(&self, memory: &Memory) -> PointStruct {
        let mut payload = HashMap::new();

        // Basic fields
        payload.insert("content".to_string(), memory.content.clone().into());
        payload.insert(
            "created_at".to_string(),
            memory.created_at.to_rfc3339().into(),
        );
        payload.insert(
            "updated_at".to_string(),
            memory.updated_at.to_rfc3339().into(),
        );

        // Numeric timestamps for efficient range filtering
        payload.insert(
            "created_at_ts".to_string(),
            (memory.created_at.timestamp_millis() as i64).into(),
        );
        payload.insert(
            "updated_at_ts".to_string(),
            (memory.updated_at.timestamp_millis() as i64).into(),
        );

        // Metadata fields
        if let Some(uri) = &memory.metadata.uri {
            payload.insert("uri".to_string(), uri.clone().into());
        }
        if let Some(user_id) = &memory.metadata.user_id {
            payload.insert("user_id".to_string(), user_id.clone().into());
        }
        if let Some(agent_id) = &memory.metadata.agent_id {
            payload.insert("agent_id".to_string(), agent_id.clone().into());
        }
        if let Some(run_id) = &memory.metadata.run_id {
            payload.insert("run_id".to_string(), run_id.clone().into());
        }
        if let Some(actor_id) = &memory.metadata.actor_id {
            payload.insert("actor_id".to_string(), actor_id.clone().into());
        }
        if let Some(role) = &memory.metadata.role {
            payload.insert("role".to_string(), role.clone().into());
        }

        // Store layer (L0, L1, L2)
        payload.insert("layer".to_string(), memory.metadata.layer.clone().into());
        payload.insert("hash".to_string(), memory.metadata.hash.clone().into());
        payload.insert(
            "importance_score".to_string(),
            memory.metadata.importance_score.into(),
        );

        // Store entities and topics as arrays
        if !memory.metadata.entities.is_empty() {
            let entities_values: Vec<qdrant_client::qdrant::Value> = memory
                .metadata
                .entities
                .iter()
                .map(|entity| entity.to_string().into())
                .collect();
            payload.insert(
                "entities".to_string(),
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::ListValue(
                        qdrant_client::qdrant::ListValue {
                            values: entities_values,
                        },
                    )),
                },
            );
        }

        if !memory.metadata.topics.is_empty() {
            let topics_values: Vec<qdrant_client::qdrant::Value> = memory
                .metadata
                .topics
                .iter()
                .map(|topic| topic.to_string().into())
                .collect();
            payload.insert(
                "topics".to_string(),
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::ListValue(
                        qdrant_client::qdrant::ListValue {
                            values: topics_values,
                        },
                    )),
                },
            );
        }

        // Custom metadata
        for (key, value) in &memory.metadata.custom {
            payload.insert(format!("custom_{}", key), value.to_string().into());
        }

        PointStruct::new(memory.id.clone(), memory.embedding.clone(), payload)
    }

    /// Convert filters to Qdrant filter
    fn filters_to_qdrant_filter(&self, filters: &Filters) -> Option<Filter> {
        let mut conditions = Vec::new();

        if let Some(user_id) = &filters.user_id {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "user_id".to_string(),
                    r#match: Some(Match {
                        match_value: Some(r#match::MatchValue::Keyword(user_id.clone())),
                    }),
                    ..Default::default()
                })),
            });
        }

        if let Some(agent_id) = &filters.agent_id {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "agent_id".to_string(),
                    r#match: Some(Match {
                        match_value: Some(r#match::MatchValue::Keyword(agent_id.clone())),
                    }),
                    ..Default::default()
                })),
            });
        }

        if let Some(run_id) = &filters.run_id {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "run_id".to_string(),
                    r#match: Some(Match {
                        match_value: Some(r#match::MatchValue::Keyword(run_id.clone())),
                    }),
                    ..Default::default()
                })),
            });
        }

        if let Some(layer) = &filters.layer {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "layer".to_string(),
                    r#match: Some(Match {
                        match_value: Some(r#match::MatchValue::Keyword(layer.clone())),
                    }),
                    ..Default::default()
                })),
            });
        }

        // Time range filters
        // NOTE: Qdrant Range filters require numeric fields, so we filter on *_ts (milliseconds since epoch)
        if let Some(created_after) = filters.created_after {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "created_at_ts".to_string(),
                    range: Some(Range {
                        gt: None,
                        gte: Some(created_after.timestamp_millis() as f64),
                        lt: None,
                        lte: None,
                    }),
                    ..Default::default()
                })),
            });
        }

        if let Some(created_before) = filters.created_before {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "created_at_ts".to_string(),
                    range: Some(Range {
                        gt: None,
                        gte: None,
                        lt: None,
                        lte: Some(created_before.timestamp_millis() as f64),
                    }),
                    ..Default::default()
                })),
            });
        }

        if let Some(updated_after) = filters.updated_after {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "updated_at_ts".to_string(),
                    range: Some(Range {
                        gt: None,
                        gte: Some(updated_after.timestamp_millis() as f64),
                        lt: None,
                        lte: None,
                    }),
                    ..Default::default()
                })),
            });
        }

        if let Some(updated_before) = filters.updated_before {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "updated_at_ts".to_string(),
                    range: Some(Range {
                        gt: None,
                        gte: None,
                        lt: None,
                        lte: Some(updated_before.timestamp_millis() as f64),
                    }),
                    ..Default::default()
                })),
            });
        }

        // Filter by topics - check if any of the requested topics are present
        if let Some(topics) = &filters.topics {
            if !topics.is_empty() {
                for topic in topics {
                    conditions.push(Condition {
                        condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                            key: "topics".to_string(),
                            r#match: Some(Match {
                                match_value: Some(r#match::MatchValue::Keyword(topic.clone())),
                            }),
                            ..Default::default()
                        })),
                    });
                }
            }
        }

        // Filter by entities - check if any of the requested entities are present
        if let Some(entities) = &filters.entities {
            if !entities.is_empty() {
                for entity in entities {
                    conditions.push(Condition {
                        condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                            key: "entities".to_string(),
                            r#match: Some(Match {
                                match_value: Some(r#match::MatchValue::Keyword(entity.clone())),
                            }),
                            ..Default::default()
                        })),
                    });
                }
            }
        }

        // Filter by importance score (salience)
        if let Some(min_importance) = filters.min_importance {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "importance_score".to_string(),
                    range: Some(Range {
                        gt: None,
                        gte: Some(min_importance as f64),
                        lt: None,
                        lte: None,
                    }),
                    ..Default::default()
                })),
            });
        }

        if let Some(max_importance) = filters.max_importance {
            conditions.push(Condition {
                condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                    key: "importance_score".to_string(),
                    range: Some(Range {
                        gt: None,
                        gte: None,
                        lt: Some(max_importance as f64),
                        lte: None,
                    }),
                    ..Default::default()
                })),
            });
        }

        // Filter by custom fields (including keywords)
        for (key, value) in &filters.custom {
            if let Some(keywords_array) = value.as_array() {
                // Handle keywords array
                let keyword_conditions: Vec<Condition> = keywords_array
                    .iter()
                    .filter_map(|kw| kw.as_str())
                    .map(|keyword| Condition {
                        condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                            key: format!("custom_{}", key),
                            r#match: Some(Match {
                                match_value: Some(r#match::MatchValue::Text(keyword.to_string())),
                            }),
                            ..Default::default()
                        })),
                    })
                    .collect();

                if !keyword_conditions.is_empty() {
                    conditions.push(Condition {
                        condition_one_of: Some(condition::ConditionOneOf::Filter(Filter {
                            should: keyword_conditions,
                            ..Default::default()
                        })),
                    });
                }
            } else if let Some(keyword_str) = value.as_str() {
                // Handle single string value
                conditions.push(Condition {
                    condition_one_of: Some(condition::ConditionOneOf::Field(FieldCondition {
                        key: format!("custom_{}", key),
                        r#match: Some(Match {
                            match_value: Some(r#match::MatchValue::Text(keyword_str.to_string())),
                        }),
                        ..Default::default()
                    })),
                });
            }
        }

        if conditions.is_empty() {
            None
        } else {
            Some(Filter {
                must: conditions,
                ..Default::default()
            })
        }
    }

    /// Convert Qdrant point to Memory
    fn point_to_memory(&self, point: &ScoredPoint) -> Result<Memory> {
        let payload = &point.payload;

        let id = match &point.id {
            Some(PointId {
                point_id_options: Some(point_id),
            }) => match point_id {
                point_id::PointIdOptions::Uuid(uuid) => uuid.clone(),
                point_id::PointIdOptions::Num(num) => num.to_string(),
            },
            _ => return Err(Error::Other("Invalid point ID".to_string())),
        };

        let content = payload
            .get("content")
            .and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.as_str()),
                _ => None,
            })
            .ok_or_else(|| Error::Other("Missing content field".to_string()))?
            .to_string();

        // Extract embedding from point vectors (VectorsOutput type from ScoredPoint)
        let embedding = point
            .vectors
            .as_ref()
            .and_then(|v| v.vectors_options.as_ref())
            .and_then(|opts| match opts {
                vectors_output::VectorsOptions::Vector(vec) => {
                    // Use the new vector enum instead of deprecated .data field
                    match &vec.vector {
                        Some(vector_output::Vector::Dense(dense)) => Some(dense.data.clone()),
                        Some(vector_output::Vector::Sparse(sparse)) => Some(sparse.values.clone()),
                        Some(vector_output::Vector::MultiDense(_)) => {
                                // For multi-dense, flatten all vectors
                                warn!("MultiDense vector not fully supported, using zero vector");
                                None
                            }
                        None => None,
                    }
                }
                vectors_output::VectorsOptions::Vectors(named) => {
                    // For named vectors, try to get the default "" vector first
                    named
                        .vectors
                        .get("")
                        .and_then(|v| match &v.vector {
                            Some(vector_output::Vector::Dense(dense)) => Some(dense.data.clone()),
                            Some(vector_output::Vector::Sparse(sparse)) => Some(sparse.values.clone()),
                            _ => None,
                        })
                        .or_else(|| {
                            // Try any other named vector
                            named.vectors.values().next().and_then(|v| match &v.vector {
                                Some(vector_output::Vector::Dense(dense)) => Some(dense.data.clone()),
                                Some(vector_output::Vector::Sparse(sparse)) => Some(sparse.values.clone()),
                                _ => None,
                            })
                        })
                }
            })
            .unwrap_or_else(|| {
                let dim = self.embedding_dim.unwrap_or(1024);
                warn!(
                    "No embedding found in point, using zero vector of dimension {}",
                    dim
                );
                vec![0.0; dim]
            });

        let created_at = payload
            .get("created_at")
            .and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.as_str()),
                _ => None,
            })
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .ok_or_else(|| Error::Other("Invalid created_at timestamp".to_string()))?;

        let updated_at = payload
            .get("updated_at")
            .and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.as_str()),
                _ => None,
            })
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .ok_or_else(|| Error::Other("Invalid updated_at timestamp".to_string()))?;

        let layer = payload
            .get("layer")
            .and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.as_str()),
                _ => None,
            })
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                // Backward compatibility: if layer not found, default to L2
                debug!("No layer found in payload, defaulting to L2");
                "L2".to_string()
            });

        let hash = payload
            .get("hash")
            .and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.as_str()),
                _ => None,
            })
            .map(|s| s.to_string())
            .unwrap_or_default();

        let mut custom = HashMap::new();
        for (key, value) in payload {
            if key.starts_with("custom_") {
                let custom_key = key.strip_prefix("custom_").unwrap().to_string();
                custom.insert(custom_key, serde_json::Value::String(value.to_string()));
            }
        }

        let metadata = MemoryMetadata {
            uri: payload.get("uri").and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.to_string()),
                _ => None,
            }),
            user_id: payload.get("user_id").and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.to_string()),
                _ => None,
            }),
            agent_id: payload.get("agent_id").and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.to_string()),
                _ => None,
            }),
            run_id: payload.get("run_id").and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.to_string()),
                _ => None,
            }),
            actor_id: payload.get("actor_id").and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.to_string()),
                _ => None,
            }),
            role: payload.get("role").and_then(|v| match v {
                qdrant_client::qdrant::Value {
                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                } => Some(s.to_string()),
                _ => None,
            }),
            layer,
            hash,
            importance_score: payload
                .get("importance_score")
                .and_then(|v| match v {
                    qdrant_client::qdrant::Value {
                        kind: Some(qdrant_client::qdrant::value::Kind::DoubleValue(d)),
                    } => Some(*d),
                    qdrant_client::qdrant::Value {
                        kind: Some(qdrant_client::qdrant::value::Kind::IntegerValue(i)),
                    } => Some(*i as f64),
                    _ => None,
                })
                .map(|f| f as f32)
                .unwrap_or(0.5),
            entities: payload
                .get("entities")
                .and_then(|v| match v {
                    qdrant_client::qdrant::Value {
                        kind: Some(qdrant_client::qdrant::value::Kind::ListValue(list)),
                    } => Some(
                        list.values
                            .iter()
                            .filter_map(|val| match val {
                                qdrant_client::qdrant::Value {
                                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                                } => Some(s.clone()),
                                _ => None,
                            })
                            .collect::<Vec<String>>(),
                    ),
                    qdrant_client::qdrant::Value {
                        kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                    } => {
                        // Backward compatibility: parse JSON string format
                        serde_json::from_str(s).ok()
                    }
                    _ => None,
                })
                .unwrap_or_default(),
            topics: payload
                .get("topics")
                .and_then(|v| match v {
                    qdrant_client::qdrant::Value {
                        kind: Some(qdrant_client::qdrant::value::Kind::ListValue(list)),
                    } => Some(
                        list.values
                            .iter()
                            .filter_map(|val| match val {
                                qdrant_client::qdrant::Value {
                                    kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                                } => Some(s.clone()),
                                _ => None,
                            })
                            .collect::<Vec<String>>(),
                    ),
                    qdrant_client::qdrant::Value {
                        kind: Some(qdrant_client::qdrant::value::Kind::StringValue(s)),
                    } => {
                        // Backward compatibility: parse JSON string format
                        serde_json::from_str(s).ok()
                    }
                    _ => None,
                })
                .unwrap_or_default(),
            custom,
        };

        Ok(Memory {
            id,
            content,
            embedding,
            metadata,
            created_at,
            updated_at,
        })
    }
}

impl Clone for QdrantVectorStore {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            collection_name: self.collection_name.clone(),
            embedding_dim: self.embedding_dim,
        }
    }
}

impl QdrantVectorStore {
    /// Get the embedding dimension
    pub fn embedding_dim(&self) -> Option<usize> {
        self.embedding_dim
    }

    /// Set the embedding dimension (used for auto-detection)
    pub fn set_embedding_dim(&mut self, dim: usize) {
        self.embedding_dim = Some(dim);
    }
}

#[async_trait]
impl VectorStore for QdrantVectorStore {
    async fn insert(&self, memory: &Memory) -> Result<()> {
        let point = self.memory_to_point(memory);

        let upsert_request = UpsertPoints {
            collection_name: self.collection_name.clone(),
            points: vec![point],
            ..Default::default()
        };

        self.client
            .upsert_points(upsert_request)
            .await
            .map_err(|e| Error::VectorStore(e))?;

        debug!("Inserted memory with ID: {}", memory.id);
        Ok(())
    }

    async fn search(
        &self,
        query_vector: &[f32],
        filters: &Filters,
        limit: usize,
    ) -> Result<Vec<ScoredMemory>> {
        self.search_with_threshold(query_vector, filters, limit, None)
            .await
    }

    /// Search with optional similarity threshold filtering
    async fn search_with_threshold(
        &self,
        query_vector: &[f32],
        filters: &Filters,
        limit: usize,
        score_threshold: Option<f32>,
    ) -> Result<Vec<ScoredMemory>> {
        let filter = self.filters_to_qdrant_filter(filters);

        let search_points = SearchPoints {
            collection_name: self.collection_name.clone(),
            vector: query_vector.to_vec(),
            limit: limit as u64,
            filter,
            with_payload: Some(true.into()),
            with_vectors: Some(true.into()),
            score_threshold: score_threshold.map(|t| t as f32), // Set score threshold if provided
            ..Default::default()
        };

        let response = self
            .client
            .search_points(search_points)
            .await
            .map_err(|e| Error::VectorStore(e))?;

        let mut results = Vec::new();
        for point in response.result {
            match self.point_to_memory(&point) {
                Ok(memory) => {
                    results.push(ScoredMemory {
                        memory,
                        score: point.score,
                    });
                }
                Err(e) => {
                    warn!("Failed to parse memory from point: {}", e);
                }
            }
        }

        debug!(
            "Found {} memories for search query with threshold {:?}",
            results.len(),
            score_threshold
        );
        Ok(results)
    }

    async fn update(&self, memory: &Memory) -> Result<()> {
        // For Qdrant, update is the same as insert (upsert)
        self.insert(memory).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let point_id = PointId {
            point_id_options: Some(point_id::PointIdOptions::Uuid(id.to_string())),
        };

        let points_selector = PointsSelector {
            points_selector_one_of: Some(points_selector::PointsSelectorOneOf::Points(
                PointsIdsList {
                    ids: vec![point_id],
                },
            )),
        };

        let delete_request = DeletePoints {
            collection_name: self.collection_name.clone(),
            points: Some(points_selector),
            ..Default::default()
        };

        self.client
            .delete_points(delete_request)
            .await
            .map_err(|e| Error::VectorStore(e))?;

        debug!("Deleted memory with ID: {}", id);
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Memory>> {
        let point_id = PointId {
            point_id_options: Some(point_id::PointIdOptions::Uuid(id.to_string())),
        };

        let get_request = GetPoints {
            collection_name: self.collection_name.clone(),
            ids: vec![point_id],
            with_payload: Some(true.into()),
            with_vectors: Some(true.into()),
            ..Default::default()
        };

        let response = self
            .client
            .get_points(get_request)
            .await
            .map_err(|e| Error::VectorStore(e))?;

        if let Some(point) = response.result.first() {
            // Convert RetrievedPoint to ScoredPoint for parsing
            let scored_point = ScoredPoint {
                id: point.id.clone(),
                payload: point.payload.clone(),
                score: 1.0, // Not relevant for get operation
                vectors: point.vectors.clone(),
                shard_key: None,
                order_value: None,
                version: 0,
            };

            match self.point_to_memory(&scored_point) {
                Ok(memory) => Ok(Some(memory)),
                Err(e) => {
                    error!("Failed to parse memory from point: {}", e);
                    Err(e)
                }
            }
        } else {
            Ok(None)
        }
    }

    async fn list(&self, filters: &Filters, limit: Option<usize>) -> Result<Vec<Memory>> {
        let filter = self.filters_to_qdrant_filter(filters);
        let limit = limit.unwrap_or(100) as u32;

        let scroll_points = ScrollPoints {
            collection_name: self.collection_name.clone(),
            filter,
            limit: Some(limit),
            with_payload: Some(true.into()),
            with_vectors: Some(true.into()),
            ..Default::default()
        };

        let response = self
            .client
            .scroll(scroll_points)
            .await
            .map_err(|e| Error::VectorStore(e))?;

        let mut results = Vec::new();
        for point in response.result {
            // Convert RetrievedPoint to ScoredPoint for parsing
            let scored_point = ScoredPoint {
                id: point.id.clone(),
                payload: point.payload.clone(),
                score: 1.0, // Not relevant for list operation
                vectors: point.vectors.clone(),
                shard_key: None,
                order_value: None,
                version: 0,
            };

            match self.point_to_memory(&scored_point) {
                Ok(memory) => results.push(memory),
                Err(e) => {
                    warn!("Failed to parse memory from point: {}", e);
                }
            }
        }

        debug!("Listed {} memories", results.len());
        Ok(results)
    }

    async fn health_check(&self) -> Result<bool> {
        match self.client.health_check().await {
            Ok(_) => Ok(true),
            Err(e) => {
                error!("Qdrant health check failed: {}", e);
                Ok(false)
            }
        }
    }

    async fn scroll_ids(&self, filters: &Filters, limit: usize) -> Result<Vec<String>> {
        let filter = self.filters_to_qdrant_filter(filters);
        let limit = limit as u32;

        let scroll_points = ScrollPoints {
            collection_name: self.collection_name.clone(),
            filter,
            limit: Some(limit),
            with_payload: Some(false.into()), // We only need IDs
            with_vectors: Some(false.into()), // No vectors needed
            ..Default::default()
        };

        let response = self
            .client
            .scroll(scroll_points)
            .await
            .map_err(|e| Error::VectorStore(e))?;

        let ids: Vec<String> = response
            .result
            .into_iter()
            .filter_map(|point| {
                point.id.and_then(|id| match id.point_id_options {
                    Some(point_id::PointIdOptions::Uuid(uuid)) => Some(uuid),
                    Some(point_id::PointIdOptions::Num(num)) => Some(num.to_string()),
                    None => None,
                })
            })
            .collect();

        debug!("Scrolled {} IDs from vector store", ids.len());
        Ok(ids)
    }
}
