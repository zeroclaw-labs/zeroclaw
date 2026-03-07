use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Operation result wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
    pub timestamp: DateTime<Utc>,
}

impl<T> OperationResult<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            timestamp: Utc::now(),
        }
    }

    pub fn error(message: impl Into<String>) -> OperationResult<()> {
        OperationResult {
            success: false,
            data: None,
            error: Some(message.into()),
            timestamp: Utc::now(),
        }
    }
}

/// L0 Abstract response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbstractResponse {
    pub uri: String,
    pub abstract_text: String,
    pub layer: String, // "L0"
    pub token_count: usize,
}

/// L1 Overview response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewResponse {
    pub uri: String,
    pub overview_text: String,
    pub layer: String, // "L1"
    pub token_count: usize,
}

/// L2 Read response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResponse {
    pub uri: String,
    pub content: String,
    pub layer: String, // "L2"
    pub token_count: usize,
    pub metadata: Option<FileMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Search arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    pub recursive: Option<bool>,            // 是否递归搜索
    pub return_layers: Option<Vec<String>>, // ["L0", "L1", "L2"]
    pub scope: Option<String>,              // 搜索范围 URI
    pub limit: Option<usize>,
}

/// Search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub uri: String,
    pub score: f32,
    pub abstract_text: Option<String>, // L0
    pub overview_text: Option<String>, // L1
    pub content: Option<String>,       // L2
}

/// Search response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub results: Vec<SearchResult>,
    pub total: usize,
    pub engine_used: String,
}

/// Find arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindArgs {
    pub query: String,
    pub scope: Option<String>,
    pub limit: Option<usize>,
}

/// Find result (simple, only L0)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindResult {
    pub uri: String,
    pub abstract_text: String,
}

/// Find response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindResponse {
    pub query: String,
    pub results: Vec<FindResult>,
    pub total: usize,
}

/// List directory arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsArgs {
    #[serde(default)]
    pub uri: String,
    pub recursive: Option<bool>,
    pub include_abstracts: Option<bool>,
}

/// Directory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsEntry {
    pub name: String,
    pub uri: String,
    pub is_directory: bool,
    pub child_count: Option<usize>,
    pub abstract_text: Option<String>,
}

/// List directory response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsResponse {
    pub uri: String,
    pub entries: Vec<LsEntry>,
    pub total: usize,
}

/// Explore arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreArgs {
    pub query: String,
    pub start_uri: Option<String>,
    pub max_depth: Option<usize>,
    pub return_layers: Option<Vec<String>>,
}

/// Exploration path item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationPathItem {
    pub uri: String,
    pub relevance_score: f32,
    pub abstract_text: Option<String>,
}

/// Explore response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExploreResponse {
    pub query: String,
    pub exploration_path: Vec<ExplorationPathItem>,
    pub matches: Vec<SearchResult>,
    pub total_explored: usize,
    pub total_matches: usize,
}

/// Store arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreArgs {
    pub content: String,
    #[serde(default)]
    pub thread_id: String,
    pub metadata: Option<Value>,
    pub auto_generate_layers: Option<bool>,
    /// Storage scope: "session" (default), "user", or "agent"
    #[serde(default = "default_scope")]
    pub scope: String,
    /// User ID for user scope storage (required when scope is "user")
    #[serde(default)]
    pub user_id: Option<String>,
    /// Agent ID for agent scope storage (required when scope is "agent")
    #[serde(default)]
    pub agent_id: Option<String>,
}

fn default_scope() -> String {
    "session".to_string()
}

/// Store response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreResponse {
    pub uri: String,
    pub layers_generated: std::collections::HashMap<String, String>,
    pub success: bool,
}

// Internal types
#[derive(Debug, Clone)]
pub(crate) struct RawSearchResult {
    pub uri: String,
    pub score: f32,
}

/// Session info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub thread_id: String,
    pub status: String,
    pub message_count: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
