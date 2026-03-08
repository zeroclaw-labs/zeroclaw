//! Cortex-Memory backend implementation
//!
//! Provides advanced memory management with L0/L1/L2 layered architecture
//! and semantic vector search through cortex-mem-tools integration.
//!
//! ## Automatic Processing
//!
//! For long-running services like zeroclaw, this backend implements:
//! - Message count threshold triggering (default: 10 messages)
//! - Periodic background flush (default: every 5 minutes)
//! - Concurrency limiting to prevent resource exhaustion
//!
//! This ensures user/agent memories are extracted and L0/L2 layers
//! are generated without requiring explicit session close calls.

use super::cortex_config_resolver::{resolve_cortex_config, ResolvedCortexConfig};
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use crate::config::{Config, MemoryConfig};
use anyhow::{Context, Result};
use async_trait::async_trait;
use cortex_mem_core::FilesystemOperations;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{RwLock, Semaphore};

// ==================== Helper Functions ====================

/// Get current Unix timestamp in seconds
///
/// Uses SystemTime for reliable wall-clock time, suitable for
/// persistent timing across process lifetime.
#[inline]
fn current_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ==================== Auto-Processing Configuration ====================

/// Configuration for automatic memory processing
///
/// This enables automatic memory extraction and layer generation
/// suitable for long-running services.
#[derive(Debug, Clone, Copy)]
pub struct AutoProcessConfig {
    /// Minimum message count before triggering processing
    pub message_count_threshold: usize,
    /// Minimum time interval between processing (in seconds)
    pub min_process_interval_secs: u64,
    /// Enable auto-trigger on message count threshold
    pub enable_threshold_trigger: bool,
    /// Interval for periodic background flush (in seconds)
    /// Set to 0 to disable periodic flush
    pub periodic_flush_interval_secs: u64,
    /// Maximum concurrent processing tasks
    /// Prevents resource exhaustion when many sessions need processing
    pub max_concurrent_processing: usize,
}

impl Default for AutoProcessConfig {
    fn default() -> Self {
        Self {
            message_count_threshold: 10,       // Trigger after n messages
            min_process_interval_secs: 600,    // At most once ever n minutes
            enable_threshold_trigger: true,
            periodic_flush_interval_secs: 600, // Periodic flush every n minutes
            max_concurrent_processing: 1,      // Max concurrent sessions
        }
    }
}

/// Session state for auto-processing tracking
#[derive(Debug)]
struct SessionState {
    /// Number of messages since last processing
    message_count: usize,
    /// Time of last processing (Instant for relative timing)
    last_processed: Option<Instant>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            message_count: 0,
            last_processed: None,
        }
    }
}

/// Cortex-Memory backend using direct crate integration
///
/// This implementation provides:
/// - L0/L1/L2 layered memory architecture
/// - Semantic vector search with Qdrant
/// - Automatic memory extraction and organization
/// - Session-scoped memory isolation
/// - Periodic background processing for long-running services
/// - Concurrency limiting to prevent resource exhaustion
pub struct CortexMemory {
    operations: Arc<cortex_mem_tools::MemoryOperations>,
    workspace_dir: PathBuf,
    config: ResolvedCortexConfig,
    /// Auto-processing configuration
    auto_process_config: AutoProcessConfig,
    /// Session states for tracking auto-processing conditions
    session_states: Arc<RwLock<HashMap<String, SessionState>>>,
    /// Last global processing time (Unix timestamp in seconds)
    last_process_time: Arc<AtomicU64>,
    /// Set of directories already indexed for L0/L1
    /// Prevents redundant vector indexing of the same directory
    indexed_dirs: Arc<RwLock<HashSet<String>>>,
    /// Semaphore for limiting concurrent processing tasks
    processing_semaphore: Arc<Semaphore>,
}

impl CortexMemory {
    /// Create a new Cortex-Memory backend
    pub async fn new(
        memory_config: &MemoryConfig,
        workspace_dir: PathBuf,
        zeroclaw_config: &Config,
    ) -> Result<Self> {
        Self::new_with_config(
            memory_config,
            workspace_dir,
            zeroclaw_config,
            AutoProcessConfig::default(),
        ).await
    }

