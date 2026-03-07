//! Vector Sync Manager Module
//!
//! Handles synchronization between file system and vector database.
//! Ensures consistency when memories are created, updated, or deleted.

use crate::embedding::EmbeddingClient;
use crate::filesystem::{CortexFilesystem, FilesystemOperations};
use crate::memory_events::ChangeType;
use crate::types::Memory;
use crate::vector_store::{QdrantVectorStore, VectorStore, uri_to_vector_id};
use crate::{ContextLayer, Result};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Vector sync statistics
#[derive(Debug, Clone, Default)]
pub struct VectorSyncStats {
    pub indexed: usize,
    pub updated: usize,
    pub deleted: usize,
    pub skipped: usize,
    pub errors: usize,
}

impl VectorSyncStats {
    pub fn total_operations(&self) -> usize {
        self.indexed + self.updated + self.deleted + self.skipped
    }
}

/// Vector Sync Manager
///
/// Manages synchronization between the file system and vector database.
/// Supports incremental updates and full verification.
pub struct VectorSyncManager {
    filesystem: Arc<CortexFilesystem>,
    embedding: Arc<EmbeddingClient>,
    vector_store: Arc<QdrantVectorStore>,
}

impl VectorSyncManager {
    /// Create a new vector sync manager
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        embedding: Arc<EmbeddingClient>,
        vector_store: Arc<QdrantVectorStore>,
    ) -> Self {
        Self {
            filesystem,
            embedding,
            vector_store,
        }
    }

    /// Sync a file change to the vector database
    ///
    /// This is the main entry point for handling file changes.
    pub async fn sync_file_change(
        &self,
        file_uri: &str,
        change_type: ChangeType,
    ) -> Result<VectorSyncStats> {
        let mut stats = VectorSyncStats::default();
        
        match change_type {
            ChangeType::Add => {
                self.index_file(file_uri, &mut stats).await?;
            }
            ChangeType::Update => {
                self.update_file(file_uri, &mut stats).await?;
            }
            ChangeType::Delete => {
                self.delete_file(file_uri, &mut stats).await?;
            }
        }
        
        Ok(stats)
    }

    /// Index a new file to the vector database
    async fn index_file(&self, file_uri: &str, stats: &mut VectorSyncStats) -> Result<()> {
        // Check if already indexed
        let l2_id = uri_to_vector_id(file_uri, ContextLayer::L2Detail);
        if self.vector_store.get(&l2_id).await?.is_some() {
            debug!("File {} already indexed, skipping", file_uri);
            stats.skipped += 1;
            return Ok(());
        }
        
        // Read file content
        let content = match self.filesystem.read(file_uri).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read file {}: {}", file_uri, e);
                stats.errors += 1;
                return Ok(());
            }
        };
        
        // Generate embedding
        let embedding = match self.embedding.embed(&content).await {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to generate embedding for {}: {}", file_uri, e);
                stats.errors += 1;
                return Ok(());
            }
        };
        
        // Create memory for L2
        let memory = Memory {
            id: l2_id.clone(),
            content: content.clone(),
            embedding,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: Default::default(),
        };
        
        // Insert into vector store
        self.vector_store.insert(&memory).await?;
        stats.indexed += 1;
        
        debug!("Indexed L2 for {}", file_uri);
        
        // Also index L0/L1 if they exist
        self.index_layer_files(file_uri, stats).await?;
        
        Ok(())
    }

    /// Update an existing file in the vector database
    async fn update_file(&self, file_uri: &str, stats: &mut VectorSyncStats) -> Result<()> {
        // Delete old vectors first
        self.delete_vectors_for_uri(file_uri).await?;
        
        // Re-index
        self.index_file(file_uri, stats).await?;
        
        stats.updated += 1;
        stats.indexed = stats.indexed.saturating_sub(1); // Adjust for the re-index
        
        Ok(())
    }

    /// Delete a file from the vector database
    async fn delete_file(&self, file_uri: &str, stats: &mut VectorSyncStats) -> Result<()> {
        self.delete_vectors_for_uri(file_uri).await?;
        stats.deleted += 3; // L0, L1, L2
        
        Ok(())
    }

    /// Delete all vectors for a URI
    async fn delete_vectors_for_uri(&self, file_uri: &str) -> Result<()> {
        for layer in [ContextLayer::L0Abstract, ContextLayer::L1Overview, ContextLayer::L2Detail] {
            let vector_id = uri_to_vector_id(file_uri, layer.clone());
            let _ = self.vector_store.delete(&vector_id).await; // Ignore errors if not found
        }
        
        // Also delete directory-level vectors
        let dir_uri = file_uri.rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or(file_uri);
        
        for layer in [ContextLayer::L0Abstract, ContextLayer::L1Overview] {
            let vector_id = uri_to_vector_id(dir_uri, layer.clone());
            let _ = self.vector_store.delete(&vector_id).await;
        }
        
        Ok(())
    }

    /// Index L0/L1 layer files if they exist
    async fn index_layer_files(&self, file_uri: &str, stats: &mut VectorSyncStats) -> Result<()> {
        let dir_uri = file_uri.rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or(file_uri);
        
        // Index L0 abstract
        let l0_uri = format!("{}/.abstract.md", dir_uri);
        if self.filesystem.exists(&l0_uri).await? {
            let l0_id = uri_to_vector_id(dir_uri, ContextLayer::L0Abstract);
            
            if self.vector_store.get(&l0_id).await?.is_none() {
                if let Ok(content) = self.filesystem.read(&l0_uri).await {
                    if let Ok(embedding) = self.embedding.embed(&content).await {
                        let memory = Memory {
                            id: l0_id,
                            content,
                            embedding,
                            created_at: chrono::Utc::now(),
                            updated_at: chrono::Utc::now(),
                            metadata: Default::default(),
                        };
                        
                        self.vector_store.insert(&memory).await?;
                        stats.indexed += 1;
                        debug!("Indexed L0 for {}", dir_uri);
                    }
                }
            }
        }
        
        // Index L1 overview
        let l1_uri = format!("{}/.overview.md", dir_uri);
        if self.filesystem.exists(&l1_uri).await? {
            let l1_id = uri_to_vector_id(dir_uri, ContextLayer::L1Overview);
            
            if self.vector_store.get(&l1_id).await?.is_none() {
                if let Ok(content) = self.filesystem.read(&l1_uri).await {
                    if let Ok(embedding) = self.embedding.embed(&content).await {
                        let memory = Memory {
                            id: l1_id,
                            content,
                            embedding,
                            created_at: chrono::Utc::now(),
                            updated_at: chrono::Utc::now(),
                            metadata: Default::default(),
                        };
                        
                        self.vector_store.insert(&memory).await?;
                        stats.indexed += 1;
                        debug!("Indexed L1 for {}", dir_uri);
                    }
                }
            }
        }
        
        Ok(())
    }

    /// Sync all files under a directory
    pub async fn sync_directory(&self, dir_uri: &str) -> Result<VectorSyncStats> {
        let mut stats = VectorSyncStats::default();
        
        self.sync_directory_recursive(dir_uri, &mut stats).await?;
        
        info!(
            "Sync completed for {}: {} indexed, {} updated, {} deleted, {} errors",
            dir_uri,
            stats.indexed,
            stats.updated,
            stats.deleted,
            stats.errors
        );
        
        Ok(stats)
    }

    /// Recursively sync all files in a directory
    fn sync_directory_recursive<'a>(
        &'a self,
        dir_uri: &'a str,
        stats: &'a mut VectorSyncStats,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.list(dir_uri).await?;
            
            for entry in entries {
                if entry.is_directory {
                    if !entry.name.starts_with('.') {
                        self.sync_directory_recursive(&entry.uri, stats).await?;
                    }
                } else if entry.name.ends_with(".md") && !entry.name.starts_with('.') {
                    let l2_id = uri_to_vector_id(&entry.uri, ContextLayer::L2Detail);
                    
                    if self.vector_store.get(&l2_id).await?.is_some() {
                        // Check if content changed
                        if let Ok(content) = self.filesystem.read(&entry.uri).await {
                            let _hash = self.calculate_hash(&content);
                            // Compare with stored hash (simplified - would need metadata comparison)
                            // For now, skip if already indexed
                            stats.skipped += 1;
                        }
                    } else {
                        // Index new file
                        self.index_file(&entry.uri, stats).await?;
                    }
                }
            }
            
            Ok(())
        })
    }

    /// Sync layer files for a directory
    pub async fn sync_layer_files(&self, dir_uri: &str) -> Result<VectorSyncStats> {
        let mut stats = VectorSyncStats::default();
        
        let l0_uri = format!("{}/.abstract.md", dir_uri);
        let l1_uri = format!("{}/.overview.md", dir_uri);
        
        // Index L0
        if self.filesystem.exists(&l0_uri).await? {
            let l0_id = uri_to_vector_id(dir_uri, ContextLayer::L0Abstract);
            
            if self.vector_store.get(&l0_id).await?.is_none() {
                if let Ok(content) = self.filesystem.read(&l0_uri).await {
                    if let Ok(embedding) = self.embedding.embed(&content).await {
                        let memory = Memory {
                            id: l0_id,
                            content,
                            embedding,
                            created_at: chrono::Utc::now(),
                            updated_at: chrono::Utc::now(),
                            metadata: Default::default(),
                        };
                        
                        self.vector_store.insert(&memory).await?;
                        stats.indexed += 1;
                    }
                }
            }
        }
        
        // Index L1
        if self.filesystem.exists(&l1_uri).await? {
            let l1_id = uri_to_vector_id(dir_uri, ContextLayer::L1Overview);
            
            if self.vector_store.get(&l1_id).await?.is_none() {
                if let Ok(content) = self.filesystem.read(&l1_uri).await {
                    if let Ok(embedding) = self.embedding.embed(&content).await {
                        let memory = Memory {
                            id: l1_id,
                            content,
                            embedding,
                            created_at: chrono::Utc::now(),
                            updated_at: chrono::Utc::now(),
                            metadata: Default::default(),
                        };
                        
                        self.vector_store.insert(&memory).await?;
                        stats.indexed += 1;
                    }
                }
            }
        }
        
        Ok(stats)
    }

    /// Full sync for all scopes
    pub async fn sync_all(&self) -> Result<VectorSyncStats> {
        let mut total_stats = VectorSyncStats::default();
        
        // Sync user memories
        if let Ok(entries) = self.filesystem.list("cortex://user").await {
            for entry in entries {
                if entry.is_directory {
                    let stats = self.sync_directory(&entry.uri).await?;
                    total_stats.indexed += stats.indexed;
                    total_stats.skipped += stats.skipped;
                    total_stats.errors += stats.errors;
                }
            }
        }
        
        // Sync agent memories
        if let Ok(entries) = self.filesystem.list("cortex://agent").await {
            for entry in entries {
                if entry.is_directory {
                    let stats = self.sync_directory(&entry.uri).await?;
                    total_stats.indexed += stats.indexed;
                    total_stats.skipped += stats.skipped;
                    total_stats.errors += stats.errors;
                }
            }
        }
        
        // Sync session memories
        if let Ok(entries) = self.filesystem.list("cortex://session").await {
            for entry in entries {
                if entry.is_directory {
                    let stats = self.sync_directory(&entry.uri).await?;
                    total_stats.indexed += stats.indexed;
                    total_stats.skipped += stats.skipped;
                    total_stats.errors += stats.errors;
                }
            }
        }
        
        info!(
            "Full sync completed: {} indexed, {} skipped, {} errors",
            total_stats.indexed,
            total_stats.skipped,
            total_stats.errors
        );
        
        Ok(total_stats)
    }

    /// Verify and repair consistency
    pub async fn verify_and_repair(&self, scope_uri: &str) -> Result<VectorSyncStats> {
        let mut stats = VectorSyncStats::default();
        
        // Collect all files
        let mut files = Vec::new();
        self.collect_files_recursive(scope_uri, &mut files).await?;
        
        // Check each file
        for file_uri in &files {
            let l2_id = uri_to_vector_id(file_uri, ContextLayer::L2Detail);
            
            match self.vector_store.get(&l2_id).await {
                Ok(Some(vector)) => {
                    // Check content hash
                    if let Ok(content) = self.filesystem.read(file_uri).await {
                        let _current_hash = self.calculate_hash(&content);
                        
                        // Simple comparison - in production would compare with stored hash
                        if content.len() != vector.content.len() {
                            // Content changed, re-index
                            self.update_file(file_uri, &mut stats).await?;
                        }
                    }
                }
                Ok(None) => {
                    // Not indexed, index it
                    self.index_file(file_uri, &mut stats).await?;
                }
                Err(_) => {
                    stats.errors += 1;
                }
            }
        }
        
        Ok(stats)
    }

    /// Recursively collect all file URIs
    fn collect_files_recursive<'a>(
        &'a self,
        uri: &'a str,
        files: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.list(uri).await?;
            
            for entry in entries {
                if entry.is_directory {
                    if !entry.name.starts_with('.') {
                        self.collect_files_recursive(&entry.uri, files).await?;
                    }
                } else if entry.name.ends_with(".md") && !entry.name.starts_with('.') {
                    files.push(entry.uri.clone());
                }
            }
            
            Ok(())
        })
    }

    /// Calculate content hash
    fn calculate_hash(&self, content: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// Delete all vectors for a session
    pub async fn delete_session_vectors(&self, session_id: &str) -> Result<usize> {
        let session_uri = format!("cortex://session/{}", session_id);
        let mut deleted = 0;
        
        // Collect all files in the session
        let mut files = Vec::new();
        self.collect_files_recursive(&session_uri, &mut files).await?;
        
        // Delete vectors for each file
        for file_uri in &files {
            for layer in [ContextLayer::L0Abstract, ContextLayer::L1Overview, ContextLayer::L2Detail] {
                let vector_id = uri_to_vector_id(file_uri, layer);
                if self.vector_store.delete(&vector_id).await.is_ok() {
                    deleted += 1;
                }
            }
        }
        
        // Also delete session-level L0/L1
        let timeline_uri = format!("cortex://session/{}/timeline", session_id);
        for layer in [ContextLayer::L0Abstract, ContextLayer::L1Overview] {
            let vector_id = uri_to_vector_id(&timeline_uri, layer);
            if self.vector_store.delete(&vector_id).await.is_ok() {
                deleted += 1;
            }
        }
        
        info!("Deleted {} vectors for session {}", deleted, session_id);
        
        Ok(deleted)
    }
}
