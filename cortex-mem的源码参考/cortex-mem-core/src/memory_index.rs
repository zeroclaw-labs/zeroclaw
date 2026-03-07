//! Memory Index Module
//!
//! Provides version tracking and metadata management for memories.
//! Each dimension (user, agent, session) maintains a .memory_index.json file
//! that tracks all memories with their sources, timestamps, and access statistics.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Memory type enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Preference,
    Entity,
    Event,
    Case,
    PersonalInfo,
    WorkHistory,
    Relationship,
    Goal,
    Conversation,
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryType::Preference => write!(f, "preference"),
            MemoryType::Entity => write!(f, "entity"),
            MemoryType::Event => write!(f, "event"),
            MemoryType::Case => write!(f, "case"),
            MemoryType::PersonalInfo => write!(f, "personal_info"),
            MemoryType::WorkHistory => write!(f, "work_history"),
            MemoryType::Relationship => write!(f, "relationship"),
            MemoryType::Goal => write!(f, "goal"),
            MemoryType::Conversation => write!(f, "conversation"),
        }
    }
}

impl std::str::FromStr for MemoryType {
    type Err = String;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "preference" => Ok(MemoryType::Preference),
            "entity" => Ok(MemoryType::Entity),
            "event" => Ok(MemoryType::Event),
            "case" => Ok(MemoryType::Case),
            "personal_info" => Ok(MemoryType::PersonalInfo),
            "work_history" => Ok(MemoryType::WorkHistory),
            "relationship" => Ok(MemoryType::Relationship),
            "goal" => Ok(MemoryType::Goal),
            "conversation" => Ok(MemoryType::Conversation),
            _ => Err(format!("Unknown memory type: {}", s)),
        }
    }
}

/// Memory scope enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    User,
    Agent,
    Session,
    Resources,
}

impl std::fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryScope::User => write!(f, "user"),
            MemoryScope::Agent => write!(f, "agent"),
            MemoryScope::Session => write!(f, "session"),
            MemoryScope::Resources => write!(f, "resources"),
        }
    }
}

/// Metadata for a single memory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetadata {
    /// Unique memory ID
    pub id: String,
    
    /// File path relative to scope root
    pub file: String,
    
    /// Memory type
    pub memory_type: MemoryType,
    
    /// Primary key for matching (topic for preferences, name for entities, etc.)
    pub key: String,
    
    /// Content hash for change detection
    pub content_hash: String,
    
    /// Source session IDs that contributed to this memory
    pub source_sessions: Vec<String>,
    
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    
    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
    
    /// Last access timestamp
    pub last_accessed: DateTime<Utc>,
    
    /// Access count
    pub access_count: u32,
    
    /// Confidence score (0.0 - 1.0)
    pub confidence: f32,
    
    /// Current content summary (for quick comparison)
    pub content_summary: String,
}

impl MemoryMetadata {
    /// Create a new memory metadata
    pub fn new(
        id: String,
        file: String,
        memory_type: MemoryType,
        key: String,
        content_hash: String,
        source_session: &str,
        confidence: f32,
        content_summary: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            file,
            memory_type,
            key,
            content_hash,
            source_sessions: vec![source_session.to_string()],
            created_at: now,
            updated_at: now,
            last_accessed: now,
            access_count: 0,
            confidence,
            content_summary,
        }
    }
    
    /// Update the memory with new content
    pub fn update(&mut self, content_hash: String, source_session: &str, confidence: f32, content_summary: String) {
        self.content_hash = content_hash;
        self.updated_at = Utc::now();
        self.confidence = confidence;
        self.content_summary = content_summary;
        
        // Add source session if not already present
        if !self.source_sessions.contains(&source_session.to_string()) {
            self.source_sessions.push(source_session.to_string());
        }
    }
    
    /// Record an access
    pub fn record_access(&mut self) {
        self.last_accessed = Utc::now();
        self.access_count += 1;
    }
}

/// Session extraction summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionExtractionSummary {
    /// When the extraction happened
    pub extracted_at: DateTime<Utc>,
    
    /// Memory IDs created in this session
    pub memories_created: Vec<String>,
    
    /// Memory IDs updated in this session
    pub memories_updated: Vec<String>,
}

/// Memory index for a scope (user, agent, or session)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndex {
    /// Index version
    pub version: u32,
    
    /// Scope of this index
    pub scope: MemoryScope,
    
    /// Owner ID (user_id, agent_id, or "global")
    pub owner_id: String,
    
    /// Last update timestamp
    pub last_updated: DateTime<Utc>,
    
    /// All memories in this scope
    pub memories: HashMap<String, MemoryMetadata>,
    
    /// Session extraction summaries
    pub session_summaries: HashMap<String, SessionExtractionSummary>,
}

impl MemoryIndex {
    /// Current index version
    pub const CURRENT_VERSION: u32 = 1;
    
