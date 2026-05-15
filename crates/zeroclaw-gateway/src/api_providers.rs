//! `GET /api/providers` — list configured model providers.
//!
//! The dashboard's slot settings drawer needs the list of providers a
//! user has actually configured (via `[providers.models.*]` in
//! `config.toml` or the `/api/config/*` surface) so the Advanced tab
//! can render a real dropdown rather than hardcoding the provider zoo.
//! ZeroClaw ships ~15 first-class provider implementations; treating
//! the configured map as the source of truth keeps that list out of
//! the frontend.
//!
//! Response shape is intentionally minimal — `id`, `display_name`,
//! `model`, `is_fallback`. Auth headers and bespoke per-provider
//! options (api keys, base URLs) stay out of this surface; those are
//! the `/api/config/*` shape's concern.

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};

#[cfg(feature = "schema-export")]
use schemars::JsonSchema;

use super::AppState;
use super::api::require_auth;

/// One configured provider entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct ProviderInfo {
    /// Map key from `[providers.models.<id>]` — e.g. `"anthropic"`,
    /// `"claude_code"`, `"openai_codex"`. The slot's
    /// `agent_config.provider` references this id verbatim.
    pub id: String,
    /// Human-readable label for the dropdown. Falls back to the id
    /// when the static lookup has no entry.
    pub display_name: String,
    /// The currently configured model for this provider entry, if any.
    /// `None` means the provider was registered without a default
    /// model — the slot's `agent_config.model` must fill the gap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `true` when this provider is the gateway's fallback (i.e.
    /// `providers.fallback == Some(self.id)`). Frontend uses this to
    /// label the default option in the dropdown.
    pub is_fallback: bool,
}

/// `GET /api/providers` response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct ProviderListResponse {
    pub providers: Vec<ProviderInfo>,
}

/// Map a provider id to a human-readable label. Returns the id itself
/// when no mapping exists — keeps unconfigured-yet-discovered ids
/// rendering legibly in the UI.
fn display_name_for(id: &str) -> &'static str {
    match id {
        "anthropic" => "Anthropic",
        "azure_openai" => "Azure OpenAI",
        "bedrock" => "AWS Bedrock",
        "claude_code" => "Claude Code",
        "compatible" => "OpenAI-Compatible",
        "copilot" => "GitHub Copilot",
        "gemini" => "Google Gemini",
        "gemini_cli" => "Gemini CLI",
        "glm" => "GLM",
        "kilocli" => "Kilo CLI",
        "llamacpp" => "llama.cpp",
        "ollama" => "Ollama",
        "openai" => "OpenAI",
        "openai_codex" => "OpenAI Codex",
        "openrouter" => "OpenRouter",
        "telnyx" => "Telnyx",
        _ => "",
    }
}

