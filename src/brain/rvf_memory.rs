//! RVF memory wrapper — manages the RVF vector store + SQLite text store.

use anyhow::Result;
use rvf_runtime::{
    RvfStore,
    options::{
        IngestResult, MetadataEntry, MetadataValue, QueryOptions, RvfOptions,
        SearchResult, WitnessConfig,
    },
};
use std::sync::Arc;
use tokio::sync::Mutex;

use super::text_store::TextStore;

pub struct RvfMemory {
    pub store: Arc<Mutex<RvfStore>>,
    pub text_store: Arc<TextStore>,
    pub branch_name: String,
}

impl RvfMemory {
    pub async fn open(rvf_path: &str, db_path: &str, branch: &str) -> Result<Self> {
        let expanded = shellexpand::tilde(rvf_path).to_string();
        let opts = RvfOptions {
            dimension: 384,
            witness: WitnessConfig {
                witness_ingest: true,
                witness_delete: true,
                witness_compact: true,
                audit_queries: false,
            },
            ..Default::default()
        };

        let path = std::path::Path::new(&expanded);
        let store = if path.exists() {
            match RvfStore::open(path) {
                Ok(s) => s,
                Err(e) => {
                    let backup = path.with_extension("rvf.corrupt");
                    let _ = std::fs::rename(path, &backup);
                    tracing::warn!(
                        error = ?e,
                        backup = ?backup,
                        "corrupt RVF store, moved to backup and created fresh"
                    );
                    RvfStore::create(path, opts)
                        .map_err(|e2| anyhow::anyhow!("{:?}", e2))?
                }
            }
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            RvfStore::create(path, opts)
                .map_err(|e| anyhow::anyhow!("{:?}", e))?
        };

        let text_store = TextStore::new(db_path)?;

        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            text_store: Arc::new(text_store),
            branch_name: branch.to_string(),
        })
    }

    pub async fn save_message(
        &self,
        message_id: &str,
        content: &str,
        embedding: Vec<f32>,
        sender: &str,
        channel: &str,
        tags: &[&str],
        timestamp: i64,
    ) -> Result<IngestResult> {
        let tags_str = tags.join(",");
        let vector_id: u64 = uuid::Uuid::new_v4().as_u128() as u64 & 0x7FFF_FFFF_FFFF_FFFF;

        self.text_store.save_text(
            message_id, vector_id, content, sender, channel, &tags_str, "chat", timestamp,
        )?;

        let mut metadata = vec![
            MetadataEntry { field_id: 1, value: MetadataValue::String(message_id.to_string()) },
            MetadataEntry { field_id: 2, value: MetadataValue::String(sender.into()) },
            MetadataEntry { field_id: 3, value: MetadataValue::String(channel.into()) },
            MetadataEntry { field_id: 4, value: MetadataValue::String(self.branch_name.clone()) },
            MetadataEntry { field_id: 5, value: MetadataValue::I64(timestamp) },
            MetadataEntry { field_id: 6, value: MetadataValue::String("chat".into()) },
        ];
        for tag in tags {
            metadata.push(MetadataEntry {
                field_id: 7,
                value: MetadataValue::String(tag.to_string()),
            });
        }

        let mut store = self.store.lock().await;
        let result = store
            .ingest_batch(&[&embedding], &[vector_id], Some(&metadata))
            .map_err(|e| anyhow::anyhow!("{:?}", e))?;
        Ok(result)
    }

    pub async fn recall(
        &self,
        query_embedding: Vec<f32>,
        k: usize,
    ) -> Result<Vec<(SearchResult, Option<String>)>> {
        let opts = QueryOptions {
            ef_search: 200,
            ..Default::default()
        };

        let store = self.store.lock().await;
        let results = store
            .query(&query_embedding, k, &opts)
            .map_err(|e| anyhow::anyhow!("{:?}", e))?;

        let mut hydrated = Vec::new();
        for result in results {
            let text = self.text_store.get_text_by_vector_id(result.id).unwrap_or(None);
            hydrated.push((result, text));
        }
        Ok(hydrated)
    }

    pub async fn compact(&self) -> Result<()> {
        let mut store = self.store.lock().await;
        store.compact().map_err(|e| anyhow::anyhow!("{:?}", e))?;
        Ok(())
    }
}
