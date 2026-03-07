//! Memory Index Manager Module
//!
//! Manages loading, saving, and querying memory index files.
//! Each scope (user, agent, session) has its own .memory_index.json file.

use crate::filesystem::{CortexFilesystem, FilesystemOperations};
use crate::memory_index::{MemoryIndex, MemoryMetadata, MemoryScope, MemoryType};
use crate::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// In-memory cache key for memory indices
type CacheKey = (MemoryScope, String);

/// Memory Index Manager
///
/// Handles loading, caching, and persisting memory indices.
/// Also provides utilities for content hashing and memory matching.
pub struct MemoryIndexManager {
    filesystem: Arc<CortexFilesystem>,
    /// In-memory cache of loaded indices
    cache: Arc<RwLock<std::collections::HashMap<CacheKey, MemoryIndex>>>,
}

impl MemoryIndexManager {
    /// Create a new memory index manager
    pub fn new(filesystem: Arc<CortexFilesystem>) -> Self {
        Self {
            filesystem,
            cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Get the index file URI for a scope
    fn get_index_uri(scope: &MemoryScope, owner_id: &str) -> String {
        match scope {
            MemoryScope::User => format!("cortex://user/{}/.memory_index.json", owner_id),
            MemoryScope::Agent => format!("cortex://agent/{}/.memory_index.json", owner_id),
            MemoryScope::Session => format!("cortex://session/{}/.memory_index.json", owner_id),
            MemoryScope::Resources => "cortex://resources/.memory_index.json".to_string(),
        }
    }

    /// Load the memory index for a scope (with caching)
    pub async fn load_index(&self, scope: MemoryScope, owner_id: String) -> Result<MemoryIndex> {
        let key = (scope.clone(), owner_id.clone());
        
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(index) = cache.get(&key) {
                return Ok(index.clone());
            }
        }
        
        // Load from filesystem
        let index_uri = Self::get_index_uri(&scope, &owner_id);
        
        let index = if self.filesystem.exists(&index_uri).await? {
            let content = self.filesystem.read(&index_uri).await?;
            match serde_json::from_str::<MemoryIndex>(&content) {
                Ok(index) => index,
                Err(e) => {
                    warn!("Failed to parse memory index for {:?}/{}, creating new: {}", scope, owner_id, e);
                    MemoryIndex::new(scope.clone(), owner_id.clone())
                }
            }
        } else {
            debug!("Creating new memory index for {:?}/{}", scope, owner_id);
            MemoryIndex::new(scope.clone(), owner_id.clone())
        };
        
        // Cache the index
        {
            let mut cache = self.cache.write().await;
            cache.insert(key.clone(), index.clone());
        }
        
        Ok(index)
    }

    /// Save the memory index for a scope
    pub async fn save_index(&self, index: &MemoryIndex) -> Result<()> {
        let key = (index.scope.clone(), index.owner_id.clone());
        
        // Update cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(key, index.clone());
        }
        
        // Persist to filesystem
        let index_uri = Self::get_index_uri(&index.scope, &index.owner_id);
        let content = serde_json::to_string_pretty(index)?;
        self.filesystem.write(&index_uri, &content).await?;
        
        debug!("Saved memory index for {:?}/{}", index.scope, index.owner_id);
        Ok(())
    }

    /// Invalidate cached index for a scope
    pub async fn invalidate_cache(&self, scope: &MemoryScope, owner_id: &str) {
        let key = (scope.clone(), owner_id.to_string());
        let mut cache = self.cache.write().await;
        cache.remove(&key);
    }

    /// Find a matching memory by type and key
    pub async fn find_matching_memory(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_type: &MemoryType,
        key: &str,
    ) -> Result<Option<MemoryMetadata>> {
        let index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        Ok(index.find_by_type_and_key(memory_type, key).cloned())
    }

    /// Add or update a memory in the index
    pub async fn upsert_memory(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        metadata: MemoryMetadata,
    ) -> Result<bool> {
        let mut index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        let is_new = index.upsert_memory(metadata);
        self.save_index(&index).await?;
        Ok(is_new)
    }

    /// Remove a memory from the index
    pub async fn remove_memory(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_id: &str,
    ) -> Result<Option<MemoryMetadata>> {
        let mut index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        let removed = index.remove_memory(memory_id);
        if removed.is_some() {
            self.save_index(&index).await?;
        }
        Ok(removed)
    }

    /// Record a session extraction in the index
    pub async fn record_session_extraction(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        session_id: &str,
        created: Vec<String>,
        updated: Vec<String>,
    ) -> Result<()> {
        let mut index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        index.record_session_extraction(session_id, created, updated);
        self.save_index(&index).await?;
        Ok(())
    }

    /// Record a memory access
    pub async fn record_access(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_id: &str,
    ) -> Result<()> {
        let mut index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        
        if let Some(metadata) = index.memories.get_mut(memory_id) {
            metadata.record_access();
            self.save_index(&index).await?;
        }
        
        Ok(())
    }