    /// Create a new memory index
    pub fn new(scope: MemoryScope, owner_id: String) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            scope,
            owner_id,
            last_updated: Utc::now(),
            memories: HashMap::new(),
            session_summaries: HashMap::new(),
        }
    }
    
    /// Add or update a memory
    pub fn upsert_memory(&mut self, metadata: MemoryMetadata) -> bool {
        let is_new = !self.memories.contains_key(&metadata.id);
        self.memories.insert(metadata.id.clone(), metadata);
        self.last_updated = Utc::now();
        is_new
    }
    
    /// Remove a memory
    pub fn remove_memory(&mut self, memory_id: &str) -> Option<MemoryMetadata> {
        let removed = self.memories.remove(memory_id);
        if removed.is_some() {
            self.last_updated = Utc::now();
        }
        removed
    }
    
    /// Find memory by type and key
    pub fn find_by_type_and_key(&self, memory_type: &MemoryType, key: &str) -> Option<&MemoryMetadata> {
        self.memories.values().find(|m| {
            m.memory_type == *memory_type && m.key == key
        })
    }
    
    /// Find memory by type and key (mutable)
    pub fn find_by_type_and_key_mut(&mut self, memory_type: &MemoryType, key: &str) -> Option<&mut MemoryMetadata> {
        self.memories.values_mut().find(|m| {
            m.memory_type == *memory_type && m.key == key
        })
    }
    
    /// Get all memories of a specific type
    pub fn get_by_type(&self, memory_type: &MemoryType) -> Vec<&MemoryMetadata> {
        self.memories.values()
            .filter(|m| m.memory_type == *memory_type)
            .collect()
    }
    
    /// Record a session extraction
    pub fn record_session_extraction(
        &mut self,
        session_id: &str,
        created: Vec<String>,
        updated: Vec<String>,
    ) {
        self.session_summaries.insert(
            session_id.to_string(),
            SessionExtractionSummary {
                extracted_at: Utc::now(),
                memories_created: created,
                memories_updated: updated,
            },
        );
        self.last_updated = Utc::now();
    }
    
    /// Get memories from a specific session
    pub fn get_memories_from_session(&self, session_id: &str) -> Vec<&MemoryMetadata> {
        self.memories.values()
            .filter(|m| m.source_sessions.contains(&session_id.to_string()))
            .collect()
    }
    
    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.memories.is_empty()
    }
    
    /// Get memory count
    pub fn len(&self) -> usize {
        self.memories.len()
    }
}

/// Result of a memory update operation
#[derive(Debug, Clone, Default)]
pub struct MemoryUpdateResult {
    /// Number of memories created
    pub created: usize,
    
    /// Number of memories updated
    pub updated: usize,
    
    /// Number of memories deleted
    pub deleted: usize,
    
    /// IDs of created memories
    pub created_ids: Vec<String>,
    
    /// IDs of updated memories
    pub updated_ids: Vec<String>,
    
    /// IDs of deleted memories
    pub deleted_ids: Vec<String>,
}

impl MemoryUpdateResult {
    pub fn is_empty(&self) -> bool {
        self.created == 0 && self.updated == 0 && self.deleted == 0
    }
    
    pub fn total_changes(&self) -> usize {
        self.created + self.updated + self.deleted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_index_new() {
        let index = MemoryIndex::new(MemoryScope::User, "test_user".to_string());
        assert_eq!(index.version, MemoryIndex::CURRENT_VERSION);
        assert!(index.is_empty());
    }

    #[test]
    fn test_memory_metadata_new() {
        let metadata = MemoryMetadata::new(
            "pref_001".to_string(),
            "preferences/pref_001.md".to_string(),
            MemoryType::Preference,
            "programming_language".to_string(),
            "abc123".to_string(),
            "session_001",
            0.9,
            "Prefers Rust for systems programming".to_string(),
        );
        
        assert_eq!(metadata.id, "pref_001");
        assert_eq!(metadata.memory_type, MemoryType::Preference);
        assert_eq!(metadata.confidence, 0.9);
        assert_eq!(metadata.source_sessions.len(), 1);
    }

    #[test]
    fn test_find_by_type_and_key() {
        let mut index = MemoryIndex::new(MemoryScope::User, "test_user".to_string());
        
        let metadata = MemoryMetadata::new(
            "pref_001".to_string(),
            "preferences/pref_001.md".to_string(),
            MemoryType::Preference,
            "programming_language".to_string(),
            "abc123".to_string(),
            "session_001",
            0.9,
            "Prefers Rust".to_string(),
        );
        
        index.upsert_memory(metadata);
        
        let found = index.find_by_type_and_key(&MemoryType::Preference, "programming_language");
        assert!(found.is_some());
        
        let not_found = index.find_by_type_and_key(&MemoryType::Preference, "nonexistent");
        assert!(not_found.is_none());
    }
}
