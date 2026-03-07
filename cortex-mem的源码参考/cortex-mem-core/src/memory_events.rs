//! Memory Events Module
//!
//! Defines the event system for memory operations.
//! All memory changes flow through these events to ensure consistency.

use crate::memory_index::{MemoryScope, MemoryType};
use crate::ContextLayer;
use serde::{Deserialize, Serialize};

/// Reason for memory deletion
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeleteReason {
    /// User requested deletion
    UserRequest,
    /// Replaced by updated version
    Replaced,
    /// Source session deleted
    SourceDeleted,
    /// Conflict resolved by merge
    Merged,
}

/// Type of change for layer update events
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    Add,
    Update,
    Delete,
}

/// Memory system events
#[derive(Debug, Clone)]
pub enum MemoryEvent {
    /// A new memory was created
    MemoryCreated {
        scope: MemoryScope,
        owner_id: String,
        memory_id: String,
        memory_type: MemoryType,
        key: String,
        source_session: String,
        file_uri: String,
    },

    /// An existing memory was updated
    MemoryUpdated {
        scope: MemoryScope,
        owner_id: String,
        memory_id: String,
        memory_type: MemoryType,
        key: String,
        source_session: String,
        file_uri: String,
        old_content_hash: String,
        new_content_hash: String,
    },

    /// A memory was deleted
    MemoryDeleted {
        scope: MemoryScope,
        owner_id: String,
        memory_id: String,
        memory_type: MemoryType,
        file_uri: String,
        reason: DeleteReason,
    },

    /// A memory was accessed (for tracking usage)
    MemoryAccessed {
        scope: MemoryScope,
        owner_id: String,
        memory_id: String,
        context: String, // Query or access context
    },

    /// Layer files (L0/L1) were updated for a directory
    LayersUpdated {
        scope: MemoryScope,
        owner_id: String,
        directory_uri: String,
        layers: Vec<ContextLayer>,
    },

    /// A session was closed (triggers full memory extraction flow)
    SessionClosed {
        session_id: String,
        user_id: String,
        agent_id: String,
    },

    /// Layer update needed for a directory
    LayerUpdateNeeded {
        scope: MemoryScope,
        owner_id: String,
        directory_uri: String,
        change_type: ChangeType,
        changed_file: String,
    },

    /// Vector sync needed
    VectorSyncNeeded {
        file_uri: String,
        change_type: ChangeType,
    },
}

impl MemoryEvent {
    /// Get the scope of the event
    pub fn scope(&self) -> Option<&MemoryScope> {
        match self {
            MemoryEvent::MemoryCreated { scope, .. } => Some(scope),
            MemoryEvent::MemoryUpdated { scope, .. } => Some(scope),
            MemoryEvent::MemoryDeleted { scope, .. } => Some(scope),
            MemoryEvent::MemoryAccessed { scope, .. } => Some(scope),
            MemoryEvent::LayersUpdated { scope, .. } => Some(scope),
            MemoryEvent::LayerUpdateNeeded { scope, .. } => Some(scope),
            MemoryEvent::SessionClosed { .. } => None,
            MemoryEvent::VectorSyncNeeded { .. } => None,
        }
    }

    /// Get the owner ID for the event
    pub fn owner_id(&self) -> Option<&str> {
        match self {
            MemoryEvent::MemoryCreated { owner_id, .. } => Some(owner_id),
            MemoryEvent::MemoryUpdated { owner_id, .. } => Some(owner_id),
            MemoryEvent::MemoryDeleted { owner_id, .. } => Some(owner_id),
            MemoryEvent::MemoryAccessed { owner_id, .. } => Some(owner_id),
            MemoryEvent::LayersUpdated { owner_id, .. } => Some(owner_id),
            MemoryEvent::LayerUpdateNeeded { owner_id, .. } => Some(owner_id),
            MemoryEvent::SessionClosed { user_id, .. } => Some(user_id),
            MemoryEvent::VectorSyncNeeded { .. } => None,
        }
    }

    /// Check if this event requires layer cascade update
    pub fn requires_cascade_update(&self) -> bool {
        matches!(
            self,
            MemoryEvent::MemoryCreated { .. }
                | MemoryEvent::MemoryUpdated { .. }
                | MemoryEvent::MemoryDeleted { .. }
        )
    }

    /// Check if this event requires vector sync
    pub fn requires_vector_sync(&self) -> bool {
        matches!(
            self,
            MemoryEvent::MemoryCreated { .. }
                | MemoryEvent::MemoryUpdated { .. }
                | MemoryEvent::MemoryDeleted { .. }
                | MemoryEvent::LayersUpdated { .. }
        )
    }
}

