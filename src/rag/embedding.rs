//! NVIDIA NIM embedding client for the RAG pipeline.
//!
//! The NVIDIA NIM embedding API is OpenAI-compatible but requires an
//! `input_type` field to select between passage (document ingest) and query
//! (retrieval) encodings.  The [`NvidiaEmbeddingClient`] exposes distinct
//! methods for each so callers cannot accidentally mix them up.
//!
//! Requests are batched in groups of [`BATCH_SIZE`] inputs and retried with
//! exponential back-off on transient failures.

use anyhow::{bail, Result};

/// Default NVIDIA NIM embedding endpoint.
const NVIDIA_EMBEDDING_URL: &str = "https://integrate.api.nvidia.com/v1/embeddings";

/// Maximum inputs per API request (NVIDIA NIM limit).
const BATCH_SIZE: usize = 50;

/// Number of retry attempts on transient failure (not counting the first try).
const MAX_RETRIES: u32 = 3;

// ── Client ─────────────────────────────────────────────────────────────────────

/// HTTP client for the NVIDIA NIM embeddings endpoint.
///
/// Instantiate once and share across ingest and retrieval calls.
pub struct NvidiaEmbeddingClient {
    api_key: String,
    model: String,
    /// Override endpoint (for testing / private NIM deployments).
    endpoint: String,
}

impl NvidiaEmbeddingClient {
    /// Create a client using the default NVIDIA NIM endpoint.
    pub fn new(api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            endpoint: NVIDIA_EMBEDDING_URL.to_string(),
        }
    }

    /// Create a client with a custom endpoint (primarily for tests).
    pub fn with_endpoint(api_key: &str, model: &str, endpoint: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            endpoint: endpoint.to_string(),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("memory.embeddings")
    }

    // ── Public API ─────────────────────────────────────────────────────────────

    /// Embed document passages for storage.
    ///
    /// Uses `input_type = "passage"`, which is required for accurate retrieval
    /// with `nvidia/nv-embedqa-e5-v5`.
    pub async fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_batched(texts, "passage").await
    }

    /// Embed a user query for retrieval.
    ///
    /// Uses `input_type = "query"`.
    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut results = self.embed_batched(&[text], "query").await?;
        results
            .pop()
            .ok_or_else(|| anyhow::anyhow!("empty embedding result from NVIDIA NIM"))
    }

    // ── Internal ───────────────────────────────────────────────────────────────

    async fn embed_batched(&self, texts: &[&str], input_type: &str) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut all = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH_SIZE) {
            let embeddings = self.embed_batch_with_retry(batch, input_type).await?;
            all.extend(embeddings);
        }
        Ok(all)
    }

    async fn embed_batch_with_retry(
        &self,
        texts: &[&str],
        input_type: &str,
    ) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
            "input_type": input_type,
            "encoding_format": "float",
            "truncate": "END",
        });

        let mut delay_secs = 1u64;
        let mut last_err = anyhow::anyhow!("no attempts made");

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                delay_secs = delay_secs.saturating_mul(2);
                tracing::warn!(
                    attempt,
                    max_retries = MAX_RETRIES,
                    "retrying NVIDIA embedding request: {last_err}"
                );
            }

            match self.send_request(&body).await {
                Ok(result) => return Ok(result),
                Err(e) => last_err = e,
            }
        }

        Err(last_err)
    }

    async fn send_request(&self, body: &serde_json::Value) -> Result<Vec<Vec<f32>>> {
        let resp = self
            .http_client()
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("NVIDIA NIM embedding API error {status}: {text}");
        }

        let json: serde_json::Value = resp.json().await?;
        parse_embedding_response(&json)
    }
}

// ── Response parsing ───────────────────────────────────────────────────────────

fn parse_embedding_response(json: &serde_json::Value) -> Result<Vec<Vec<f32>>> {
    let data = json.get("data").and_then(|d| d.as_array()).ok_or_else(|| {
        anyhow::anyhow!("invalid NVIDIA embedding response: missing 'data' array")
    })?;

    let mut embeddings = Vec::with_capacity(data.len());
    for (i, item) in data.iter().enumerate() {
        let raw = item
            .get("embedding")
            .and_then(|e| e.as_array())
            .ok_or_else(|| {
                anyhow::anyhow!("invalid embedding response: item {i} missing 'embedding' array")
            })?;

        #[allow(clippy::cast_possible_truncation)]
        let vec: Vec<f32> = raw
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();

        embeddings.push(vec);
    }

    Ok(embeddings)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_stores_api_key_and_model() {
        let c = NvidiaEmbeddingClient::new("test-key", "nvidia/nv-embedqa-e5-v5");
        assert_eq!(c.api_key, "test-key");
        assert_eq!(c.model, "nvidia/nv-embedqa-e5-v5");
        assert_eq!(c.endpoint, NVIDIA_EMBEDDING_URL);
    }

    #[test]
    fn with_endpoint_overrides_url() {
        let c = NvidiaEmbeddingClient::with_endpoint("k", "m", "http://localhost:9999");
        assert_eq!(c.endpoint, "http://localhost:9999");
    }

    #[test]
    fn parse_embedding_response_valid() {
        let json = serde_json::json!({
            "data": [
                {"embedding": [0.1, 0.2, 0.3]},
                {"embedding": [0.4, 0.5, 0.6]},
            ]
        });
        let result = parse_embedding_response(&json).unwrap();
        assert_eq!(result.len(), 2);
        assert!((result[0][0] - 0.1f32).abs() < 1e-5);
        assert!((result[1][2] - 0.6f32).abs() < 1e-5);
    }

    #[test]
    fn parse_embedding_response_missing_data() {
        let json = serde_json::json!({"model": "test"});
        assert!(parse_embedding_response(&json).is_err());
    }

    #[test]
    fn parse_embedding_response_missing_embedding_field() {
        let json = serde_json::json!({
            "data": [{"index": 0}]
        });
        assert!(parse_embedding_response(&json).is_err());
    }

    #[test]
    fn parse_embedding_response_empty_data() {
        let json = serde_json::json!({"data": []});
        let result = parse_embedding_response(&json).unwrap();
        assert!(result.is_empty());
    }
}
