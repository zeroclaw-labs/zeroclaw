use cortex_mem_core::{FilesystemOperations, SearchOptions};
use cortex_mem_tools::MemoryOperations;
use rmcp::{
    handler::server::tool::ToolRouter, handler::server::wrapper::Parameters, model::*, tool,
    tool_handler, tool_router, Json, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

// ==================== Auto-Trigger Configuration ====================

/// Configuration for automatic processing triggers
///
/// This enables MCP clients (like Zed) that don't actively call close_session
/// to still have user/agent memories extracted and indexed automatically.
#[derive(Debug, Clone, Copy)]
pub struct AutoTriggerConfig {
    /// Minimum message count before triggering processing
    pub message_count_threshold: usize,
    /// Minimum time interval between processing (in seconds)
    pub min_process_interval_secs: u64,
    /// Inactivity timeout to trigger processing (in seconds)
    /// If no new messages for this duration, trigger processing
    pub inactivity_timeout_secs: u64,
    /// Enable auto-trigger on store_memory
    pub enable_auto_trigger: bool,
}

impl Default for AutoTriggerConfig {
    fn default() -> Self {
        Self {
            message_count_threshold: 10,       // Trigger after 10 messages
            min_process_interval_secs: 300,    // At most once every 5 minutes
            inactivity_timeout_secs: 120,      // Trigger after 2 min of inactivity
            enable_auto_trigger: true,
        }
    }
}

/// Session state for auto-trigger tracking
#[derive(Debug)]
struct SessionState {
    /// Number of messages since last processing
    message_count: usize,
    /// Time of last processing
    last_processed: Option<Instant>,
    /// Time of last message
    last_message: Instant,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            message_count: 0,
            last_processed: None,
            last_message: Instant::now(),
        }
    }
}

