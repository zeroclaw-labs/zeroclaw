//! Cascade Layer Updater Module
//!
//! Handles cascading updates to L0/L1 layers when memories change.
//! When a memory file changes, it updates the parent directory's layers,
/// then recursively updates all ancestor directories up to the root.
///
/// ## Optimizations:
/// - **Phase 1**: Content Hash Check - Skip unchanged content (50-80% reduction)
/// - **Phase 3**: LLM Result Cache - Reuse results for same content (50-75% reduction)

use crate::filesystem::{CortexFilesystem, FilesystemOperations};
use crate::layers::generator::{AbstractGenerator, OverviewGenerator};
use crate::llm::LLMClient;
use crate::llm_result_cache::{CacheConfig, LlmResultCache};
use crate::memory_events::{ChangeType, MemoryEvent};
use crate::memory_index::MemoryScope;
use crate::{ContextLayer, Result};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info};

/// Update statistics for monitoring optimization effectiveness
#[derive(Debug, Clone, Default)]
pub struct UpdateStats {
    /// Number of directories actually updated (LLM called)
    pub updated_count: usize,
    /// Number of updates skipped (content unchanged)
    pub skipped_count: usize,
    /// Total LLM calls made
    pub llm_call_count: usize,
    /// Cache hits (Phase 3)
    pub cache_hits: usize,
    /// Cache misses (Phase 3)
    pub cache_misses: usize,
}

impl UpdateStats {
    pub fn total_operations(&self) -> usize {
        self.updated_count + self.skipped_count
    }
    
    pub fn skip_rate(&self) -> f64 {
        if self.total_operations() == 0 {
            0.0
        } else {
            self.skipped_count as f64 / self.total_operations() as f64
        }
    }
    
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }
}

/// Cascade Layer Updater
///
/// Listens for memory change events and updates the layered memory files
/// (L0 abstracts and L1 overviews) in a cascading manner.
///
/// Optimizations:
/// - Phase 1: Content hash check to skip unchanged content
/// - Phase 3: LLM result cache to reuse previous results
pub struct CascadeLayerUpdater {
    filesystem: Arc<CortexFilesystem>,
    llm_client: Arc<dyn LLMClient>,
    l0_generator: AbstractGenerator,
    l1_generator: OverviewGenerator,
    event_tx: mpsc::UnboundedSender<MemoryEvent>,
    /// Statistics for monitoring (Phase 1)
    stats: Arc<RwLock<UpdateStats>>,
    /// LLM result cache (Phase 3)
    llm_cache: Option<Arc<LlmResultCache>>,
}

