use crate::{
    config::QdrantConfig,
    error::Result,
    llm::LLMClientImpl,
    vector_store::{QdrantVectorStore, VectorStore},
};
use tracing::info;

/// Initialize the memory system with auto-detected embedding dimensions
/// Note: This function uses cortex_mem_config::Config from the config crate
pub async fn initialize_memory_system(
    qdrant_config: &QdrantConfig,
    llm_config: crate::llm::LLMConfig,
) -> Result<(Box<dyn VectorStore>, Box<dyn crate::llm::LLMClient>)> {
    // Create LLM client first
    let llm_client = Box::new(LLMClientImpl::new(llm_config)?) as Box<dyn crate::llm::LLMClient>;
    
    // Create vector store with auto-detection if needed
    let vector_store: Box<dyn VectorStore> = if qdrant_config.embedding_dim.is_some() {
        info!("Using configured embedding dimension: {:?}", qdrant_config.embedding_dim);
        Box::new(QdrantVectorStore::new(qdrant_config).await?)
    } else {
        info!("Auto-detecting embedding dimension...");
        Box::new(QdrantVectorStore::new_with_llm_client(qdrant_config, llm_client.as_ref()).await?)
    };
    
    Ok((vector_store, llm_client))
}

/// Create a QdrantConfig with auto-detected embedding dimension
pub async fn create_auto_config(
    base_config: &QdrantConfig,
    _llm_client: &dyn crate::llm::LLMClient,
) -> Result<QdrantConfig> {
    let mut config = base_config.clone();
    
    if config.embedding_dim.is_none() {
        info!("Auto-detecting embedding dimension for configuration...");
        // Try to get from environment variable first, then use default
        let detected_dim = std::env::var("EMBEDDING_DIM")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1536); // Default for text-embedding-3-small
        info!("Using embedding dimension: {}", detected_dim);
        config.embedding_dim = Some(detected_dim);
    }
    
    Ok(config)
}