// ==================== Tool Arguments & Results ====================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StoreMemoryArgs {
    /// Content to store
    content: String,
    /// Thread/session ID (optional, defaults to "default")
    thread_id: Option<String>,
    /// Message role: "user", "assistant", or "system"
    role: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StoreMemoryResult {
    success: bool,
    uri: String,
    message_id: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueryMemoryArgs {
    /// Search query
    query: String,
    /// Thread ID to search in (optional)
    thread_id: Option<String>,
    /// Maximum number of results (default: 10)
    limit: Option<usize>,
    /// Search scope: "session", "user", "agent" (default: "session")
    scope: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueryMemoryResult {
    success: bool,
    query: String,
    results: Vec<SearchResultItem>,
    total: usize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchResultItem {
    uri: String,
    score: f32,
    snippet: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListMemoriesArgs {
    /// URI to list (e.g., "cortex://session" or "cortex://user/preferences")
    uri: Option<String>,
    /// Maximum number of results (default: 50)
    limit: Option<usize>,
    /// Include abstracts in results
    include_abstracts: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListMemoriesResult {
    success: bool,
    uri: String,
    entries: Vec<ListEntry>,
    total: usize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListEntry {
    name: String,
    uri: String,
    is_directory: bool,
    size: Option<usize>,
    abstract_text: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetMemoryArgs {
    /// URI of the memory to retrieve
    uri: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetMemoryResult {
    success: bool,
    uri: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeleteMemoryArgs {
    /// URI of the memory to delete
    uri: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeleteMemoryResult {
    success: bool,
    uri: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetAbstractArgs {
    /// URI of the memory
    uri: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetAbstractResult {
    success: bool,
    uri: String,
    abstract_text: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetOverviewArgs {
    /// URI of the memory
    uri: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetOverviewResult {
    success: bool,
    uri: String,
    overview_text: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GenerateLayersArgs {
    /// Thread/session ID (optional, if not provided, generates for all sessions)
    thread_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GenerateLayersResult {
    success: bool,
    message: String,
    total: usize,
    generated: usize,
    failed: usize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IndexMemoriesArgs {
    /// Thread/session ID (optional, if not provided, indexes all files)
    thread_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IndexMemoriesResult {
    success: bool,
    message: String,
    total_files: usize,
    indexed_files: usize,
    skipped_files: usize,
    error_files: usize,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CloseSessionArgs {
    /// Thread/session ID to close
    thread_id: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CloseSessionResult {
    success: bool,
    thread_id: String,
    message: String,
}

// ==================== MCP Service ====================

/// MCP Service for Cortex Memory
///
/// Provides automatic triggering of memory extraction and layer generation
/// to support MCP clients that don't actively call close_session.
///
/// ## Auto-Trigger Mechanism
///
/// When `store_memory` is called, the service checks:
/// 1. Message count threshold (default: 10 messages)
/// 2. Inactivity timeout (default: 2 minutes without new messages)
///
/// If conditions are met, it sends a `SessionClosed` event to the
/// `MemoryEventCoordinator`, which handles:
/// - Memory extraction (session → user/agent memories)
/// - L0/L1 layer generation
/// - Vector indexing
#[derive(Clone)]
pub struct MemoryMcpService {
    operations: Arc<MemoryOperations>,
    tool_router: ToolRouter<Self>,
    /// Auto-trigger configuration
    auto_trigger_config: AutoTriggerConfig,
    /// Session states for tracking auto-trigger conditions
    /// Key: thread_id, Value: session state
    session_states: Arc<RwLock<std::collections::HashMap<String, SessionState>>>,
    /// Last global processing time (to prevent too frequent processing)
    last_global_process: Arc<AtomicU64>,
}

#[tool_router]
impl MemoryMcpService {
    /// Create a new MCP service with auto-trigger configuration
    pub fn with_config(operations: Arc<MemoryOperations>, config: AutoTriggerConfig) -> Self {
        Self {
            operations,
            tool_router: Self::tool_router(),
            auto_trigger_config: config,
            session_states: Arc::new(RwLock::new(std::collections::HashMap::new())),
            last_global_process: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Check if auto-trigger conditions are met and send SessionClosed event
    ///
    /// This leverages the existing MemoryEventCoordinator infrastructure,
    /// which handles memory extraction, layer generation, and vector indexing.
    async fn check_and_trigger_processing(&self, thread_id: &str) -> bool {
        if !self.auto_trigger_config.enable_auto_trigger {
            return false;
        }

        let mut states = self.session_states.write().await;
        let state = states.entry(thread_id.to_string()).or_default();

        // Update last message time
        state.last_message = Instant::now();
        state.message_count += 1;

        let should_trigger = self.should_trigger_processing_inner(state);

        if should_trigger {
            // Reset state
            state.message_count = 0;
            state.last_processed = Some(Instant::now());

            // Update global processing time
            self.last_global_process.store(
                Instant::now().elapsed().as_secs(),
                Ordering::Relaxed,
            );

            // Send SessionClosed event to MemoryEventCoordinator
            // This triggers the full processing pipeline:
            // 1. Memory extraction (session → user/agent)
            // 2. Timeline L0/L1 generation
            // 3. Vector sync
            if let Some(tx) = self.operations.memory_event_tx() {
                use cortex_mem_core::memory_events::MemoryEvent;
                
                let user_id = self.operations.default_user_id().to_string();
                let agent_id = self.operations.default_agent_id().to_string();

                let _ = tx.send(MemoryEvent::SessionClosed {
                    session_id: thread_id.to_string(),
                    user_id,
                    agent_id,
                });

                info!(
                    "🚀 Auto-triggered SessionClosed event for session {} (will process in background)",
                    thread_id
                );
            } else {
                warn!("⚠️ memory_event_tx not available, cannot auto-trigger processing");
            }

            return true;
        }

        false
    }

    /// Check if processing should be triggered based on session state
    fn should_trigger_processing_inner(&self, state: &SessionState) -> bool {
        let config = &self.auto_trigger_config;

        // Condition 1: Message count threshold
        if state.message_count >= config.message_count_threshold {
            // Check minimum interval since last processing
            if let Some(last_processed) = state.last_processed {
                let elapsed = last_processed.elapsed().as_secs();
                if elapsed < config.min_process_interval_secs {
                    debug!(
                        "Message threshold reached but min interval not met ({}s < {}s)",
                        elapsed, config.min_process_interval_secs
                    );
                    return false;
                }
            }
            info!(
                "🎯 Auto-trigger: message count {} >= threshold {}",
                state.message_count, config.message_count_threshold
            );
            return true;
        }

        false
    }

    /// Start a background task to check for inactive sessions
    /// This triggers processing when sessions become inactive
    pub fn start_inactivity_checker(&self) {
        let session_states = self.session_states.clone();
        let operations = self.operations.clone();
        let config = self.auto_trigger_config;

        tokio::spawn(async move {
            let check_interval = std::time::Duration::from_secs(30); // Check every 30 seconds
            let mut interval = tokio::time::interval(check_interval);

            loop {
                interval.tick().await;

                if !config.enable_auto_trigger {
                    continue;
                }

                let mut states = session_states.write().await;
                let mut to_process = Vec::new();

                for (thread_id, state) in states.iter_mut() {
                    // Check inactivity timeout
                    let inactive_duration = state.last_message.elapsed().as_secs();

                    if inactive_duration >= config.inactivity_timeout_secs && state.message_count > 0
                    {
                        // Check minimum interval since last processing
                        let can_process = if let Some(last_processed) = state.last_processed {
                            last_processed.elapsed().as_secs() >= config.min_process_interval_secs
                        } else {
                            true
                        };

                        if can_process {
                            info!(
                                "⏰ Session {} inactive for {}s, triggering processing",
                                thread_id, inactive_duration
                            );
                            to_process.push(thread_id.clone());
                        }
                    }
                }

                // Process inactive sessions
                for thread_id in to_process {
                    if let Some(state) = states.get_mut(&thread_id) {
                        state.message_count = 0;
                        state.last_processed = Some(Instant::now());

                        // Send SessionClosed event
                        if let Some(tx) = operations.memory_event_tx() {
                            use cortex_mem_core::memory_events::MemoryEvent;
                            
                            let user_id = operations.default_user_id().to_string();
                            let agent_id = operations.default_agent_id().to_string();

                            let _ = tx.send(MemoryEvent::SessionClosed {
                                session_id: thread_id.clone(),
                                user_id,
                                agent_id,
                            });
                        }
                    }
                }
            }
        });

        info!("⏱️ Session inactivity checker started");
    }

    #[tool(description = "Store a new memory in the cortex memory system")]
    async fn store_memory(
        &self,
        params: Parameters<StoreMemoryArgs>,
    ) -> std::result::Result<Json<StoreMemoryResult>, String> {
        debug!("store_memory called with args: {:?}", params.0);

        let thread_id = params.0.thread_id.unwrap_or_else(|| "default".to_string());
        let role = params.0.role.as_deref().unwrap_or("user");

        match self
            .operations
            .add_message(&thread_id, role, &params.0.content)
            .await
        {
            Ok(message_uri) => {
                // Extract message_id from URI (last segment without extension)
                let message_id = message_uri
                    .rsplit('/')
                    .next()
                    .and_then(|s| s.strip_suffix(".md"))
                    .unwrap_or("unknown")
                    .to_string();

                info!("Memory stored at: {}", message_uri);

                // 🔧 Auto-trigger: Check if processing should be triggered
                let triggered = self.check_and_trigger_processing(&thread_id).await;
                if triggered {
                    info!("🚀 Auto-triggered memory processing for thread {}", thread_id);
                }

                Ok(Json(StoreMemoryResult {
                    success: true,
                    uri: message_uri,
                    message_id,
                }))
            }
            Err(e) => {
                error!("Failed to store memory: {}", e);
                Err(format!("Failed to store memory: {}", e))
            }
        }
    }

    #[tool(description = "Search memories using semantic vector search")]
    async fn query_memory(
        &self,
        params: Parameters<QueryMemoryArgs>,
    ) -> std::result::Result<Json<QueryMemoryResult>, String> {
        debug!("query_memory called with args: {:?}", params.0);

        let limit = params.0.limit.unwrap_or(10);
        let scope = params.0.scope.as_deref().unwrap_or("session");

        // Build search scope URI
        let scope_uri = if let Some(ref thread_id) = params.0.thread_id {
            format!("cortex://session/{}", thread_id)
        } else {
            match scope {
                "session" => "cortex://session".to_string(),
                "user" => "cortex://user".to_string(),
                "agent" => "cortex://agent".to_string(),
                _ => "cortex://session".to_string(),
            }
        };

        // Use VectorSearchEngine for layered semantic search (L0/L1/L2)
        let options = SearchOptions {
            limit,
            threshold: 0.5, // Consistent with other usage modes
            root_uri: Some(scope_uri.clone()),
            recursive: true,
        };

        match self
            .operations
            .vector_engine()
            .layered_semantic_search(&params.0.query, &options)
            .await
        {
            Ok(results) => {
                let search_results: Vec<SearchResultItem> = results
                    .iter()
                    .map(|r| SearchResultItem {
                        uri: r.uri.clone(),
                        score: r.score,
                        snippet: r.snippet.clone(),
                    })
                    .collect();

                let total = search_results.len();
                info!("Query '{}' found {} results", params.0.query, total);

                Ok(Json(QueryMemoryResult {
                    success: true,
                    query: params.0.query.clone(),
                    results: search_results,
                    total,
                }))
            }
            Err(e) => {
                error!("Query failed: {}", e);
                Err(format!("Search failed: {}", e))
            }
        }
    }

    #[tool(description = "List memories from a specific URI path")]
    async fn list_memories(
        &self,
        params: Parameters<ListMemoriesArgs>,
    ) -> std::result::Result<Json<ListMemoriesResult>, String> {
        debug!("list_memories called with args: {:?}", params.0);

        let uri = params.0.uri.as_deref().unwrap_or("cortex://session");
        let limit = params.0.limit.unwrap_or(50);
        let include_abstracts = params.0.include_abstracts.unwrap_or(false);

        // Use filesystem to list entries
        let entries = match self.operations.filesystem().list(uri).await {
            Ok(e) => e,
            Err(e) => {
                error!("List failed: {}", e);
                return Err(format!("Failed to list: {}", e));
            }
        };

        let mut result_entries = Vec::new();

        for entry in entries.into_iter().take(limit) {
            // Skip hidden files (except layer files)
            if entry.name.starts_with('.')
                && entry.name != ".abstract.md"
                && entry.name != ".overview.md"
            {
                continue;
            }

            let abstract_text =
                if include_abstracts && !entry.is_directory {
                    self.operations
                        .get_abstract(&entry.uri)
                        .await
                        .ok()
                        .map(|a| a.abstract_text)
                } else {
                    None
                };

            result_entries.push(ListEntry {
                name: entry.name,
                uri: entry.uri,
                is_directory: entry.is_directory,
                size: Some(entry.size as usize),
                abstract_text,
            });
        }

        let total = result_entries.len();
        info!("Listed {} items at {}", total, uri);

        Ok(Json(ListMemoriesResult {
            success: true,
            uri: uri.to_string(),
            entries: result_entries,
            total,
        }))
    }

    #[tool(description = "Retrieve a specific memory by its URI")]
    async fn get_memory(
        &self,
        params: Parameters<GetMemoryArgs>,
    ) -> std::result::Result<Json<GetMemoryResult>, String> {
        debug!("get_memory called with args: {:?}", params.0);

        match self.operations.read_file(&params.0.uri).await {
            Ok(content) => {
                info!("Memory retrieved from: {}", params.0.uri);
                Ok(Json(GetMemoryResult {
                    success: true,
                    uri: params.0.uri.clone(),
                    content,
                }))
            }
            Err(e) => {
                error!("Failed to get memory: {}", e);
                Err(format!("Failed to get memory: {}", e))
            }
        }
    }

    #[tool(description = "Delete a memory by its URI")]
    async fn delete_memory(
        &self,
        params: Parameters<DeleteMemoryArgs>,
    ) -> std::result::Result<Json<DeleteMemoryResult>, String> {
        debug!("delete_memory called with args: {:?}", params.0);

        match self.operations.delete(&params.0.uri).await {
            Ok(_) => {
                info!("Memory deleted: {}", params.0.uri);
                Ok(Json(DeleteMemoryResult {
                    success: true,
                    uri: params.0.uri.clone(),
                }))
            }
            Err(e) => {
                error!("Failed to delete memory: {}", e);
                Err(format!("Failed to delete memory: {}", e))
            }
        }
    }

    #[tool(description = "Get the L0 abstract (~100 tokens, for quick relevance checking) of a memory")]
    async fn get_abstract(
        &self,
        params: Parameters<GetAbstractArgs>,
    ) -> std::result::Result<Json<GetAbstractResult>, String> {
        debug!("get_abstract called with args: {:?}", params.0);

        match self.operations.get_abstract(&params.0.uri).await {
            Ok(abstract_result) => {
                info!("Abstract retrieved for: {}", params.0.uri);
                Ok(Json(GetAbstractResult {
                    success: true,
                    uri: params.0.uri.clone(),
                    abstract_text: abstract_result.abstract_text,
                }))
            }
            Err(e) => {
                error!("Failed to get abstract: {}", e);
                Err(format!("Failed to get abstract: {}", e))
            }
        }
    }

    #[tool(description = "Get the L1 overview (~2000 tokens, for understanding core information) of a memory")]
    async fn get_overview(
        &self,
        params: Parameters<GetOverviewArgs>,
    ) -> std::result::Result<Json<GetOverviewResult>, String> {
        debug!("get_overview called with args: {:?}", params.0);

        match self.operations.get_overview(&params.0.uri).await {
            Ok(overview_result) => {
                info!("Overview retrieved for: {}", params.0.uri);
                Ok(Json(GetOverviewResult {
                    success: true,
                    uri: params.0.uri.clone(),
                    overview_text: overview_result.overview_text,
                }))
            }
            Err(e) => {
                error!("Failed to get overview: {}", e);
                Err(format!("Failed to get overview: {}", e))
            }
        }
    }

    #[tool(description = "Generate L0/L1 layer files for memories")]
    async fn generate_layers(
        &self,
        params: Parameters<GenerateLayersArgs>,
    ) -> std::result::Result<Json<GenerateLayersResult>, String> {
        debug!("generate_layers called with args: {:?}", params.0);

        // ✅ 根据thread_id参数选择不同的处理方式
        let (stats, message) = if let Some(ref thread_id) = params.0.thread_id {
            // 只生成特定session的层级文件
            match self.operations.ensure_session_layers(thread_id).await {
                Ok(stats) => {
                    let msg = format!("Generated layers for session {}", thread_id);
                    (stats, msg)
                }
                Err(e) => {
                    error!(
                        "Failed to generate layers for session {}: {}",
                        thread_id, e
                    );
                    return Err(format!("Failed to generate layers: {}", e));
                }
            }
        } else {
            // 生成所有session的层级文件
            match self.operations.ensure_all_layers().await {
                Ok(stats) => {
                    let msg = "Generated layers for all sessions".to_string();
                    (stats, msg)
                }
                Err(e) => {
                    error!("Failed to generate layers: {}", e);
                    return Err(format!("Failed to generate layers: {}", e));
                }
            }
        };

        info!(
            "{}: total={}, generated={}, failed={}",
            message, stats.total, stats.generated, stats.failed
        );

        Ok(Json(GenerateLayersResult {
            success: true,
            message,
            total: stats.total,
            generated: stats.generated,
            failed: stats.failed,
        }))
    }

    #[tool(description = "Index memories to vector database")]
    async fn index_memories(
        &self,
        params: Parameters<IndexMemoriesArgs>,
    ) -> std::result::Result<Json<IndexMemoriesResult>, String> {
        debug!("index_memories called with args: {:?}", params.0);

        // ✅ 根据thread_id参数选择不同的处理方式
        let (stats, message) = if let Some(ref thread_id) = params.0.thread_id {
            // 只索引特定session的文件
            match self.operations.index_session_files(thread_id).await {
                Ok(stats) => {
                    let msg = format!("Indexed memories for session {}", thread_id);
                    (stats, msg)
                }
                Err(e) => {
                    error!("Failed to index session {}: {}", thread_id, e);
                    return Err(format!("Failed to index memories: {}", e));
                }
            }
        } else {
            // 索引所有文件
            match self.operations.index_all_files().await {
                Ok(stats) => {
                    let msg = "Indexed all memory files".to_string();
                    (stats, msg)
                }
                Err(e) => {
                    error!("Failed to index memories: {}", e);
                    return Err(format!("Failed to index memories: {}", e));
                }
            }
        };

        info!(
            "{}: total={}, indexed={}, skipped={}, errors={}",
            message, stats.total_files, stats.indexed_files, stats.skipped_files, stats.error_files
        );

        Ok(Json(IndexMemoriesResult {
            success: true,
            message,
            total_files: stats.total_files,
            indexed_files: stats.indexed_files,
            skipped_files: stats.skipped_files,
            error_files: stats.error_files,
        }))
    }

    #[tool(description = "Close a session and wait for final processing (L0/L1 generation, memory extraction, indexing)")]
    async fn close_session(
        &self,
        params: Parameters<CloseSessionArgs>,
    ) -> std::result::Result<Json<CloseSessionResult>, String> {
        debug!("close_session called with args: {:?}", params.0);

        let thread_id = &params.0.thread_id;

        match self.operations.close_session(thread_id).await {
            Ok(_) => {
                info!(
                    "Session closed, waiting for background tasks: {}",
                    thread_id
                );

                // Wait for background memory extraction, L0/L1 generation, and indexing to complete
                let completed = self.operations.flush_and_wait(Some(1)).await;

                let message = if completed {
                    "Session closed. All background tasks (L0/L1 generation, memory extraction, indexing) completed successfully.".to_string()
                } else {
                    "Session closed. Background tasks initiated but may still be in progress."
                        .to_string()
                };

                Ok(Json(CloseSessionResult {
                    success: true,
                    thread_id: thread_id.clone(),
                    message,
                }))
            }
            Err(e) => {
                error!("Failed to close session {}: {}", thread_id, e);
                Err(format!("Failed to close session: {}", e))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for MemoryMcpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Cortex Memory MCP Server - Provides memory management tools for AI assistants.\n\
                \n\
                **Automatic Processing (v2.5):**\n\
                The server automatically triggers memory extraction and layer generation when:\n\
                - Message count reaches threshold (default: 10 messages)\n\
                - Session becomes inactive (default: 2 minutes without new messages)\n\
                \n\
                This ensures user/agent memories are created even without explicit close_session calls.\n\
                The processing uses the existing MemoryEventCoordinator infrastructure.\n\
                \n\
                Available tools:\n\
                - store_memory: Store a new memory (triggers auto-processing when conditions met)\n\
                - query_memory: Search memories using layered semantic search (L0→L1→L2)\n\
                - list_memories: List memories at a specific path\n\
                - get_memory: Retrieve full content of a specific memory\n\
                - delete_memory: Delete a memory\n\
                - get_abstract: Get L0 abstract (~100 tokens, for quick relevance checking)\n\
                - get_overview: Get L1 overview (~2000 tokens, for understanding core information)\n\
                - generate_layers: Generate L0/L1 layer files for memories (supports optional thread_id)\n\
                - index_memories: Index memories to vector database (supports optional thread_id)\n\
                - close_session: Close a session and wait for final processing\n\
                \n\
                Layered Access (L0/L1/L2):\n\
                - L0 (Abstract): ~100 tokens, for quick relevance checking\n\
                - L1 (Overview): ~2000 tokens, for understanding core information\n\
                - L2 (Full Content): Complete content, only when detailed information is needed\n\
                \n\
                URI format: cortex://{dimension}/{category}/{resource}\n\
                Examples:\n\
                - cortex://session/default/timeline/...\n\
                - cortex://user/preferences/language.md\n\
                - cortex://agent/cases/case_001.md\n\
                \n\
                Session Management:\n\
                - Sessions are automatically created on first store_memory call\n\
                - Memory extraction happens automatically based on thresholds\n\
                - close_session can still be called for explicit final processing\n\
                - Each session has a unique thread_id for isolation"
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}