    /// Create a new Cortex-Memory backend with custom auto-process config
    pub async fn new_with_config(
        memory_config: &MemoryConfig,
        workspace_dir: PathBuf,
        zeroclaw_config: &Config,
        auto_process_config: AutoProcessConfig,
    ) -> Result<Self> {
        // Resolve configuration (auto-derive from zeroclaw settings)
        let config = resolve_cortex_config(zeroclaw_config, memory_config, &workspace_dir)?;

        tracing::debug!(
            "Cortex-Memory initializing: LLM={} @ {}, Embedding={} ({} dims) @ {}, Qdrant={}/{}, tenant={}",
            config.llm_model,
            config.llm_api_base_url,
            config.embedding_model,
            config.embedding_dimensions,
            config.embedding_api_base_url,
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

        let processing_semaphore = Arc::new(Semaphore::new(auto_process_config.max_concurrent_processing));

        let instance = Self {
            operations: Arc::new(operations),
            workspace_dir,
            config,
            auto_process_config,
            session_states: Arc::new(RwLock::new(HashMap::new())),
            last_process_time: Arc::new(AtomicU64::new(0)),
            indexed_dirs: Arc::new(RwLock::new(HashSet::new())),
            processing_semaphore,
        };

        // Start periodic flush task if enabled
        if instance.auto_process_config.periodic_flush_interval_secs > 0 {
            instance.start_periodic_flush();
        }

        Ok(instance)
    }

    /// Start the periodic background flush task
    fn start_periodic_flush(&self) {
        let operations = self.operations.clone();
        let session_states = self.session_states.clone();
        let last_process_time = self.last_process_time.clone();
        let indexed_dirs = self.indexed_dirs.clone();
        let semaphore = self.processing_semaphore.clone();
        let interval_secs = self.auto_process_config.periodic_flush_interval_secs;
        let min_interval_secs = self.auto_process_config.min_process_interval_secs;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));

