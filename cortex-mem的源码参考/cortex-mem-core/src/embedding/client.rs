use crate::Result;
use serde::{Deserialize, Serialize};

/// Embedding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub api_base_url: String,
    pub api_key: String,
    pub model_name: String,
    pub batch_size: usize,
    pub timeout_secs: u64,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            api_base_url: std::env::var("EMBEDDING_API_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            api_key: std::env::var("EMBEDDING_API_KEY")
                .or_else(|_| std::env::var("LLM_API_KEY"))
                .unwrap_or_else(|_| "".to_string()),
            model_name: std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "text-embedding-3-small".to_string()),
            batch_size: 10,
            timeout_secs: 30,
        }
    }
}

/// Embedding client
pub struct EmbeddingClient {
    config: EmbeddingConfig,
    client: reqwest::Client,
}

impl EmbeddingClient {
    /// Create a new embedding client
    pub fn new(config: EmbeddingConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| crate::Error::Embedding(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self { config, client })
    }

    /// Embed a single text
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed_batch(&[text.to_string()]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| crate::Error::Embedding("No embedding returned".to_string()))
    }

    /// Embed a batch of texts
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        #[derive(Serialize)]
        struct EmbeddingRequest {
            input: Vec<String>,
            model: String,
        }

        #[derive(Deserialize)]
        struct EmbeddingData {
            embedding: Vec<f32>,
        }

        #[derive(Deserialize)]
        struct EmbeddingResponse {
            data: Vec<EmbeddingData>,
        }

        let request = EmbeddingRequest {
            input: texts.to_vec(),
            model: self.config.model_name.clone(),
        };

        let url = format!("{}/embeddings", self.config.api_base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| crate::Error::Embedding(format!("HTTP request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(crate::Error::Embedding(format!(
                "Embedding API error ({}): {}",
                status, text
            )));
        }

        let embedding_response: EmbeddingResponse = response
            .json()
            .await
            .map_err(|e| crate::Error::Embedding(format!("Failed to parse response: {}", e)))?;

        // 强制等待1秒以避免限流
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        Ok(embedding_response
            .data
            .into_iter()
            .map(|d| d.embedding)
            .collect())
    }

    /// Embed texts in batches
    pub async fn embed_batch_chunked(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut all_embeddings = Vec::new();

        for chunk in texts.chunks(self.config.batch_size) {
            let embeddings = self.embed_batch(chunk).await?;
            all_embeddings.extend(embeddings);
        }

        Ok(all_embeddings)
    }

    /// Get embedding dimension
    pub async fn dimension(&self) -> Result<usize> {
        let embedding = self.embed("test").await?;
        Ok(embedding.len())
    }
}
