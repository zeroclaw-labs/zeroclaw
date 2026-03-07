pub mod qdrant;

#[cfg(test)]
mod tests;

use crate::{
    error::Result,
    types::{ContextLayer, Filters, Memory, ScoredMemory},
};
use async_trait::async_trait;

pub use qdrant::QdrantVectorStore;

/// Generate normalized vector ID from URI and layer
/// 
/// Uses a hash function to generate a deterministic ID from the URI and layer.
/// This ensures the ID is consistent for the same URI and layer combination.
/// 
/// # Examples
/// - L0: generates ID from `{uri}#/L0`
/// - L1: generates ID from `{uri}#/L1`
/// - L2: generates ID from `{uri}`
pub fn uri_to_vector_id(uri: &str, layer: ContextLayer) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    
    let id_string = match layer {
        ContextLayer::L0Abstract => format!("{}#/L0", uri),
        ContextLayer::L1Overview => format!("{}#/L1", uri),
        ContextLayer::L2Detail => uri.to_string(),
    };
    
    // Generate two different hashes from the ID string for more entropy
    let mut hasher1 = DefaultHasher::new();
    id_string.hash(&mut hasher1);
    let hash1 = hasher1.finish();
    
    let mut hasher2 = DefaultHasher::new();
    // Hash the hash to get a different value
    hash1.hash(&mut hasher2);
    let hash2 = hasher2.finish();
    
    // Format as a proper UUID string (8-4-4-4-12 format = 32 hex chars total)
    // Use hash1 for first 3 segments, hash2 for last 2 segments
    format!("{:08x}-{:04x}-{:04x}-{:04x}-{:012x}", 
        ((hash1 >> 32) & 0xFFFFFFFF) as u32,  // 8 chars from hash1
        ((hash1 >> 16) & 0xFFFF) as u16,      // 4 chars from hash1
        (hash1 & 0xFFFF) as u16,              // 4 chars from hash1
        ((hash2 >> 48) & 0xFFFF) as u16,      // 4 chars from hash2
        hash2 & 0xFFFFFFFFFFFF                // 12 chars from hash2
    )
}

/// Parse vector ID back to (uri, layer)
/// 
/// Parse a vector ID to extract URI and layer information.
/// 
/// This function handles two formats:
/// 1. Legacy format: `cortex://...#/L0` or `cortex://...#/L1` (with layer suffix)
/// 2. New UUID format: Returns as-is with L2Detail layer (cannot reverse hash)
/// 
/// For the new UUID format, the original URI should be retrieved from MemoryMetadata.
pub fn parse_vector_id(vector_id: &str) -> (String, ContextLayer) {
    // Check if it's a legacy format with layer suffix
    if vector_id.ends_with("#/L0") {
        let uri = vector_id.trim_end_matches("#/L0").to_string();
        return (uri, ContextLayer::L0Abstract);
    }
    if vector_id.ends_with("#/L1") {
        let uri = vector_id.trim_end_matches("#/L1").to_string();
        return (uri, ContextLayer::L1Overview);
    }
    
    // Check if it's a UUID format (contains dashes in specific positions)
    let parts: Vec<&str> = vector_id.split('-').collect();
    if parts.len() == 5 && 
       parts[0].len() == 8 && parts[1].len() == 4 && 
       parts[2].len() == 4 && parts[3].len() == 4 && parts[4].len() == 12 {
        // It's a UUID format, return as-is with L2Detail
        return (vector_id.to_string(), ContextLayer::L2Detail);
    }
    
    // Assume it's a legacy L2 format (plain URI)
    (vector_id.to_string(), ContextLayer::L2Detail)
}

/// Trait for vector store operations
#[async_trait]
pub trait VectorStore: Send + Sync + dyn_clone::DynClone {
    /// Insert a memory into the vector store
    async fn insert(&self, memory: &Memory) -> Result<()>;

    /// Search for similar memories
    async fn search(
        &self,
        query_vector: &[f32],
        filters: &Filters,
        limit: usize,
    ) -> Result<Vec<ScoredMemory>>;

    /// Search for similar memories with similarity threshold
    async fn search_with_threshold(
        &self,
        query_vector: &[f32],
        filters: &Filters,
        limit: usize,
        score_threshold: Option<f32>,
    ) -> Result<Vec<ScoredMemory>>;

    /// Update an existing memory
    async fn update(&self, memory: &Memory) -> Result<()>;

    /// Delete a memory by ID
    async fn delete(&self, id: &str) -> Result<()>;

    /// Get a memory by ID
    async fn get(&self, id: &str) -> Result<Option<Memory>>;

    /// List all memories with optional filters
    async fn list(&self, filters: &Filters, limit: Option<usize>) -> Result<Vec<Memory>>;

    /// Check if the vector store is healthy
    async fn health_check(&self) -> Result<bool>;
    
    /// Scroll through memory IDs (for incremental indexing)
    async fn scroll_ids(&self, filters: &Filters, limit: usize) -> Result<Vec<String>>;
}

dyn_clone::clone_trait_object!(VectorStore);
