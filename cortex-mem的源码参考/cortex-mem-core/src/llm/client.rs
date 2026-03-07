use crate::Result;
use rig::providers::openai::Client;
use serde::{Deserialize, Serialize};

/// LLM configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMConfig {
    pub api_base_url: String,
    pub api_key: String,
    pub model_efficient: String,
    pub temperature: f32,
    pub max_tokens: usize,
}

impl Default for LLMConfig {
    fn default() -> Self {
        Self {
            api_base_url: std::env::var("LLM_API_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            api_key: std::env::var("LLM_API_KEY")
                .unwrap_or_else(|_| "".to_string()),
            model_efficient: std::env::var("LLM_MODEL")
                .unwrap_or_else(|_| "gpt-3.5-turbo".to_string()),
            temperature: 0.1,
            max_tokens: 4096,
        }
    }
}

/// Memory extraction response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryExtractionResponse {
    pub facts: Vec<ExtractedFactRaw>,
    pub decisions: Vec<ExtractedDecisionRaw>,
    pub entities: Vec<ExtractedEntityRaw>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFactRaw {
    pub content: String,
    #[serde(default)]
    pub subject: Option<String>,
    pub confidence: f32,
    #[serde(default)]
    pub importance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedDecisionRaw {
    pub decision: String,
    pub context: String,
    pub rationale: Option<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntityRaw {
    pub name: String,
    pub entity_type: String,
    pub description: Option<String>,
    pub confidence: f32,
}

/// LLM Client trait for dependency injection and testing
#[async_trait::async_trait]
pub trait LLMClient: Send + Sync {
    /// Simple completion without tools or streaming
    async fn complete(&self, prompt: &str) -> Result<String>;
    
    /// Generate completion with system message
    async fn complete_with_system(&self, system: &str, prompt: &str) -> Result<String>;
    
    /// Extract memories from conversation
    async fn extract_memories(&self, prompt: &str) -> Result<MemoryExtractionResponse>;
    
    /// Extract structured facts using rig extractor
    async fn extract_structured_facts(&self, prompt: &str) -> Result<crate::llm::extractor_types::StructuredFactExtraction>;
    
    /// Extract detailed facts using rig extractor
    async fn extract_detailed_facts(&self, prompt: &str) -> Result<crate::llm::extractor_types::DetailedFactExtraction>;
    
    /// Get the model name
    fn model_name(&self) -> &str;
    
    /// Get the config
    fn config(&self) -> &LLMConfig;
}

/// LLM Client wrapper for rig-core
/// 
/// This is a lightweight wrapper that creates agents for LLM interactions.
/// Following the rig pattern: Client -> CompletionModel -> Agent
pub struct LLMClientImpl {
    client: Client,
    config: LLMConfig,
}

impl LLMClientImpl {
    /// Create a new LLM client
    /// 
    /// Note: For rig-core 0.31.0, we use Client::builder() pattern
    /// with custom base URL configuration through .base_url() method
    pub fn new(config: LLMConfig) -> Result<Self> {
        // Using Client::builder pattern - compatible with rig-core 0.31.0
        let client = Client::builder()
            .api_key(&config.api_key)
            .base_url(&config.api_base_url)
            .build()
            .map_err(|e| crate::Error::Llm(format!("Failed to build OpenAI client: {:?}", e)))?;

        Ok(Self { client, config })
    }

    /// Create a default LLM config
    pub fn default_config() -> LLMConfig {
        LLMConfig::default()
    }

    /// Create an agent with a system prompt
    /// 
    /// This is the recommended way to interact with LLMs in rig-core.
    /// Returns an Agent that can handle streaming and tool calls.
    /// 
    /// Note: In rig-core 0.31.0, the default agent uses ResponsesCompletionModel.
    /// We use .completions_api() to get the traditional CompletionModel.
    pub async fn create_agent(&self, system_prompt: &str) -> Result<rig::agent::Agent<rig::providers::openai::CompletionModel>> {
        use rig::client::CompletionClient;
        
        // Clone the client to avoid moving out of self
        let agent = self.client.clone()
            .completions_api()  // Use completions API to get CompletionModel
            .agent(&self.config.model_efficient)
            .preamble(system_prompt)
            .build();
            
        Ok(agent)
    }

    /// Simple completion without tools or streaming
    /// For basic use cases - creates a temporary agent
    pub async fn complete(&self, prompt: &str) -> Result<String> {
        use rig::completion::Prompt;
        
        tracing::info!("🔄 LLM 调用开始 [模型: {}]", self.config.model_efficient);
        tracing::debug!("📝 Prompt 长度: {} 字符", prompt.len());
        
        let start = std::time::Instant::now();
        
        let agent = self.create_agent("You are a helpful assistant.").await?;
        let response = agent
            .prompt(prompt)
            .await
            .map_err(|e| crate::Error::Llm(format!("LLM completion failed: {}", e)))?;

        let elapsed = start.elapsed();
        tracing::info!("✅ LLM 调用完成 [耗时: {:.2}s, 响应: {} 字符]", elapsed.as_secs_f64(), response.len());
        
        Ok(response)
    }

    /// Generate completion with system message
    pub async fn complete_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        use rig::completion::Prompt;
        
        tracing::info!("🔄 LLM 调用开始 (with system) [模型: {}]", self.config.model_efficient);
        tracing::debug!("📝 System: {}..., Prompt 长度: {} 字符", 
            &system.chars().take(50).collect::<String>(), prompt.len());
        
        let start = std::time::Instant::now();
        
        let agent = self.create_agent(system).await?;
        let response = agent
            .prompt(prompt)
            .await
            .map_err(|e| crate::Error::Llm(format!("LLM completion failed: {}", e)))?;
            
        let elapsed = start.elapsed();
        tracing::info!("✅ LLM 调用完成 [耗时: {:.2}s, 响应: {} 字符]", elapsed.as_secs_f64(), response.len());
        
        Ok(response)
    }

    /// Extract memories from conversation
    pub async fn extract_memories(&self, prompt: &str) -> Result<MemoryExtractionResponse> {
        let response: String = self.complete(prompt).await?;
        
        // Extract JSON from response (handles markdown code blocks)
        let json_str = Self::extract_json_from_response(&response);
        
        // Try to parse as structured response first
        if let Ok(extracted) = serde_json::from_str::<MemoryExtractionResponse>(json_str) {
            tracing::debug!("Successfully parsed MemoryExtractionResponse");
            return Ok(extracted);
        }
        
        // Try to parse as just an array of facts (fallback)
        if let Ok(facts) = serde_json::from_str::<Vec<ExtractedFactRaw>>(json_str) {
            tracing::debug!("Parsed as facts array, found {} facts", facts.len());
            return Ok(MemoryExtractionResponse {
                facts,
                decisions: Vec::new(),
                entities: Vec::new(),
            });
        }
        
        // Try to parse as just an array of decisions (fallback)
        if let Ok(decisions) = serde_json::from_str::<Vec<ExtractedDecisionRaw>>(json_str) {
            tracing::debug!("Parsed as decisions array, found {} decisions", decisions.len());
            return Ok(MemoryExtractionResponse {
                facts: Vec::new(),
                decisions,
                entities: Vec::new(),
            });
        }
        
        // Try to parse as just an array of entities (fallback)
        if let Ok(entities) = serde_json::from_str::<Vec<ExtractedEntityRaw>>(json_str) {
            tracing::debug!("Parsed as entities array, found {} entities", entities.len());
            return Ok(MemoryExtractionResponse {
                facts: Vec::new(),
                decisions: Vec::new(),
                entities,
            });
        }
        
        eprintln!("[DEBUG] Failed to parse JSON, returning empty extraction");
        // If all parsing fails, return empty extraction
        Ok(MemoryExtractionResponse {
            facts: Vec::new(),
            decisions: Vec::new(),
            entities: Vec::new(),
        })
    }

    /// Get the underlying rig Client
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Extract JSON from LLM response, handling markdown code blocks
    fn extract_json_from_response(response: &str) -> &str {
        let trimmed = response.trim();
        
        // If response is wrapped in ```json ... ``` or ``` ... ```
        if trimmed.starts_with("```json") {
            if let Some(json_start) = trimmed.find('\n') {
                let rest = &trimmed[json_start + 1..];
                if let Some(end) = rest.find("```") {
                    return rest[..end].trim();
                }
                return rest.trim();
            }
        } else if trimmed.starts_with("```") {
            if let Some(json_start) = trimmed.find('\n') {
                let rest = &trimmed[json_start + 1..];
                if let Some(end) = rest.find("```") {
                    return rest[..end].trim();
                }
                return rest.trim();
            }
        }
        
        // Try to find JSON object boundaries
        if let Some(start) = trimmed.find('{') {
            let mut depth = 0;
            for (i, c) in trimmed[start..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &trimmed[start..start + i + 1];
                        }
                    }
                    _ => {}
                }
            }
        }
        
