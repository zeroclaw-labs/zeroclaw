use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

/// Structured fact extraction target for rig extractor
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StructuredFactExtraction {
    pub facts: Vec<String>,
}

/// Detailed fact extraction with metadata
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DetailedFactExtraction {
    pub facts: Vec<StructuredFact>,
}

/// Individual structured fact with metadata
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StructuredFact {
    pub content: String,
    pub importance: f32,
    pub category: String,
    pub entities: Vec<String>,
    pub source_role: String,
}

/// Keyword extraction result
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KeywordExtraction {
    pub keywords: Vec<String>,
}

/// Memory classification result
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryClassification {
    pub memory_type: String,
    pub confidence: f32,
    pub reasoning: String,
}

/// Memory importance scoring
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportanceScore {
    pub score: f32,
    pub reasoning: String,
}

/// Memory deduplication result
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeduplicationResult {
    pub is_duplicate: bool,
    pub similarity_score: f32,
    pub original_memory_id: Option<String>,
}

/// Summary generation result
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SummaryResult {
    pub summary: String,
    pub key_points: Vec<String>,
}

/// Language detection result
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LanguageDetection {
    pub language: String,
    pub confidence: f32,
}

/// Entity extraction result
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityExtraction {
    pub entities: Vec<Entity>,
}

/// Individual extracted entity
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Entity {
    pub text: String,
    pub label: String,
    pub confidence: f32,
}

/// Conversation analysis result
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConversationAnalysis {
    pub topics: Vec<String>,
    pub sentiment: String,
    pub user_intent: String,
    pub key_information: Vec<String>,
}