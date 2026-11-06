//! System One decision-model provider implementation.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use zeroclaw_api::decision::{Answers, DecisionModel, Question};

const DECISION_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP client for any `/v1/systemone` endpoint. Point `base_url` at
/// `https://api.typesafe.ai` for Jev or at a local `laya-serve` for Laya.
pub struct SystemOneClient {
    http: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
}

impl SystemOneClient {
    pub fn new(base_url: &str, model: &str, api_key: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(DECISION_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building decision-model HTTP client")?;
        Ok(Self {
            http,
            endpoint: format!("{}/v1/systemone", base_url.trim_end_matches('/')),
            model: model.to_string(),
            api_key: api_key.filter(|key| !key.is_empty()),
        })
    }
}

/// Construct the provider behind the provider-neutral decision contract.
/// Consumers should depend on this factory and [`DecisionModel`], rather than
/// naming the System One transport type in their own construction code.
pub fn build_decision_provider(
    base_url: &str,
    model: &str,
    api_key: Option<String>,
) -> Result<Box<dyn DecisionModel>> {
    Ok(Box::new(SystemOneClient::new(base_url, model, api_key)?))
}

#[async_trait::async_trait]
impl DecisionModel for SystemOneClient {
    fn id(&self) -> &str {
        &self.model
    }

    async fn ask(&self, state: Value, questions: BTreeMap<String, Question>) -> Result<Answers> {
        let body = json!({ "model": self.model, "state": state, "questions": questions });
        let mut request = self.http.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .context("decision model request failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("decision model returned HTTP {}", status.as_u16());
        }
        response
            .json::<Answers>()
            .await
            .context("decision model response was not a System One answer set")
    }
}