        // Return as-is if no special handling needed
        trimmed
    }
}

#[async_trait::async_trait]
impl LLMClient for LLMClientImpl {
    async fn complete(&self, prompt: &str) -> Result<String> {
        use rig::completion::Prompt;
        
        tracing::info!("🔄 LLM 调用开始 [模型: {}]", self.config.model_efficient);
        tracing::debug!("📝 Prompt 长度: {} 字符", prompt.len());
        
        let start = std::time::Instant::now();
        
        let agent = self.create_agent("You are a helpful assistant.").await?;
        let response = agent
            .prompt(prompt)
            .await
            .map_err(|e| crate::Error::Llm(format!("LLM completion failed: {}", e)))?;

        let elapsed = start.elapsed();
        tracing::info!("✅ LLM 调用完成 [耗时: {:.2}s, 响应: {} 字符]", elapsed.as_secs_f64(), response.len());

        Ok(response)
    }

    async fn complete_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        use rig::completion::Prompt;
        
        tracing::info!("🔄 LLM 调用开始 (with system) [模型: {}]", self.config.model_efficient);
        tracing::debug!("📝 System: {}..., Prompt 长度: {} 字符", 
            &system.chars().take(50).collect::<String>(), prompt.len());
        
        let start = std::time::Instant::now();
        
