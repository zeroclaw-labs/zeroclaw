//! Cortex-Memory backend implementation
//!
//! Provides advanced memory management with L0/L1/L2 layered architecture
//! and semantic vector search through cortex-mem-tools integration.

use super::cortex_config_resolver::{resolve_cortex_config, ResolvedCortexConfig};
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use crate::config::{Config, MemoryConfig};
use anyhow::{Context, Result};
use async_trait::async_trait;
use cortex_mem_core::FilesystemOperations;
use std::path::PathBuf;
use std::sync::Arc;

/// Cortex-Memory backend using direct crate integration
///
/// This implementation provides:
/// - L0/L1/L2 layered memory architecture
/// - Semantic vector search with Qdrant
/// - Automatic memory extraction and organization
/// - Session-scoped memory isolation
pub struct CortexMemory {
    operations: Arc<cortex_mem_tools::MemoryOperations>,
    workspace_dir: PathBuf,
    config: ResolvedCortexConfig,
}

impl CortexMemory {
    /// Create a new Cortex-Memory backend
    ///
    /// This function:
    /// 1. Resolves configuration from zeroclaw's global settings
    /// 2. Initializes LLM client for memory extraction
    /// 3. Connects to Qdrant vector database
    /// 4. Sets up MemoryOperations for high-level memory management
    pub async fn new(
        memory_config: &MemoryConfig,
        workspace_dir: PathBuf,
        zeroclaw_config: &Config,
    ) -> Result<Self> {
        // Resolve configuration (auto-derive from zeroclaw settings)
        let config = resolve_cortex_config(zeroclaw_config, memory_config, &workspace_dir)?;
        
        tracing::info!(
            "🧠 Initializing Cortex-Memory:\n\
             ├─ LLM: {} @ {}\n\
             ├─ Embedding: {} ({} dims)\n\
             ├─ Qdrant: {} / {}\n\
             └─ Tenant: {}",
            config.llm_model,
            config.llm_api_base_url,
            config.embedding_model,
            config.embedding_dimensions,
            config.qdrant_url,
            config.qdrant_collection,
            config.tenant_id,
        );
        
        // Create LLM client using cortex-mem's LLMClientImpl
        let llm_config = cortex_mem_core::llm::LLMConfig {
            api_base_url: config.llm_api_base_url.clone(),
            api_key: config.llm_api_key.clone(),
            model_efficient: config.llm_model.clone(),
            temperature: config.llm_temperature,
            max_tokens: 4096,
        };
        
        let llm_client = Arc::new(
            cortex_mem_core::llm::LLMClientImpl::new(llm_config)
                .context("Failed to create LLM client for Cortex-Memory")?,
        );
        
        // Initialize MemoryOperations
        let operations = cortex_mem_tools::MemoryOperations::new(
            &config.data_dir,
            &config.tenant_id,
            llm_client,
            &config.qdrant_url,
            &config.qdrant_collection,
            config.qdrant_api_key.as_deref(),
            &config.embedding_api_base_url,
            &config.embedding_api_key,
            &config.embedding_model,
            Some(config.embedding_dimensions),
            None, // user_id (optional)
        )
        .await
        .context("Failed to initialize Cortex-Memory operations")?;
        
        Ok(Self {
            operations: Arc::new(operations),
            workspace_dir,
            config,
        })
    }
}

#[async_trait]
impl Memory for CortexMemory {
    fn name(&self) -> &str {
        "cortex"
    }
    
    async fn store(
        &self,
        _key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let thread_id = session_id.unwrap_or("default");

        // Map category to cortex-mem scope
        let role = match category {
            MemoryCategory::Core => "system",
            MemoryCategory::Daily | MemoryCategory::Conversation => "user",
            MemoryCategory::Custom(_) => "user",
        };

        tracing::info!(
            "Cortex storing memory: key={}, thread={}, role={}, category={:?}, content_len={}",
            _key,
            thread_id,
            role,
            category,
            content.len()
        );

        // Use MemoryOperations to add message
        self.operations
            .add_message(thread_id, role, content)
            .await
            .context("Failed to store memory in Cortex-Memory")?;

        tracing::info!(
            "Cortex stored memory successfully: key={}, thread={}",
            _key,
            thread_id
        );

        Ok(())
    }
    
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        use cortex_mem_core::SearchOptions;
        
