use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Dimension of memory storage
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dimension {
    /// Resource-specific memories (facts, knowledge)
    Resources,
    /// User-specific memories
    User,
    /// Agent-specific memories
    Agent,
    /// Session/conversation memories
    Session,
}

impl Dimension {
    pub fn as_str(&self) -> &'static str {
        match self {
            Dimension::Resources => "resources",
            Dimension::User => "user",
            Dimension::Agent => "agent",
            Dimension::Session => "session",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "resources" => Some(Dimension::Resources),
            "user" => Some(Dimension::User),
            "agent" => Some(Dimension::Agent),
            "session" => Some(Dimension::Session),
            _ => None,
        }
    }
}

/// Context layer for tiered loading
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextLayer {
    /// L0: Abstract (~100 tokens)
    L0Abstract,
    /// L1: Overview (~500-2000 tokens)
    L1Overview,
    /// L2: Full detail
    L2Detail,
}

impl ContextLayer {
    pub fn filename(&self) -> &'static str {
        match self {
            ContextLayer::L0Abstract => ".abstract.md",
            ContextLayer::L1Overview => ".overview.md",
            ContextLayer::L2Detail => "",
        }
    }

    pub fn max_tokens(&self) -> usize {
        match self {
            ContextLayer::L0Abstract => 100,
            ContextLayer::L1Overview => 2000,
            ContextLayer::L2Detail => usize::MAX,
        }
    }
}

/// File entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub uri: String,
    pub name: String,
    pub is_directory: bool,
    pub size: u64,
    pub modified: DateTime<Utc>,
}

/// File metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub size: u64,
    pub is_directory: bool,
}

/// Memory metadata for vector store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetadata {
    pub uri: Option<String>,
    pub user_id: Option<String>,
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
    pub actor_id: Option<String>,
    pub role: Option<String>,
    /// Layer: L0, L1, or L2
    pub layer: String,
    pub hash: String,
    pub importance_score: f32,
    pub entities: Vec<String>,
    pub topics: Vec<String>,
    pub custom: HashMap<String, serde_json::Value>,
}

impl Default for MemoryMetadata {
    fn default() -> Self {
        Self {
            uri: None,
            user_id: None,
            agent_id: None,
            run_id: None,
            actor_id: None,
            role: None,
            layer: "L2".to_string(),
            hash: String::new(),
            importance_score: 0.5,
            entities: Vec::new(),
            topics: Vec::new(),
            custom: HashMap::new(),
        }
    }
}

/// User memory category
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UserMemoryCategory {
    /// User profile (appendable)
    Profile,
    /// User preferences by topic
    Preferences,
    /// Entity memories (people, projects)
    Entities,
    /// Event records (decisions, milestones)
    Events,
}

impl UserMemoryCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            UserMemoryCategory::Profile => "profile",
            UserMemoryCategory::Preferences => "preferences",
            UserMemoryCategory::Entities => "entities",
            UserMemoryCategory::Events => "events",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "profile" => Some(UserMemoryCategory::Profile),
            "preferences" => Some(UserMemoryCategory::Preferences),
            "entities" => Some(UserMemoryCategory::Entities),
            "events" => Some(UserMemoryCategory::Events),
            _ => None,
        }
    }
}

/// Agent memory category
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentMemoryCategory {
    /// Problem + solution cases
    Cases,
    /// Skills
    Skills,
    /// Instructions
    Instructions,
}

impl AgentMemoryCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentMemoryCategory::Cases => "cases",
            AgentMemoryCategory::Skills => "skills",
            AgentMemoryCategory::Instructions => "instructions",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "cases" => Some(AgentMemoryCategory::Cases),
            "skills" => Some(AgentMemoryCategory::Skills),
            "instructions" => Some(AgentMemoryCategory::Instructions),
            _ => None,
        }
    }
}

/// Memory struct (for vector store)
#[derive(Debug, Clone)]
pub struct Memory {
    pub id: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: MemoryMetadata,
}

/// Scored memory (search result)
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub memory: Memory,
    pub score: f32,
}

/// Filters for memory search
#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub user_id: Option<String>,
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
    /// Layer filter: L0, L1, or L2
    pub layer: Option<String>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub updated_after: Option<DateTime<Utc>>,
    pub updated_before: Option<DateTime<Utc>>,
    pub topics: Option<Vec<String>>,
    pub entities: Option<Vec<String>>,
    pub min_importance: Option<f32>,
    pub max_importance: Option<f32>,
    /// URI prefix filter for scope-based searching
    pub uri_prefix: Option<String>,
    pub custom: HashMap<String, serde_json::Value>,
}

impl Filters {
    /// Add a custom filter field
    pub fn add_custom(&mut self, key: &str, value: impl Into<serde_json::Value>) {
        self.custom.insert(key.to_string(), value.into());
    }

    /// Create filters with a specific layer
    pub fn with_layer(layer: &str) -> Self {
        Self {
            layer: Some(layer.to_string()),
            ..Default::default()
        }
    }
}