impl std::fmt::Display for MemoryEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryEvent::MemoryCreated { memory_id, memory_type, .. } => {
                write!(f, "MemoryCreated({}, {:?})", memory_id, memory_type)
            }
            MemoryEvent::MemoryUpdated { memory_id, memory_type, .. } => {
                write!(f, "MemoryUpdated({}, {:?})", memory_id, memory_type)
            }
            MemoryEvent::MemoryDeleted { memory_id, reason, .. } => {
                write!(f, "MemoryDeleted({}, {:?})", memory_id, reason)
            }
            MemoryEvent::MemoryAccessed { memory_id, .. } => {
                write!(f, "MemoryAccessed({})", memory_id)
            }
            MemoryEvent::LayersUpdated { directory_uri, layers, .. } => {
                write!(f, "LayersUpdated({}, {:?})", directory_uri, layers)
            }
            MemoryEvent::SessionClosed { session_id, .. } => {
                write!(f, "SessionClosed({})", session_id)
            }
            MemoryEvent::LayerUpdateNeeded { directory_uri, change_type, .. } => {
                write!(f, "LayerUpdateNeeded({}, {:?})", directory_uri, change_type)
            }
            MemoryEvent::VectorSyncNeeded { file_uri, change_type } => {
                write!(f, "VectorSyncNeeded({}, {:?})", file_uri, change_type)
            }
        }
    }
}

/// Event statistics for tracking
#[derive(Debug, Clone, Default)]
pub struct EventStats {
    pub memory_created: u64,
    pub memory_updated: u64,
    pub memory_deleted: u64,
    pub memory_accessed: u64,
    pub layers_updated: u64,
    pub sessions_closed: u64,
}

impl EventStats {
    pub fn record(&mut self, event: &MemoryEvent) {
        match event {
            MemoryEvent::MemoryCreated { .. } => self.memory_created += 1,
            MemoryEvent::MemoryUpdated { .. } => self.memory_updated += 1,
            MemoryEvent::MemoryDeleted { .. } => self.memory_deleted += 1,
            MemoryEvent::MemoryAccessed { .. } => self.memory_accessed += 1,
            MemoryEvent::LayersUpdated { .. } => self.layers_updated += 1,
            MemoryEvent::SessionClosed { .. } => self.sessions_closed += 1,
            MemoryEvent::LayerUpdateNeeded { .. } => {}
            MemoryEvent::VectorSyncNeeded { .. } => {}
        }
    }

    pub fn total_events(&self) -> u64 {
        self.memory_created
            + self.memory_updated
            + self.memory_deleted
            + self.memory_accessed
            + self.layers_updated
            + self.sessions_closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_event_created() {
        let event = MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: "user_001".to_string(),
            memory_id: "pref_001".to_string(),
            memory_type: MemoryType::Preference,
            key: "programming_language".to_string(),
            source_session: "session_001".to_string(),
            file_uri: "cortex://user/user_001/preferences/pref_001.md".to_string(),
        };

        assert!(event.requires_cascade_update());
        assert!(event.requires_vector_sync());
        assert_eq!(event.scope(), Some(&MemoryScope::User));
    }

    #[test]
    fn test_memory_event_session_closed() {
        let event = MemoryEvent::SessionClosed {
            session_id: "session_001".to_string(),
            user_id: "user_001".to_string(),
            agent_id: "agent_001".to_string(),
        };

        assert!(!event.requires_cascade_update());
        assert!(!event.requires_vector_sync());
    }

    #[test]
    fn test_event_stats() {
        let mut stats = EventStats::default();
        
        stats.record(&MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: "user_001".to_string(),
            memory_id: "pref_001".to_string(),
            memory_type: MemoryType::Preference,
            key: "test".to_string(),
            source_session: "s1".to_string(),
            file_uri: "uri".to_string(),
        });
        
        stats.record(&MemoryEvent::MemoryUpdated {
            scope: MemoryScope::User,
            owner_id: "user_001".to_string(),
            memory_id: "pref_001".to_string(),
            memory_type: MemoryType::Preference,
            key: "test".to_string(),
            source_session: "s2".to_string(),
            file_uri: "uri".to_string(),
            old_content_hash: "old".to_string(),
            new_content_hash: "new".to_string(),
        });
        
        assert_eq!(stats.memory_created, 1);
        assert_eq!(stats.memory_updated, 1);
        assert_eq!(stats.total_events(), 2);
    }
}