        let agent = self.create_agent(system).await?;
        let response = agent
            .prompt(prompt)
            .await
            .map_err(|e| crate::Error::Llm(format!("LLM completion failed: {}", e)))?;
            
        let elapsed = start.elapsed();
        tracing::info!("✅ LLM 调用完成 [耗时: {:.2}s, 响应: {} 字符]", elapsed.as_secs_f64(), response.len());
            
        Ok(response)
    }

    async fn extract_memories(&self, prompt: &str) -> Result<MemoryExtractionResponse> {
        let response: String = self.complete(prompt).await?;
        
        // Extract JSON from response (handles markdown code blocks)
        let json_str = Self::extract_json_from_response(&response);
        
        // Try to parse as structured response first
        if let Ok(extracted) = serde_json::from_str::<MemoryExtractionResponse>(json_str) {
            return Ok(extracted);
        }
        
        // Try to parse as just an array of facts (fallback)
        if let Ok(facts) = serde_json::from_str::<Vec<ExtractedFactRaw>>(json_str) {
            return Ok(MemoryExtractionResponse {
                facts,
                decisions: Vec::new(),
                entities: Vec::new(),
            });
        }
        
        // Try to parse as just an array of decisions (fallback)
        if let Ok(decisions) = serde_json::from_str::<Vec<ExtractedDecisionRaw>>(json_str) {
            return Ok(MemoryExtractionResponse {
                facts: Vec::new(),
                decisions,
                entities: Vec::new(),
            });
        }
        
        // Try to parse as just an array of entities (fallback)
        if let Ok(entities) = serde_json::from_str::<Vec<ExtractedEntityRaw>>(json_str) {
            return Ok(MemoryExtractionResponse {
                facts: Vec::new(),
                decisions: Vec::new(),
                entities,
            });
        }
        
        // If all parsing fails, return empty extraction
        Ok(MemoryExtractionResponse {
            facts: Vec::new(),
            decisions: Vec::new(),
            entities: Vec::new(),
        })
    }

    async fn extract_structured_facts(&self, prompt: &str) -> Result<crate::llm::extractor_types::StructuredFactExtraction> {
        // Build a structured extraction prompt that guides the LLM to return valid JSON
        let extraction_prompt = format!(
            r#"Extract factual information from the text below.

## Instructions
1. Identify all factual statements that can be verified
2. Focus on concrete facts, not opinions or speculations
3. Each fact should be a single, atomic statement

## Output Format
Return ONLY a valid JSON object with this exact structure:
{{
  "facts": ["fact 1", "fact 2", "fact 3"]
}}

If no facts are found, return: {{"facts": []}}

## Text to Analyze
{}

## Response (JSON only)"#,
            prompt
        );

        let response = self.complete(&extraction_prompt).await?;
        
        // Try to extract JSON from the response
        let json_str = Self::extract_json_from_response(&response);
        
        // Try to parse as structured facts
        match serde_json::from_str::<crate::llm::extractor_types::StructuredFactExtraction>(json_str) {
            Ok(facts) => Ok(facts),
            Err(e) => {
                tracing::warn!("Failed to parse structured facts: {}. Response: {}", e, json_str);
                // Fallback: return empty extraction
                Ok(crate::llm::extractor_types::StructuredFactExtraction {
                    facts: vec![],
                })
            }
        }
    }

    async fn extract_detailed_facts(&self, prompt: &str) -> Result<crate::llm::extractor_types::DetailedFactExtraction> {
        // Build a detailed extraction prompt
        let extraction_prompt = format!(
            r#"Extract detailed factual information from the text below.

## Instructions
1. Identify all factual statements that can be verified
2. For each fact, determine:
   - content: The factual statement
   - importance: A score from 0.0 to 1.0 (how important/relevant is this fact)
   - category: One of "personal", "work", "preference", "event", "knowledge", "other"
   - entities: List of named entities mentioned (people, places, organizations, etc.)
   - source_role: Either "user", "assistant", or "system" (who stated this fact)

## Output Format
Return ONLY a valid JSON object with this exact structure:
{{
  "facts": [
    {{
      "content": "The factual statement",
      "importance": 0.8,
      "category": "personal",
      "entities": ["John", "New York"],
      "source_role": "user"
    }}
  ]
}}

If no facts are found, return: {{"facts": []}}

## Text to Analyze
{}

## Response (JSON only)"#,
            prompt
        );

        let response = self.complete(&extraction_prompt).await?;
        
        // Try to extract JSON from the response
        let json_str = Self::extract_json_from_response(&response);
        
        // Try to parse as detailed facts
        match serde_json::from_str::<crate::llm::extractor_types::DetailedFactExtraction>(json_str) {
            Ok(facts) => Ok(facts),
            Err(e) => {
                tracing::warn!("Failed to parse detailed facts: {}. Response: {}", e, json_str);
                // Fallback: return empty extraction
                Ok(crate::llm::extractor_types::DetailedFactExtraction {
                    facts: vec![],
                })
            }
        }
    }

    fn model_name(&self) -> &str {
        &self.config.model_efficient
    }

    fn config(&self) -> &LLMConfig {
        &self.config
    }
}

// 核心功能测试已迁移至 cortex-mem-tools/tests/core_functionality_tests.rs