            loop {
                interval.tick().await;

                // Check if enough time has passed since last processing
                // FIX: Use SystemTime-based Unix timestamp instead of Instant::now().elapsed()
                let last = last_process_time.load(Ordering::Relaxed);
                let now = current_timestamp_secs();

                if last > 0 && now.saturating_sub(last) < min_interval_secs {
                    tracing::trace!(
                        "Skipping periodic flush: min interval not reached ({}s < {}s)",
                        now.saturating_sub(last),
                        min_interval_secs
                    );
                    continue;
                }

                // Find sessions with pending messages
                let sessions_to_process = {
                    let mut states = session_states.write().await;
                    let mut to_process = Vec::new();

                    for (session_id, state) in states.iter_mut() {
                        if state.message_count > 0 {
                            to_process.push(session_id.clone());
                            state.message_count = 0;
                            state.last_processed = Some(Instant::now());
                        }
                    }

                    to_process
                };

                if sessions_to_process.is_empty() {
                    tracing::trace!("Periodic flush: no sessions with pending messages");
                    continue;
                }

                tracing::debug!(
                    "Periodic flush: processing {} sessions",
                    sessions_to_process.len()
                );

                // FIX: Acquire semaphore permit to limit concurrency
                let permit = match semaphore.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::warn!("Semaphore closed, stopping periodic flush");
                        break;
                    }
                };

                // Update last process time before spawning
                last_process_time.store(current_timestamp_secs(), Ordering::Relaxed);

                // Spawn processing task with concurrency control
                let operations_clone = operations.clone();
                let indexed_dirs_clone = indexed_dirs.clone();
                let last_process_time_clone = last_process_time.clone();

                tokio::spawn(async move {
                    // Send SessionClosed events for each session
                    if let Some(tx) = operations_clone.memory_event_tx() {
                        use cortex_mem_core::memory_events::MemoryEvent;

                        let user_id = operations_clone.default_user_id().to_string();
                        let agent_id = operations_clone.default_agent_id().to_string();

                        for session_id in &sessions_to_process {
                            // Mark directory as being processed (avoid redundant L0/L1 indexing)
                            {
                                let mut dirs = indexed_dirs_clone.write().await;
                                dirs.insert(format!("cortex://session/{}/timeline", session_id));
                            }

                            if let Err(e) = tx.send(MemoryEvent::SessionClosed {
                                session_id: session_id.clone(),
                                user_id: user_id.clone(),
                                agent_id: agent_id.clone(),
                            }) {
                                tracing::warn!("Failed to send SessionClosed event for {}: {}", session_id, e);
                            }
                        }
                    }

                    // Wait for background processing to complete
                    let completed = operations_clone.flush_and_wait(Some(1)).await;

                    // Update last process time after completion
                    last_process_time_clone.store(current_timestamp_secs(), Ordering::Relaxed);

                    if completed {
                        tracing::debug!("Periodic flush completed successfully");
                    } else {
                        tracing::warn!("Periodic flush initiated but some tasks may still be in progress");
                    }

                    // Release semaphore permit by dropping
                    drop(permit);
                });
            }
        });

        tracing::debug!(
            "Started periodic flush task (interval: {}s, max concurrent: {})",
            interval_secs,
            self.auto_process_config.max_concurrent_processing
        );
    }

    /// Check if threshold-based processing should be triggered
    async fn check_threshold_trigger(&self, thread_id: &str) -> bool {
        if !self.auto_process_config.enable_threshold_trigger {
            return false;
        }

        let mut states = self.session_states.write().await;
        let state = states.entry(thread_id.to_string()).or_default();

        // Increment message count
        state.message_count += 1;

        // Check threshold
        if state.message_count < self.auto_process_config.message_count_threshold {
            return false;
        }

        // Check minimum interval using Instant for relative timing
        if let Some(last_processed) = state.last_processed {
            let elapsed = last_processed.elapsed().as_secs();
            if elapsed < self.auto_process_config.min_process_interval_secs {
                tracing::trace!(
                    "Threshold reached but min interval not met ({}s < {}s)",
                    elapsed,
                    self.auto_process_config.min_process_interval_secs
                );
                return false;
            }
        }

        // Try to acquire semaphore permit (non-blocking)
        let permit = match self.processing_semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::debug!(
                    "Threshold trigger for session {} skipped: too many concurrent processing tasks",
                    thread_id
                );
                return false;
            }
        };

        // Trigger processing
        state.message_count = 0;
        state.last_processed = Some(Instant::now());

        // FIX: Use SystemTime-based timestamp
        self.last_process_time.store(current_timestamp_secs(), Ordering::Relaxed);

        // Send SessionClosed event
        if let Some(tx) = self.operations.memory_event_tx() {
            use cortex_mem_core::memory_events::MemoryEvent;

            let user_id = self.operations.default_user_id().to_string();
            let agent_id = self.operations.default_agent_id().to_string();

            if let Err(e) = tx.send(MemoryEvent::SessionClosed {
                session_id: thread_id.to_string(),
                user_id,
                agent_id,
            }) {
                tracing::warn!("Failed to send SessionClosed event: {}", e);
                return false;
            }

            // Mark directory as being processed
            {
                let mut dirs = self.indexed_dirs.write().await;
                dirs.insert(format!("cortex://session/{}/timeline", thread_id));
            }

            tracing::debug!(
                "Threshold-triggered processing for session {}",
                thread_id
            );

            // Spawn async task to release permit after processing
            let operations = self.operations.clone();
            let last_process_time = self.last_process_time.clone();
            tokio::spawn(async move {
                let _ = operations.flush_and_wait(Some(1)).await;
                last_process_time.store(current_timestamp_secs(), Ordering::Relaxed);
                drop(permit); // Release permit
            });

            return true;
        }

        false
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

        tracing::debug!(
            "Cortex storing memory: key={}, thread={}, role={}, content_len={}",
            _key,
            thread_id,
            role,
            content.len()
        );

        // Use MemoryOperations to add message
        let message_uri = self.operations
            .add_message(thread_id, role, content)
            .await
            .context("Failed to store memory in Cortex-Memory")?;

        // Check threshold-based trigger
        self.check_threshold_trigger(thread_id).await;

        tracing::debug!(
            "Cortex stored memory: key={}, thread={}, uri={}",
            _key,
            thread_id,
            message_uri
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
                content: String::new(),
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
