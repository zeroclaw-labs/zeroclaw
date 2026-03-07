pub mod client;
pub mod extractor_types;
pub mod prompts;

pub use client::{LLMClient, LLMClientImpl, LLMConfig, MemoryExtractionResponse, ExtractedFactRaw, ExtractedDecisionRaw, ExtractedEntityRaw};
pub use extractor_types::{StructuredFactExtraction, DetailedFactExtraction, StructuredFact};
pub use prompts::Prompts;

/// Type alias for boxed LLMClient trait object
pub type BoxedLLMClient = Box<dyn LLMClient>;

/// Mock LLM Client for testing
/// 
/// This is a simple mock implementation that returns predefined responses.
/// Use this for unit tests that don't need actual LLM interaction.
pub struct MockLLMClient {
    response: String,
}

impl MockLLMClient {
    /// Create a new mock LLM client with default response
    pub fn new() -> Self {
        Self {
            response: "Mock LLM response".to_string(),
        }
    }
    
    /// Create a mock LLM client with a custom response
    pub fn with_response(response: &str) -> Self {
        Self {
            response: response.to_string(),
        }
    }
}

impl Default for MockLLMClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LLMClient for MockLLMClient {
    async fn complete(&self, _prompt: &str) -> crate::Result<String> {
        Ok(self.response.clone())
    }

    async fn complete_with_system(&self, _system: &str, _prompt: &str) -> crate::Result<String> {
        Ok(self.response.clone())
    }

    async fn extract_memories(&self, _prompt: &str) -> crate::Result<MemoryExtractionResponse> {
        Ok(MemoryExtractionResponse {
            facts: vec![],
            decisions: vec![],
            entities: vec![],
        })
    }

    async fn extract_structured_facts(&self, _prompt: &str) -> crate::Result<StructuredFactExtraction> {
        Ok(StructuredFactExtraction { facts: vec![] })
    }

    async fn extract_detailed_facts(&self, _prompt: &str) -> crate::Result<DetailedFactExtraction> {
        Ok(DetailedFactExtraction { facts: vec![] })
    }

    fn model_name(&self) -> &str {
        "mock-llm"
    }

    fn config(&self) -> &LLMConfig {
        static CONFIG: std::sync::OnceLock<LLMConfig> = std::sync::OnceLock::new();
        CONFIG.get_or_init(|| LLMConfig {
            api_base_url: String::new(),
            api_key: String::new(),
            model_efficient: "mock-llm".to_string(),
            temperature: 0.7,
            max_tokens: 2048,
        })
    }
}
