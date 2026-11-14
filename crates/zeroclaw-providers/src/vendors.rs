//! Provider-specific defaults and capability construction policy.
//!
//! Common wire protocols must not select vendors or maintain endpoint maps.
//! Fixed defaults come from the existing chat family factory; OpenAI's native
//! API-key endpoint is resolved separately because its chat factory also owns
//! the Codex authentication mode.

use anyhow::{Context, Result, ensure};
use zeroclaw_config::provider_registry::{OpenAiAuth, ProviderConnection};
use zeroclaw_config::schema::{ModelEndpoint, OpenAIEndpoint};

pub(crate) fn base_url(connection: &ProviderConnection) -> Result<String> {
    if let ProviderConnection::OpenAi(config) = connection {
        ensure!(
            !matches!(config.auth.as_ref(), Some(OpenAiAuth::Codex {})),
            "Codex authentication does not provide native audio, embedding or decision APIs"
        );
    }
    if let Some(uri) = connection
        .uri()
        .map(str::trim)
        .filter(|uri| !uri.is_empty())
    {
        return Ok(uri.to_owned());
    }
    if matches!(connection, ProviderConnection::OpenAi(_)) {
        return Ok(OpenAIEndpoint::default().uri().to_owned());
    }
    crate::factory::endpoint_for_family(connection.kind())
        .and_then(crate::factory::ProviderEndpoint::fixed_url)
        .map(str::to_owned)
        .with_context(|| format!("provider connection `{}` requires a uri", connection.kind()))
}