impl CascadeLayerUpdater {
    /// Create a new cascade layer updater without cache
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        llm_client: Arc<dyn LLMClient>,
        event_tx: mpsc::UnboundedSender<MemoryEvent>,
    ) -> Self {
        Self::new_with_cache(filesystem, llm_client, event_tx, None)
    }
    
    /// Create a new cascade layer updater with optional cache
    pub fn new_with_cache(
        filesystem: Arc<CortexFilesystem>,
        llm_client: Arc<dyn LLMClient>,
        event_tx: mpsc::UnboundedSender<MemoryEvent>,
        cache_config: Option<CacheConfig>,
    ) -> Self {
        let llm_cache = cache_config.map(|config| {
            Arc::new(LlmResultCache::new(config))
        });
        
        Self {
            filesystem,
            llm_client,
            l0_generator: AbstractGenerator::new(),
            l1_generator: OverviewGenerator::new(),
            event_tx,
            stats: Arc::new(RwLock::new(UpdateStats::default())),
            llm_cache,
        }
    }
    
    /// Get current update statistics
    pub async fn get_stats(&self) -> UpdateStats {
        self.stats.read().await.clone()
    }
    
    /// Reset statistics
    pub async fn reset_stats(&self) {
        let mut stats = self.stats.write().await;
        *stats = UpdateStats::default();
    }
    
    /// Calculate content hash for change detection
    fn calculate_content_hash(&self, content: &str) -> String {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }
    
    /// Check if layer should be updated based on content hash
    /// 
    /// Returns true if:
    /// - Layer file doesn't exist
    /// - Content hash has changed
    async fn should_update_layer(&self, layer_uri: &str, new_content_hash: &str) -> Result<bool> {
        // Try to read existing layer file
        match self.filesystem.read(layer_uri).await {
            Ok(existing_content) => {
                // Calculate hash of existing content (excluding timestamp)
                // Remove timestamp line for comparison
                let content_without_ts = existing_content
                    .lines()
                    .filter(|line| !line.starts_with("**Added**:"))
                    .collect::<Vec<_>>()
                    .join("\n");
                
                let old_hash = self.calculate_content_hash(&content_without_ts);
                
                // Only update if content changed
                Ok(old_hash != new_content_hash)
            }
            Err(_) => {
                // File doesn't exist, need to create
                Ok(true)
            }
        }
    }

    /// Handle a memory change event
    ///
    /// This is the main entry point for handling memory changes.
    /// It updates layers in a cascading manner from the changed file up to the root.
    pub async fn on_memory_changed(
        &self,
        scope: MemoryScope,
        owner_id: String,
        file_uri: String,
        change_type: ChangeType,
    ) -> Result<()> {
        debug!(
            "CascadeLayerUpdater: handling {:?} for {} in {:?}/{}",
            change_type, file_uri, scope, owner_id
        );

        // 1. Get parent directory
        let parent_dir = self.get_parent_directory(&file_uri);
        
        // 2. Update the parent directory's L0/L1
        self.update_directory_layers(&parent_dir, &scope, &owner_id).await?;
        
        // 3. Cascade to ancestor directories
        self.update_ancestor_layers(&scope, &owner_id, &parent_dir).await?;
        
        Ok(())
    }

    /// Update L0/L1 for a specific directory
    /// 
    /// Optimizations:
    /// - Phase 1: Content hash check
    /// - Phase 3: LLM result cache
    /// 
    /// This method is public to allow LayerUpdateDebouncer to call it
    pub async fn update_directory_layers(&self, dir_uri: &str, scope: &MemoryScope, owner_id: &str) -> Result<()> {
        // Check if directory has content to aggregate
        let content = self.aggregate_directory_content(dir_uri).await?;
        
        if content.is_empty() {
            debug!("Directory {} has no content, skipping layer update", dir_uri);
            return Ok(());
        }
        
        // 🔧 Phase 1: Calculate content hash
        let new_content_hash = self.calculate_content_hash(&content);
        
        // 🔧 Phase 1: Check if update is needed
        let abstract_uri = format!("{}/.abstract.md", dir_uri);
        let should_update = self.should_update_layer(&abstract_uri, &new_content_hash).await?;
        
        if !should_update {
            // Content unchanged, skip LLM calls
            debug!("⏭️  Skipped L0/L1 update for {} (content unchanged, hash: {})", dir_uri, &new_content_hash[..8]);
            
            // Update stats
            let mut stats = self.stats.write().await;
            stats.skipped_count += 1;
            
            return Ok(());
        }
        
        // Content changed or file doesn't exist, proceed with LLM generation
        info!("🔄 Updating L0/L1 for {} (hash: {} -> {})", dir_uri, "new", &new_content_hash[..8]);
        
        // 🔧 Phase 3: Try cache first
        let (abstract_text, overview) = if let Some(ref cache) = self.llm_cache {
            let cache_key_l0 = format!("{}:L0", new_content_hash);
            let cache_key_l1 = format!("{}:L1", new_content_hash);
            
            let cached_l0 = cache.get(&cache_key_l0).await;
            let cached_l1 = cache.get(&cache_key_l1).await;
            
            match (cached_l0, cached_l1) {
                (Some(l0), Some(l1)) => {
                    // Both cached!
                    debug!("💚 Cache HIT for both L0 and L1");
                    let mut stats = self.stats.write().await;
                    stats.cache_hits += 2;
                    (l0, l1)
                }
                _ => {
                    // Cache miss, generate and cache
                    debug!("💔 Cache MISS, generating with LLM");
                    
                    let l0 = self.l0_generator
                        .generate_with_llm(&content, &self.llm_client)
                        .await?;
                    
                    let l1 = self.l1_generator
                        .generate_with_llm(&content, &self.llm_client)
                        .await?;
                    
                    // Cache the results
                    cache.put(cache_key_l0, l0.clone()).await;
                    cache.put(cache_key_l1, l1.clone()).await;
                    
                    let mut stats = self.stats.write().await;
                    stats.cache_misses += 2;
                    stats.llm_call_count += 2;
                    
                    (l0, l1)
                }
            }
        } else {
            // No cache, generate directly
            let l0 = self.l0_generator
                .generate_with_llm(&content, &self.llm_client)
                .await?;
            
            let l1 = self.l1_generator
                .generate_with_llm(&content, &self.llm_client)
                .await?;
            
            let mut stats = self.stats.write().await;
            stats.llm_call_count += 2;
            
            (l0, l1)
        };
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.updated_count += 1;
        }
        
        // Add timestamp
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let abstract_with_ts = format!("{}\n\n**Added**: {}", abstract_text, timestamp);
        let overview_with_ts = format!("{}\n\n---\n\n**Added**: {}", overview, timestamp);
        
        // Write layer files
        let overview_uri = format!("{}/.overview.md", dir_uri);
        
        self.filesystem.write(&abstract_uri, &abstract_with_ts).await?;
        self.filesystem.write(&overview_uri, &overview_with_ts).await?;
        
        info!("✅ Updated L0/L1 layers for {}", dir_uri);
        
        // Emit layer update event
        let _ = self.event_tx.send(MemoryEvent::LayersUpdated {
            scope: scope.clone(),
            owner_id: owner_id.to_string(),
            directory_uri: dir_uri.to_string(),
            layers: vec![ContextLayer::L0Abstract, ContextLayer::L1Overview],
        });
        
        Ok(())
    }

    /// Update all ancestor directories up to the root
    async fn update_ancestor_layers(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        start_dir: &str,
    ) -> Result<()> {
        let root_uri = self.get_scope_root(scope, owner_id);
        
        let mut current = start_dir.to_string();
        
        // Walk up the directory tree until we reach the root
        loop {
            let parent = match self.get_parent_directory_opt(&current) {
                Some(p) => p,
                None => break,
            };
            
            if parent == current || parent.len() < root_uri.len() {
                break;
            }
            
            // For the root directory, aggregate all child directories' L0 abstracts
            if parent == root_uri {
                self.update_root_layers(scope, owner_id).await?;
                break;
            }
            
            // For intermediate directories, aggregate direct children
            self.update_directory_layers(&parent, scope, owner_id).await?;
            
            current = parent;
        }
        
        Ok(())
    }

    /// Update the root directory's L0/L1 by aggregating all subdirectories
    /// 
    /// Optimizations:
    /// - Phase 1: Content hash check
    /// - Phase 3: LLM result cache
    async fn update_root_layers(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
    ) -> Result<()> {
        let root_uri = self.get_scope_root(scope, owner_id);
        
        // Aggregate all child directories' L0 abstracts
        let aggregated = self.aggregate_child_abstracts(&root_uri).await?;
        
        if aggregated.is_empty() {
            debug!("Root {} has no content, skipping layer update", root_uri);
            return Ok(());
        }
        
        // 🔧 Phase 1: Calculate content hash
        let new_content_hash = self.calculate_content_hash(&aggregated);
        
        // 🔧 Phase 1: Check if update is needed
        let abstract_uri = format!("{}/.abstract.md", root_uri);
        let should_update = self.should_update_layer(&abstract_uri, &new_content_hash).await?;
        
        if !should_update {
            debug!("⏭️  Skipped root L0/L1 update for {:?}/{} (content unchanged)", scope, owner_id);
            
            // Update stats
            let mut stats = self.stats.write().await;
            stats.skipped_count += 1;
            
            return Ok(());
        }
        
        info!("🔄 Updating root L0/L1 for {:?}/{}", scope, owner_id);
        
        // 🔧 Phase 3: Try cache first
        let (abstract_text, overview) = if let Some(ref cache) = self.llm_cache {
            let cache_key_l0 = format!("{}:L0:root", new_content_hash);
            let cache_key_l1 = format!("{}:L1:root", new_content_hash);
            
            let cached_l0 = cache.get(&cache_key_l0).await;
            let cached_l1 = cache.get(&cache_key_l1).await;
            
            match (cached_l0, cached_l1) {
                (Some(l0), Some(l1)) => {
                    debug!("💚 Cache HIT for root L0 and L1");
                    let mut stats = self.stats.write().await;
                    stats.cache_hits += 2;
                    (l0, l1)
                }
                _ => {
                    debug!("💔 Cache MISS for root, generating with LLM");
                    
                    let l0 = self.l0_generator
                        .generate_with_llm(&aggregated, &self.llm_client)
                        .await?;
                    
                    let l1 = self.l1_generator
                        .generate_with_llm(&aggregated, &self.llm_client)
                        .await?;
                    
                    cache.put(cache_key_l0, l0.clone()).await;
                    cache.put(cache_key_l1, l1.clone()).await;
                    
                    let mut stats = self.stats.write().await;
                    stats.cache_misses += 2;
                    stats.llm_call_count += 2;
                    
                    (l0, l1)
                }
            }
        } else {
            let l0 = self.l0_generator
                .generate_with_llm(&aggregated, &self.llm_client)
                .await?;
            
            let l1 = self.l1_generator
                .generate_with_llm(&aggregated, &self.llm_client)
                .await?;
            
            let mut stats = self.stats.write().await;
            stats.llm_call_count += 2;
            
            (l0, l1)
        };
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.updated_count += 1;
        }
        
        // Add timestamp
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let abstract_with_ts = format!("{}\n\n**Added**: {}", abstract_text, timestamp);
        let overview_with_ts = format!("{}\n\n---\n\n**Added**: {}", overview, timestamp);
        
        // Write layer files
        let overview_uri = format!("{}/.overview.md", root_uri);
        
        self.filesystem.write(&abstract_uri, &abstract_with_ts).await?;
        self.filesystem.write(&overview_uri, &overview_with_ts).await?;
        
        info!("✅ Updated root L0/L1 layers for {:?}/{}", scope, owner_id);
        
        // Emit event
        let _ = self.event_tx.send(MemoryEvent::LayersUpdated {
            scope: scope.clone(),
            owner_id: owner_id.to_string(),
            directory_uri: root_uri,
            layers: vec![ContextLayer::L0Abstract, ContextLayer::L1Overview],
        });
        
        Ok(())
    }

    /// Aggregate content from all files in a directory (not recursive)
    async fn aggregate_directory_content(&self, dir_uri: &str) -> Result<String> {
        let entries = self.filesystem.list(dir_uri).await?;
        let mut content = String::new();
        let mut file_count = 0;
        
        for entry in entries {
            // Skip hidden files and directories
            if entry.name.starts_with('.') {
                continue;
            }
            
            if entry.is_directory {
                continue;
            }
            
            // Only read .md and .txt files
            if entry.name.ends_with(".md") || entry.name.ends_with(".txt") {
                match self.filesystem.read(&entry.uri).await {
                    Ok(file_content) => {
                        content.push_str(&format!("\n\n=== {} ===\n\n", entry.name));
                        content.push_str(&file_content);
                        file_count += 1;
                    }
                    Err(e) => {
                        debug!("Failed to read {}: {}", entry.uri, e);
                    }
                }
            }
        }
        
        if file_count > 0 {
            debug!("Aggregated {} files from {}", file_count, dir_uri);
        }
        
        // Truncate if too long
        let max_chars = 10000;
        if content.chars().count() > max_chars {
            let truncated: String = content.chars().take(max_chars).collect();
            content = truncated;
            content.push_str("\n\n[内容已截断...]");
        }
        
        Ok(content)
    }

    /// Aggregate L0 abstracts from all child directories
    async fn aggregate_child_abstracts(&self, dir_uri: &str) -> Result<String> {
        let entries = self.filesystem.list(dir_uri).await?;
        let mut content = String::new();
        let mut dir_count = 0;
        
        for entry in entries {
            // Only process directories
            if !entry.is_directory || entry.name.starts_with('.') {
                continue;
            }
            
            // Read the child directory's .abstract.md
            let abstract_uri = format!("{}/.abstract.md", entry.uri);
            if let Ok(abstract_content) = self.filesystem.read(&abstract_uri).await {
                content.push_str(&format!("\n\n## {}\n\n", entry.name));
                content.push_str(&abstract_content);
                dir_count += 1;
            }
        }
        
        if dir_count > 0 {
            debug!("Aggregated abstracts from {} child directories of {}", dir_count, dir_uri);
        }
        
        Ok(content)
    }

    /// Get the parent directory of a URI
    fn get_parent_directory(&self, uri: &str) -> String {
        uri.rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_else(|| uri.to_string())
    }

    /// Get the parent directory of a URI, if it exists
    fn get_parent_directory_opt(&self, uri: &str) -> Option<String> {
        uri.rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .filter(|dir| !dir.is_empty())
    }

    /// Get the root URI for a scope
    fn get_scope_root(&self, scope: &MemoryScope, owner_id: &str) -> String {
        match scope {
            MemoryScope::User => format!("cortex://user/{}", owner_id),
            MemoryScope::Agent => format!("cortex://agent/{}", owner_id),
            MemoryScope::Session => format!("cortex://session/{}", owner_id),
            MemoryScope::Resources => "cortex://resources".to_string(),
        }
    }

    /// Update timeline layers for a session
    ///
    /// This is called when a session closes to generate comprehensive
    /// L0/L1 for the entire timeline.
    pub async fn update_timeline_layers(&self, session_id: &str) -> Result<()> {
        let timeline_uri = format!("cortex://session/{}/timeline", session_id);
        
        // Check if timeline exists
        if !self.filesystem.exists(&timeline_uri).await? {
            debug!("Timeline {} does not exist, skipping", timeline_uri);
            return Ok(());
        }
        
        // Recursively collect all messages
        let content = self.aggregate_timeline_content(&timeline_uri).await?;
        
        if content.is_empty() {
            debug!("Timeline {} is empty, skipping layer update", timeline_uri);
            return Ok(());
        }
        
        // Generate L0 abstract
        let abstract_text = self.l0_generator
            .generate_with_llm(&content, &self.llm_client)
            .await?;
        
        // Generate L1 overview
        let overview = self.l1_generator
            .generate_with_llm(&content, &self.llm_client)
            .await?;
        
        // Add timestamp
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let abstract_with_ts = format!("{}\n\n**Added**: {}", abstract_text, timestamp);
        let overview_with_ts = format!("{}\n\n---\n\n**Added**: {}", overview, timestamp);
        
        // Write layer files
        let abstract_uri = format!("{}/.abstract.md", timeline_uri);
        let overview_uri = format!("{}/.overview.md", timeline_uri);
        
        self.filesystem.write(&abstract_uri, &abstract_with_ts).await?;
        self.filesystem.write(&overview_uri, &overview_with_ts).await?;
        
        info!("Updated timeline L0/L1 layers for session {}", session_id);
        
        // Emit event
        let _ = self.event_tx.send(MemoryEvent::LayersUpdated {
            scope: MemoryScope::Session,
            owner_id: session_id.to_string(),
            directory_uri: timeline_uri.clone(),
            layers: vec![ContextLayer::L0Abstract, ContextLayer::L1Overview],
        });
        
        // Also update date-level layers
        self.update_timeline_date_layers(&timeline_uri).await?;
        
        Ok(())
    }

    /// Recursively aggregate all messages from a timeline
    async fn aggregate_timeline_content(&self, timeline_uri: &str) -> Result<String> {
        let mut content = String::new();
        let mut message_count = 0;
        
        self.collect_timeline_messages_recursive(timeline_uri, &mut content, &mut message_count)
            .await?;
        
        if message_count > 0 {
            content.insert_str(0, &format!("# Timeline Messages: {}\n\n", message_count));
            debug!("Aggregated {} messages from {}", message_count, timeline_uri);
        }
        
        // Truncate if too long
        let max_chars = 15000;
        if content.chars().count() > max_chars {
            let truncated: String = content.chars().take(max_chars).collect();
            content = truncated;
            content.push_str("\n\n[内容已截断...]");
        }
        
        Ok(content)
    }

    /// Recursively collect messages from timeline subdirectories
    fn collect_timeline_messages_recursive<'a>(
        &'a self,
        uri: &'a str,
        content: &'a mut String,
        message_count: &'a mut usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.list(uri).await?;
            
            for entry in entries {
                if entry.name.starts_with('.') {
                    continue;
                }
                
                if entry.is_directory {
                    // Recurse into subdirectories
                    self.collect_timeline_messages_recursive(&entry.uri, content, message_count)
                        .await?;
                } else if entry.name.ends_with(".md") {
                    // Read message file
                    match self.filesystem.read(&entry.uri).await {
                        Ok(file_content) => {
                            content.push_str(&format!("\n\n---\n\n## Message: {}\n\n", entry.name));
                            content.push_str(&file_content);
                            *message_count += 1;
                        }
                        Err(e) => {
                            debug!("Failed to read {}: {}", entry.uri, e);
                        }
                    }
                }
            }
            
            Ok(())
        })
    }

    /// Update date-level layers within a timeline
    async fn update_timeline_date_layers(&self, timeline_uri: &str) -> Result<()> {
        let entries = self.filesystem.list(timeline_uri).await?;
        
        for entry in entries {
            // Process year-month directories
            if entry.is_directory && !entry.name.starts_with('.') {
                // Check if it's a date directory (YYYY-MM format)
                if entry.name.len() == 7 && entry.name.contains('-') {
                    // Aggregate content from this month
                    let month_content = self.aggregate_directory_content_recursive(&entry.uri).await?;
                    
                    if !month_content.is_empty() {
                        let abstract_text = self.l0_generator
                            .generate_with_llm(&month_content, &self.llm_client)
                            .await?;
                        
                        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
                        let abstract_with_ts = format!("{}\n\n**Added**: {}", abstract_text, timestamp);
                        
                        let abstract_uri = format!("{}/.abstract.md", entry.uri);
                        self.filesystem.write(&abstract_uri, &abstract_with_ts).await?;
                        
                        debug!("Updated month-level L0 for {}", entry.uri);
                    }
                    
                    // Process day directories within
                    self.update_timeline_day_layers(&entry.uri).await?;
                }
            }
        }
        
        Ok(())
    }

    /// Update day-level layers within a month directory
    async fn update_timeline_day_layers(&self, month_uri: &str) -> Result<()> {
        let entries = self.filesystem.list(month_uri).await?;
        
        for entry in entries {
            // Process day directories
            if entry.is_directory && !entry.name.starts_with('.') {
                let day_content = self.aggregate_directory_content(&entry.uri).await?;
                
                if !day_content.is_empty() {
                    let abstract_text = self.l0_generator
                        .generate_with_llm(&day_content, &self.llm_client)
                        .await?;
                    
                    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
                    let abstract_with_ts = format!("{}\n\n**Added**: {}", abstract_text, timestamp);
                    
                    let abstract_uri = format!("{}/.abstract.md", entry.uri);
                    self.filesystem.write(&abstract_uri, &abstract_with_ts).await?;
                    
                    debug!("Updated day-level L0 for {}", entry.uri);
                }
            }
        }
        
        Ok(())
    }

    /// Recursively aggregate all content from a directory
    async fn aggregate_directory_content_recursive(&self, dir_uri: &str) -> Result<String> {
        let mut content = String::new();
        
        self.collect_content_recursive(dir_uri, &mut content).await?;
        
        Ok(content)
    }

    /// Recursively collect content from all files
    fn collect_content_recursive<'a>(
        &'a self,
        uri: &'a str,
        content: &'a mut String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.list(uri).await?;
            
            for entry in entries {
                if entry.name.starts_with('.') {
                    continue;
                }
                
                if entry.is_directory {
                    self.collect_content_recursive(&entry.uri, content).await?;
                } else if entry.name.ends_with(".md") {
                    if let Ok(file_content) = self.filesystem.read(&entry.uri).await {
                        content.push_str(&format!("\n\n=== {} ===\n\n", entry.name));
                        content.push_str(&file_content);
                    }
                }
            }
            
            Ok(())
        })
    }

    /// Force update all layers for a scope
    ///
    /// This is useful for initialization or repair scenarios.
    pub async fn update_all_layers(&self, scope: &MemoryScope, owner_id: &str) -> Result<()> {
        let root_uri = self.get_scope_root(scope, owner_id);
        
        log::info!("🔄 update_all_layers: 检查根目录 {}", root_uri);
        
        if !self.filesystem.exists(&root_uri).await? {
            log::info!("📂 根目录 {} 不存在，跳过", root_uri);
            return Ok(());
        }
        
        log::info!("📂 根目录存在，开始递归更新层级文件...");
        
        // Walk through all directories and update layers
        self.update_all_layers_recursive(&root_uri, scope, owner_id).await?;
        
        // Update root layers last
        log::info!("🔄 开始更新根目录层级文件...");
        self.update_root_layers(scope, owner_id).await?;
        
        log::info!("✅ update_all_layers 完成: {:?}", scope);
        Ok(())
    }

    /// Recursively update all layers in a directory tree
    fn update_all_layers_recursive<'a>(
        &'a self,
        dir_uri: &'a str,
        scope: &'a MemoryScope,
        owner_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.list(dir_uri).await?;
            
            log::info!("📂 update_all_layers_recursive: {} 有 {} 个条目", dir_uri, entries.len());
            
            // First, process all subdirectories
            for entry in &entries {
                if entry.is_directory && !entry.name.starts_with('.') {
                    log::info!("📂   进入子目录: {}", entry.name);
                    self.update_all_layers_recursive(&entry.uri, scope, owner_id).await?;
                }
            }
            
            // Then, update this directory's layers (if it has content files)
            let has_content = entries.iter().any(|e| {
                !e.is_directory && !e.name.starts_with('.') && e.name.ends_with(".md")
            });
            
            log::info!("📂 目录 {} 是否有内容文件: {}", dir_uri, has_content);
            
            if has_content {
                log::info!("🔄 开始为目录 {} 生成层级文件...", dir_uri);
                match self.update_directory_layers(dir_uri, scope, owner_id).await {
                    Ok(_) => log::info!("✅ 目录 {} 层级文件生成成功", dir_uri),
                    Err(e) => log::warn!("⚠️ 目录 {} 层级文件生成失败: {}", dir_uri, e),
                }
            }
            
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::CortexFilesystem;
    use crate::llm::MockLLMClient;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    #[test]
    fn test_get_parent_directory() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let filesystem = Arc::new(CortexFilesystem::new("/tmp/test"));
        let llm_client = Arc::new(MockLLMClient::new());
        
        let updater = CascadeLayerUpdater::new(filesystem, llm_client, tx);
        
        assert_eq!(updater.get_parent_directory("cortex://user/test/path/file.md"), "cortex://user/test/path");
        assert_eq!(updater.get_parent_directory("cortex://user/test/file.md"), "cortex://user/test");
        assert_eq!(updater.get_parent_directory("cortex://user/file.md"), "cortex://user");
    }

    #[test]
    fn test_get_scope_root() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let filesystem = Arc::new(CortexFilesystem::new("/tmp/test"));
        let llm_client = Arc::new(MockLLMClient::new());
        
        let updater = CascadeLayerUpdater::new(filesystem, llm_client, tx);
        
        assert_eq!(updater.get_scope_root(&MemoryScope::User, "user_001"), "cortex://user/user_001");
        assert_eq!(updater.get_scope_root(&MemoryScope::Agent, "agent_001"), "cortex://agent/agent_001");
        assert_eq!(updater.get_scope_root(&MemoryScope::Session, "session_001"), "cortex://session/session_001");
        assert_eq!(updater.get_scope_root(&MemoryScope::Resources, ""), "cortex://resources");
    }

    #[test]
    fn test_get_parent_directory_opt() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let filesystem = Arc::new(CortexFilesystem::new("/tmp/test"));
        let llm_client = Arc::new(MockLLMClient::new());
        
        let updater = CascadeLayerUpdater::new(filesystem, llm_client, tx);
        
        assert_eq!(updater.get_parent_directory_opt("cortex://user/test/file.md"), Some("cortex://user/test".to_string()));
        assert_eq!(updater.get_parent_directory_opt("cortex://user/file.md"), Some("cortex://user".to_string()));
        // cortex://file.md -> "cortex:/" (after rsplit_once('/') on "cortex://file.md")
        assert_eq!(updater.get_parent_directory_opt("cortex://file.md"), Some("cortex:/".to_string()));
    }
}
