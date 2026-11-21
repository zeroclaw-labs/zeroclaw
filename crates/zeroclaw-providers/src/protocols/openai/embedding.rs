//! OpenAI-compatible embeddings wire protocol, shared by vendor adapters.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub(crate) struct EmbeddingRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) input: &'a [&'a str],
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}
#[derive(Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

pub(crate) async fn vectors(
    response: reqwest::Response,
    count: usize,
    dimensions: Option<usize>,
) -> Result<Vec<Vec<f32>>> {
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!(
            "Embedding API returned HTTP {status}: {}",
            super::super::error_body(response).await
        );
    }
    let response: EmbeddingResponse = response
        .json()
        .await
        .context("decoding embedding response")?;
    ensure!(
        response.data.len() == count,
        "embedding response count differs from input count"
    );
    let mut vectors = vec![None; count];
    for item in response.data {
        ensure!(item.index < count, "embedding response index out of range");
        ensure!(
            dimensions.is_none_or(|expected| item.embedding.len() == expected),
            "embedding response dimensions differ from configured dimensions: expected {}, received {}",
            dimensions.unwrap_or_default(),
            item.embedding.len()
        );
        ensure!(
            item.embedding.iter().all(|value| value.is_finite()),
            "non-finite embedding component"
        );
        ensure!(
            vectors[item.index].replace(item.embedding).is_none(),
            "duplicate embedding response index"
        );
    }
    vectors
        .into_iter()
        .map(|vector| vector.context("missing embedding response index"))
        .collect()
}