/// `GET /api/providers` — list every entry in `[providers.models]`.
///
/// Empty list when no providers are configured (which is also when
/// the gateway can't actually answer model requests — UI can use the
/// empty state to nudge the user toward `/onboarding`).
pub async fn handle_api_providers_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let cfg = state.config.lock();
    let fallback = cfg.providers.fallback.clone();

    // Sort by id for stable ordering across requests — the map is a
    // HashMap and iteration order would otherwise jitter, which makes
    // diff-based dropdowns ugly and tests flaky.
    let mut entries: Vec<(String, &zeroclaw_config::schema::ModelProviderConfig)> = cfg
        .providers
        .models
        .iter()
        .map(|(k, v)| (k.clone(), v))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let providers = entries
        .into_iter()
        .map(|(id, entry)| {
            let label = display_name_for(&id);
            let display_name = if label.is_empty() {
                id.clone()
            } else {
                label.to_string()
            };
            ProviderInfo {
                is_fallback: fallback.as_deref() == Some(id.as_str()),
                model: entry.model.clone(),
                display_name,
                id,
            }
        })
        .collect();

    Json(ProviderListResponse { providers }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_queue::{SessionActorQueue, SlotActorQueue};
    use axum::body::to_bytes;
    use std::sync::Arc;
    use zeroclaw_config::schema::{Config, ModelProviderConfig};

    /// Minimal `AppState` for handler-level tests — only the config + auth
    /// path are exercised, so most fields can stay defaulted.
    fn provider_test_state(cfg: Config) -> AppState {
        AppState {
            config: Arc::new(parking_lot::Mutex::new(cfg)),
            provider: Arc::new(StubProvider),
            model: "stub-model".into(),
            temperature: 0.0,
            mem: Arc::new(StubMemory),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
                false,
                &[],
            )),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(crate::GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(crate::auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(crate::IdempotencyStore::new(
                std::time::Duration::from_secs(300),
                1000,
            )),
            whatsapp: None,
            whatsapp_app_secret: None,
            linq: None,
            linq_signing_secret: None,
            nextcloud_talk: None,
            nextcloud_talk_webhook_secret: None,
            wati: None,
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(crate::sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(crate::nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            web_dashboard_dist_dir: None,
            session_backend: None,
            session_queue: Arc::new(SessionActorQueue::new(8, 30, 600)),
            slot_queue: Arc::new(SlotActorQueue::new(8, 30, 600)),
            slot_store: None,
            slot_cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            mcp_registry: None,
            slot_registry: crate::slot_registry::SlotRegistry::new(600),
            device_registry: None,
            pending_pairings: None,
            canvas_store: zeroclaw_runtime::tools::CanvasStore::new(),
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            #[cfg(feature = "webauthn")]
            webauthn: None,
        }
    }

    struct StubProvider;
    #[async_trait::async_trait]
    impl zeroclaw_providers::Provider for StubProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".into())
        }
    }

    struct StubMemory;
    #[async_trait::async_trait]
    impl zeroclaw_memory::Memory for StubMemory {
        fn name(&self) -> &str {
            "stub"
        }
        async fn store(
            &self,
            _: &str,
            _: &str,
            _: zeroclaw_memory::MemoryCategory,
            _: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            _: &str,
            _: usize,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _: Option<&zeroclaw_memory::MemoryCategory>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn forget(&self, _: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    async fn body_to_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    #[tokio::test]
    async fn providers_empty_config_returns_empty_list() {
        let state = provider_test_state(Config::default());
        let resp = handle_api_providers_list(State(state), HeaderMap::new()).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let json = body_to_json(resp).await;
        let providers = json["providers"].as_array().expect("providers array");
        assert!(providers.is_empty());
    }

    #[tokio::test]
    async fn providers_returns_configured_entries_sorted() {
        let mut cfg = Config::default();
        cfg.providers.models.insert(
            "openai".into(),
            ModelProviderConfig {
                model: Some("gpt-5".into()),
                ..Default::default()
            },
        );
        cfg.providers.models.insert(
            "anthropic".into(),
            ModelProviderConfig {
                model: Some("claude-sonnet-4".into()),
                ..Default::default()
            },
        );
        cfg.providers.fallback = Some("anthropic".into());

        let state = provider_test_state(cfg);
        let resp = handle_api_providers_list(State(state), HeaderMap::new()).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let json = body_to_json(resp).await;
        let providers = json["providers"].as_array().expect("providers array");
        assert_eq!(providers.len(), 2);
        // Sort order is alphabetical by id.
        assert_eq!(providers[0]["id"], "anthropic");
        assert_eq!(providers[0]["display_name"], "Anthropic");
        assert_eq!(providers[0]["model"], "claude-sonnet-4");
        assert_eq!(providers[0]["is_fallback"], true);
        assert_eq!(providers[1]["id"], "openai");
        assert_eq!(providers[1]["display_name"], "OpenAI");
        assert_eq!(providers[1]["model"], "gpt-5");
        assert_eq!(providers[1]["is_fallback"], false);
    }

    #[tokio::test]
    async fn providers_unknown_id_falls_back_to_id_for_display_name() {
        let mut cfg = Config::default();
        cfg.providers.models.insert(
            "homemade-provider".into(),
            ModelProviderConfig {
                model: None,
                ..Default::default()
            },
        );
        let state = provider_test_state(cfg);
        let resp = handle_api_providers_list(State(state), HeaderMap::new()).await;
        let json = body_to_json(resp).await;
        let providers = json["providers"].as_array().unwrap();
        assert_eq!(providers[0]["id"], "homemade-provider");
        assert_eq!(providers[0]["display_name"], "homemade-provider");
        assert!(providers[0]["model"].is_null());
    }
}
