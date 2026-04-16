//! OpenAI-compatible HTTP embedder.
//!
//! Handles OpenAI cloud, OpenRouter, and any self-hosted service that speaks
//! the `POST /v1/embeddings` JSON shape. Provider family is injected by the
//! factory so the same struct can serve as both `openai` and `custom_http`
//! under distinct metadata.

use async_trait::async_trait;

use super::{EmbeddingProvider, EMBEDDING_SCHEMA_VERSION};

/// OpenAI-compatible embedder. Pairs with a `provider_family` label stored in
/// `vault_documents.embedding_provider` so downstream code can distinguish
/// direct OpenAI usage from self-hosted custom endpoints.
pub struct OpenAiEmbedding {
    pub(super) base_url: String,
    api_key: String,
    model: String,
    dims: usize,
    provider_family: &'static str,
}

impl OpenAiEmbedding {
    /// `provider_family` MUST be one of the `PROVIDER_*` constants from the
    /// module root — callers pass `PROVIDER_OPENAI` for OpenAI/OpenRouter or
    /// `PROVIDER_CUSTOM_HTTP` for user-supplied endpoints.
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        dims: usize,
        provider_family: &'static str,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            dims,
            provider_family,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("memory.embeddings")
    }

    fn has_explicit_api_path(&self) -> bool {
        let Ok(url) = reqwest::Url::parse(&self.base_url) else {
            return false;
        };

        let path = url.path().trim_end_matches('/');
        !path.is_empty() && path != "/"
    }

    fn has_embeddings_endpoint(&self) -> bool {
        let Ok(url) = reqwest::Url::parse(&self.base_url) else {
            return false;
        };

        url.path().trim_end_matches('/').ends_with("/embeddings")
    }

    pub(super) fn embeddings_url(&self) -> String {
        if self.has_embeddings_endpoint() {
            return self.base_url.clone();
        }

        if self.has_explicit_api_path() {
            format!("{}/embeddings", self.base_url)
        } else {
            format!("{}/v1/embeddings", self.base_url)
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedding {
    fn name(&self) -> &str {
        self.provider_family
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn version(&self) -> u32 {
        EMBEDDING_SCHEMA_VERSION
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let resp = self
            .http_client()
            .post(self.embeddings_url())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Embedding API error {status}: {text}");
        }

        let json: serde_json::Value = resp.json().await?;
        let data = json
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid embedding response: missing 'data'"))?;

        let mut embeddings = Vec::with_capacity(data.len());
        for item in data {
            let embedding = item
                .get("embedding")
                .and_then(|e| e.as_array())
                .ok_or_else(|| anyhow::anyhow!("Invalid embedding item"))?;

            #[allow(clippy::cast_possible_truncation)]
            let vec: Vec<f32> = embedding
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();

            embeddings.push(vec);
        }

        Ok(embeddings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::PROVIDER_OPENAI;

    fn openai(base: &str) -> OpenAiEmbedding {
        OpenAiEmbedding::new(base, "key", "text-embedding-3-small", 1536, PROVIDER_OPENAI)
    }

    #[test]
    fn trailing_slash_stripped() {
        let p = openai("https://api.openai.com/");
        assert_eq!(p.base_url, "https://api.openai.com");
    }

    #[test]
    fn dimensions_and_model_exposed() {
        let p = openai("http://localhost");
        assert_eq!(p.dimensions(), 1536);
        assert_eq!(p.model(), "text-embedding-3-small");
    }

    #[test]
    fn standard_openai_url() {
        assert_eq!(
            openai("https://api.openai.com").embeddings_url(),
            "https://api.openai.com/v1/embeddings"
        );
    }

    #[test]
    fn openrouter_url() {
        assert_eq!(
            openai("https://openrouter.ai/api/v1").embeddings_url(),
            "https://openrouter.ai/api/v1/embeddings"
        );
    }

    #[test]
    fn explicit_v1_not_duplicated() {
        assert_eq!(
            openai("https://api.example.com/v1").embeddings_url(),
            "https://api.example.com/v1/embeddings"
        );
    }

    #[test]
    fn non_v1_api_path_preserved() {
        assert_eq!(
            openai("https://api.example.com/api/coding/v3").embeddings_url(),
            "https://api.example.com/api/coding/v3/embeddings"
        );
    }

    #[test]
    fn full_embeddings_endpoint_preserved() {
        assert_eq!(
            openai("https://my-api.example.com/api/v2/embeddings").embeddings_url(),
            "https://my-api.example.com/api/v2/embeddings"
        );
    }

    #[test]
    fn provider_family_label_is_used_for_name() {
        let openai_family = OpenAiEmbedding::new("http://x", "k", "m", 1, PROVIDER_OPENAI);
        assert_eq!(openai_family.name(), PROVIDER_OPENAI);

        let custom =
            OpenAiEmbedding::new("http://x", "k", "m", 1, super::super::PROVIDER_CUSTOM_HTTP);
        assert_eq!(custom.name(), super::super::PROVIDER_CUSTOM_HTTP);
    }
}