        // Build search scope URI
        let root_uri = session_id.map(|sid| format!("cortex://session/{}", sid));
        
        let options = SearchOptions {
            limit,
            threshold: 0.4, // Minimum relevance score
            root_uri,
            recursive: true,
        };
        
        // Execute semantic search
        let results = self.operations
            .vector_engine()
            .layered_semantic_search(query, &options)
            .await
            .context("Failed to search Cortex-Memory")?;
        
        // Convert to MemoryEntry
        let entries: Vec<MemoryEntry> = results
            .into_iter()
            .map(|r| MemoryEntry {
                id: r.uri.clone(),
                key: r.uri,
                content: r.content.unwrap_or_default(),
                category: MemoryCategory::Conversation,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: session_id.map(str::to_string),
                score: Some(r.score as f64),
            })
            .collect();
        
        tracing::debug!(
            "Cortex recalled {} results for query: {}",
            entries.len(),
            query
        );
        
        Ok(entries)
    }
    
    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        match self.operations.read_file(key).await {
            Ok(content) => {
                let entry = MemoryEntry {
                    id: key.to_string(),
                    key: key.to_string(),
                    content,
                    category: MemoryCategory::Conversation,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    session_id: None,
                    score: None,
                };
                Ok(Some(entry))
            }
            Err(_) => {
                // File not found or other error
                tracing::debug!("Cortex get failed for key: {}", key);
                Ok(None)
            }
        }
    }
    
    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Build scope URI based on category and session
        let scope_uri = match category {
            Some(MemoryCategory::Core) => "cortex://user".to_string(),
            _ => session_id
                .map(|sid| format!("cortex://session/{}", sid))
                .unwrap_or_else(|| "cortex://session".to_string()),
        };
        
        // List files using cortex filesystem
        let entries = self.operations
            .filesystem()
            .list(&scope_uri)
            .await
            .context("Failed to list Cortex-Memory files")?;
        
        // Convert to MemoryEntry
        let memory_entries: Vec<MemoryEntry> = entries
            .into_iter()
            .filter(|e| !e.is_directory)
            .map(|e| MemoryEntry {
                id: e.uri.clone(),
                key: e.uri,
                content: String::new(), // Content not loaded for list
                category: category.cloned().unwrap_or(MemoryCategory::Conversation),
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: session_id.map(str::to_string),
                score: None,
            })
            .collect();
        
        tracing::debug!(
            "Cortex listed {} entries (category: {:?})",
            memory_entries.len(),
            category
        );
        
        Ok(memory_entries)
    }
    
    async fn forget(&self, key: &str) -> Result<bool> {
        match self.operations.delete(key).await {
            Ok(_) => {
                tracing::debug!("Cortex forgot memory: {}", key);
                Ok(true)
            }
            Err(e) => {
                tracing::debug!("Cortex forget failed for key {}: {}", key, e);
                Ok(false)
            }
        }
    }
    
    async fn count(&self) -> Result<usize> {
        let sessions = self.operations
            .list_sessions()
            .await
            .context("Failed to count Cortex-Memory sessions")?;
        
        Ok(sessions.len())
    }
    
    async fn health_check(&self) -> bool {
        // Try a simple search to verify connection
        use cortex_mem_core::SearchOptions;
        
        self.operations
            .vector_engine()
            .semantic_search("health-check", &SearchOptions::default())
            .await
            .is_ok()
    }
}

impl std::fmt::Debug for CortexMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CortexMemory")
            .field("workspace_dir", &self.workspace_dir)
            .field("tenant_id", &self.config.tenant_id)
            .field("qdrant_collection", &self.config.qdrant_collection)
            .finish_non_exhaustive()
    }
}