    /// Calculate content hash for change detection
    pub fn calculate_content_hash(content: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// Generate a summary from content (for quick comparison)
    pub fn generate_content_summary(content: &str, max_len: usize) -> String {
        // Remove metadata lines
        let clean_content = content
            .lines()
            .filter(|line| !line.starts_with("**") && !line.starts_with("---"))
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        
        // Truncate to max length
        if clean_content.chars().count() > max_len {
            let truncated: String = clean_content.chars().take(max_len).collect();
            format!("{}...", truncated)
        } else {
            clean_content
        }
    }

    /// Get all memories from a scope
    pub async fn get_all_memories(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
    ) -> Result<Vec<MemoryMetadata>> {
        let index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        Ok(index.memories.values().cloned().collect())
    }

    /// Get memories by type
    pub async fn get_memories_by_type(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_type: &MemoryType,
    ) -> Result<Vec<MemoryMetadata>> {
        let index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        Ok(index.get_by_type(memory_type).into_iter().cloned().collect())
    }

    /// Check if content has meaningfully changed
    pub fn content_changed(old_hash: &str, new_hash: &str, old_summary: &str, new_summary: &str) -> bool {
        // If hash is different, content changed
        if old_hash != new_hash {
            return true;
        }
        
        // If summaries differ significantly, content might have changed
        // (use similarity threshold)
        let similarity = Self::calculate_similarity(old_summary, new_summary);
        similarity < 0.9
    }

    /// Calculate similarity between two strings
    fn calculate_similarity(a: &str, b: &str) -> f64 {
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }

        let a_lower = a.to_lowercase();
        let b_lower = b.to_lowercase();

        // Simple word overlap similarity
        let a_words: std::collections::HashSet<&str> = a_lower.split_whitespace().collect();
        let b_words: std::collections::HashSet<&str> = b_lower.split_whitespace().collect();

        let intersection = a_words.intersection(&b_words).count();
        let union = a_words.union(&b_words).count();

        if union == 0 {
            0.0
        } else {
            intersection as f64 / union as f64
        }
    }

    /// Delete all memories from a specific session
    pub async fn delete_memories_from_session(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        session_id: &str,
    ) -> Result<Vec<MemoryMetadata>> {
        let mut index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        
        // Find all memories from this session
        let to_delete: Vec<String> = index
            .memories
            .values()
            .filter(|m| m.source_sessions.contains(&session_id.to_string()))
            .map(|m| m.id.clone())
            .collect();
        
        let mut deleted = Vec::new();
        for memory_id in to_delete {
            if let Some(metadata) = index.remove_memory(&memory_id) {
                deleted.push(metadata);
            }
        }
        
        if !deleted.is_empty() {
            self.save_index(&index).await?;
        }
        
        Ok(deleted)
    }

    /// Migrate existing files to index (one-time migration)
    pub async fn migrate_existing_files(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_type: &MemoryType,
        directory: &str,
    ) -> Result<usize> {
        let mut index = self.load_index(scope.clone(), owner_id.to_string()).await?;
        let mut migrated = 0;
        
        // Check if directory exists
        let dir_uri = format!("cortex://{}/{}/{}", 
            match scope {
                MemoryScope::User => "user",
                MemoryScope::Agent => "agent",
                MemoryScope::Session => "session",
                MemoryScope::Resources => "resources",
            },
            owner_id,
            directory
        );
        
        if !self.filesystem.exists(&dir_uri).await? {
            return Ok(0);
        }
        
        // List files and add to index
        let entries = self.filesystem.list(&dir_uri).await?;
        for entry in entries {
            if entry.name.ends_with(".md") && !entry.name.starts_with('.') {
                let content = self.filesystem.read(&entry.uri).await?;
                let hash = Self::calculate_content_hash(&content);
                let summary = Self::generate_content_summary(&content, 200);
                
                // Extract key from filename or content
                let memory_id = entry.name.trim_end_matches(".md").to_string();
                let key = memory_id.clone(); // Can be improved to extract from content
                
                let metadata = MemoryMetadata::new(
                    memory_id,
                    format!("{}/{}", directory, entry.name),
                    memory_type.clone(),
                    key,
                    hash,
                    "migration",
                    0.5, // Default confidence
                    summary,
                );
                
                index.upsert_memory(metadata);
                migrated += 1;
            }
        }
        
        if migrated > 0 {
            self.save_index(&index).await?;
            info!("Migrated {} files to memory index for {:?}/{}", migrated, scope, owner_id);
        }
        
        Ok(migrated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_content_hash() {
        let hash1 = MemoryIndexManager::calculate_content_hash("test content");
        let hash2 = MemoryIndexManager::calculate_content_hash("test content");
        let hash3 = MemoryIndexManager::calculate_content_hash("different content");
        
        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_generate_content_summary() {
        let content = "# Title\n\nThis is the main content.\n\n**Added**: 2024-01-01";
        let summary = MemoryIndexManager::generate_content_summary(content, 50);
        
        assert!(!summary.contains("**Added**"));
        assert!(summary.contains("Title"));
    }

    #[test]
    fn test_content_changed() {
        let hash1 = "abc123";
        let hash2 = "def456";
        let summary = "test summary";
        
        assert!(MemoryIndexManager::content_changed(hash1, hash2, summary, summary));
        assert!(!MemoryIndexManager::content_changed(hash1, hash1, summary, summary));
    }
}
