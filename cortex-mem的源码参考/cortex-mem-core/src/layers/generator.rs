use crate::{Result, llm::LLMClient};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, debug};

/// Abstract (L0) generator
/// 
/// Generates a concise summary (~100 tokens) from content using LLM
/// for quick relevance checking and filtering
pub struct AbstractGenerator;

impl AbstractGenerator {
    pub fn new() -> Self {
        Self
    }
    
    /// Generate abstract from content using LLM (mandatory)
    pub async fn generate_with_llm(&self, content: &str, llm: &Arc<dyn LLMClient>) -> Result<String> {
        info!("📝 生成 L0 Abstract (内容长度: {} 字符)", content.len());
        
        let system = r#"You are an expert at creating concise abstracts.
Your goal is to generate summaries that capture multiple key aspects of content for quick relevance checking.
Keep abstracts under 100 tokens. Prioritize breadth over depth - cover more topics briefly rather than elaborating on one.
Be direct and informative. Use compact phrasing to maximize information density."#;
        
        let prompt = crate::llm::prompts::Prompts::abstract_generation(content);
        debug!("L0 Abstract prompt 长度: {} 字符", prompt.len());
        
        let result = llm.complete_with_system(system, &prompt).await?;
        
        info!("✅ L0 Abstract 生成完成 ({} 字符)", result.len());
        Ok(result)
    }
    
    /// Estimate token count (rough approximation)
    pub fn estimate_tokens(text: &str) -> usize {
        text.len() / 3
    }
}

/// Overview (L1) generator
/// 
/// Generates structured overview (~500-2000 tokens) from content using LLM
pub struct OverviewGenerator;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overview {
    pub core_topics: Vec<String>,
    pub key_points: Vec<String>,
    pub entities: Vec<String>,
    pub summary: String,
}

impl OverviewGenerator {
    pub fn new() -> Self {
        Self
    }
    
    /// Generate overview from content using LLM (mandatory)
    pub async fn generate_with_llm(&self, content: &str, llm: &Arc<dyn LLMClient>) -> Result<String> {
        info!("📄 生成 L1 Overview (内容长度: {} 字符)", content.len());
        
        let system = r#"You are an expert at creating structured overviews.
Your goal is to provide comprehensive yet concise summaries (500-2000 tokens) that help users understand and make decisions about content.
Use clear markdown structure with sections for Summary, Core Topics, Key Points, Entities, and Context."#;
        
        let prompt = crate::llm::prompts::Prompts::overview_generation(content);
        debug!("L1 Overview prompt 长度: {} 字符", prompt.len());
        
        let result = llm.complete_with_system(system, &prompt).await?;
        
        info!("✅ L1 Overview 生成完成 ({} 字符)", result.len());
        Ok(result)
    }
}