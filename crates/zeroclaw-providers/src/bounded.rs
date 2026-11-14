//! Admission at the generation transport boundary. Ordinary calls keep their
//! existing clients and recovery policy by passing no budget.

use anyhow::Context as _;
use reqwest::{Client, ClientBuilder, RequestBuilder, Response};
use zeroclaw_api::model_provider::GenerationAttemptBudget;

pub(crate) fn client(builder: ClientBuilder, service_key: Option<&str>) -> anyhow::Result<Client> {
    let builder = builder
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none());
    let builder = match service_key {
        Some(service_key) => {
            zeroclaw_config::schema::try_apply_runtime_proxy_to_builder(builder, service_key)?
        }
        None => builder,
    };
    builder
        .build()
        .context("Failed to build bounded provider HTTP client")
}

pub(crate) fn timeout_client(service_key: &str, timeout_secs: u64) -> anyhow::Result<Client> {
    client(
        Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .connect_timeout(std::time::Duration::from_secs(10)),
        Some(service_key),
    )
}

pub(crate) async fn send(
    request: RequestBuilder,
    budget: Option<&GenerationAttemptBudget>,
    previous_error: Option<anyhow::Error>,
) -> anyhow::Result<Response> {
    let (client, request) = request.build_split();
    let request = request?;
    #[cfg(test)]
    let request = tests::redirect_request(request);
    if let Some(budget) = budget {
        budget
            .try_spend()
            .map_err(|exhausted| match previous_error {
                Some(error) => self::exhausted(error),
                None => anyhow::Error::new(exhausted),
            })?;
    }
    Ok(client.execute(request).await?)
}

pub(crate) fn exhausted(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(
        zeroclaw_api::model_provider::GenerationAttemptBudgetExhausted::with_cause(error),
    )
}

#[cfg(test)]
pub(crate) mod tests;
