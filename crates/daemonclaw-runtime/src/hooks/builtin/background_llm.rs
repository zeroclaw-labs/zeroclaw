use std::sync::Arc;
use std::time::Duration;

use daemonclaw_providers::Provider;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Configuration for background LLM calls.
#[derive(Clone)]
pub struct BackgroundLlmConfig {
    pub provider_name: String,
    pub api_key: Option<String>,
    pub model: String,
    pub temperature: f64,
    pub runtime_options: daemonclaw_providers::ProviderRuntimeOptions,
}

/// Shared helper for all background LLM calls (skill autogen, deviation
/// detection, dialectic reasoning, curator).
///
/// Enforces a timeout, catches all errors, logs via tracing, and returns
/// `Option<String>` (None on failure). Callers should never panic on
/// background LLM failures.
pub async fn background_llm_call(
    config: &BackgroundLlmConfig,
    prompt: &str,
    observer: Option<&Arc<dyn crate::observability::Observer>>,
) -> Option<String> {
    background_llm_call_with_timeout(config, prompt, observer, DEFAULT_TIMEOUT).await
}

pub async fn background_llm_call_with_timeout(
    config: &BackgroundLlmConfig,
    prompt: &str,
    observer: Option<&Arc<dyn crate::observability::Observer>>,
    timeout: Duration,
) -> Option<String> {
    let provider: Box<dyn Provider> =
        match daemonclaw_providers::create_provider_with_options(
            &config.provider_name,
            config.api_key.as_deref(),
            &config.runtime_options,
        ) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(target: "background_llm", "failed to create provider: {e}");
                return None;
            }
        };

    if let Some(obs) = observer {
        obs.record_event(&crate::observability::ObserverEvent::LlmRequest {
            provider: config.provider_name.clone(),
            model: config.model.clone(),
            messages_count: 1,
        });
    }

    let start = std::time::Instant::now();

    let result = tokio::time::timeout(
        timeout,
        provider.simple_chat(prompt, &config.model, config.temperature),
    )
    .await;

    let duration = start.elapsed();

    match result {
        Ok(Ok(text)) => {
            if let Some(obs) = observer {
                obs.record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: config.provider_name.clone(),
                    model: config.model.clone(),
                    duration,
                    success: true,
                    error_message: None,
                    input_tokens: None,
                    output_tokens: None,
                });
            }
            Some(text)
        }
        Ok(Err(e)) => {
            tracing::warn!(target: "background_llm", model = %config.model, "LLM call failed: {e}");
            if let Some(obs) = observer {
                obs.record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: config.provider_name.clone(),
                    model: config.model.clone(),
                    duration,
                    success: false,
                    error_message: Some(e.to_string()),
                    input_tokens: None,
                    output_tokens: None,
                });
            }
            None
        }
        Err(_elapsed) => {
            tracing::warn!(
                target: "background_llm",
                model = %config.model,
                timeout_ms = timeout.as_millis(),
                "background LLM call timed out"
            );
            if let Some(obs) = observer {
                obs.record_event(&crate::observability::ObserverEvent::LlmResponse {
                    provider: config.provider_name.clone(),
                    model: config.model.clone(),
                    duration,
                    success: false,
                    error_message: Some("timeout".into()),
                    input_tokens: None,
                    output_tokens: None,
                });
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BackgroundLlmConfig {
        BackgroundLlmConfig {
            provider_name: "nonexistent-provider".into(),
            api_key: None,
            model: "test-model".into(),
            temperature: 0.3,
            runtime_options: Default::default(),
        }
    }

    #[tokio::test]
    async fn invalid_provider_returns_none() {
        let config = test_config();
        let result = background_llm_call(&config, "hello", None).await;
        assert!(result.is_none());
    }
}
