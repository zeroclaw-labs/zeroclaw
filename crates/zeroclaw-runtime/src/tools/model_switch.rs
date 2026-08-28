use crate::agent::turn::current_model_switch_state;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::schema::Config;

#[cfg(test)]
type ModelCatalogResolver = std::sync::Arc<
    dyn Fn(
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<Vec<String>>> + Send>,
        > + Send
        + Sync,
>;

async fn fallback_if_model_listing_unsupported<F, Fut>(
    live_result: anyhow::Result<Vec<String>>,
    family_catalog: F,
) -> anyhow::Result<Vec<String>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<String>>>,
{
    match live_result {
        Err(error) if crate::quickstart::model_listing_is_unsupported(&error) => {
            family_catalog().await
        }
        result => result,
    }
}

fn configured_model_provider_profiles(config: &Config) -> Vec<String> {
    let mut profiles = config
        .providers
        .models
        .iter_entries()
        .map(|(family, alias, _profile)| format!("{family}.{alias}"))
        .collect::<Vec<_>>();
    profiles.sort();
    profiles
}

fn resolve_model_provider_profile_ref(config: &Config, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let Some((family, alias)) = raw.split_once('.') else {
        return Err(format!(
            "model_provider must be a dotted `<type>.<alias>` provider profile reference, got `{raw}`"
        ));
    };
    let family = family.trim();
    let alias = alias.trim();
    if family.is_empty() || alias.is_empty() {
        return Err(format!(
            "model_provider must be a dotted `<type>.<alias>` provider profile reference, got `{raw}`"
        ));
    }

    if config.providers.models.find(family, alias).is_none() {
        let available = configured_model_provider_profiles(config);
        let available = if available.is_empty() {
            "no configured provider profiles".to_string()
        } else {
            available.join(", ")
        };
        return Err(format!(
            "model_provider `{raw}` is not a configured provider profile. Add a [providers.models.{family}.{alias}] entry or use one of: {available}"
        ));
    }

    Ok(format!("{family}.{alias}"))
}

pub struct ModelSwitchTool {
    security: Arc<SecurityPolicy>,
    config: Arc<Config>,
    #[cfg(test)]
    catalog_resolver: Option<ModelCatalogResolver>,
}

impl ModelSwitchTool {
    /// Canonical tool name. Referenced by the subagent registry filter so
    /// a rename cannot desync the two.
    pub const NAME: &'static str = "model_switch";

    pub fn new(security: Arc<SecurityPolicy>, config: Arc<Config>) -> Self {
        Self {
            security,
            config,
            #[cfg(test)]
            catalog_resolver: None,
        }
    }
}

#[async_trait]
impl Tool for ModelSwitchTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Request a runtime model switch using a configured provider profile plus provider-local model. Use 'get' to see the pending switch, 'list_model_providers' to see provider families, 'list_models' to see common models for a provider profile, or 'set' with a dotted provider profile ref such as 'openai.default'. The switch is runtime/session state and does not write config."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "list_model_providers", "list_models"],
                    "description": "Action to perform: get pending switch state, set a runtime provider-profile/model switch, list available provider families, or list common models for a provider profile"
                },
                "model_provider": {
                    "type": "string",
                    "description": "Dotted provider profile reference (e.g., 'openai.default', 'anthropic.sonnet', 'ollama.local'). Required for 'set' and 'list_models' actions."
                },
                "model": {
                    "type": "string",
                    "description": "Model ID (e.g., 'gpt-4o', 'claude-sonnet-4-6'). Required for 'set' action."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("get");

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "model_switch")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        match action {
            "get" => self.handle_get(),
            "set" => self.handle_set(&args),
            "list_model_providers" => self.handle_list_providers(),
            "list_models" => self.handle_list_models(&args).await,
            _ => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Unknown action: {}. Valid actions: get, set, list_model_providers, list_models",
                    action
                )),
            }),
        }
    }
}

impl ModelSwitchTool {
    fn handle_get(&self) -> anyhow::Result<ToolResult> {
        let switch_state = current_model_switch_state()?;
        let pending = switch_state.lock().unwrap().clone();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "pending_switch": pending,
                "note": "To switch models, use action 'set' with dotted <type>.<alias> model_provider and model parameters"
            }))?.into(),
            error: None,
        })
    }

    fn handle_set(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let model_provider = args.get("model_provider").and_then(|v| v.as_str());

        let model_provider = match model_provider {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'model_provider' parameter for 'set' action".to_string()),
                });
            }
        };

        let model = args.get("model").and_then(|v| v.as_str());

        let model = match model {
            Some(m) => m,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'model' parameter for 'set' action".to_string()),
                });
            }
        };

        let model_provider = match resolve_model_provider_profile_ref(&self.config, model_provider)
        {
            Ok(model_provider) => model_provider,
            Err(error) => {
                let known_model_providers = zeroclaw_providers::list_model_providers();
                let configured_profiles = configured_model_provider_profiles(&self.config);
                return Ok(ToolResult {
                    success: false,
                    output: serde_json::to_string_pretty(&json!({
                        "provider_ref_shape": "<type>.<alias>",
                        "available_provider_families": known_model_providers.iter().map(|p| p.name).collect::<Vec<_>>(),
                        "configured_provider_profiles": configured_profiles
                    }))?.into(),
                    error: Some(error),
                });
            }
        };

        let model = model.trim();
        if model.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Model ID cannot be empty".to_string()),
            });
        }

        let switch_state = current_model_switch_state()?;
        *switch_state.lock().unwrap() = Some((model_provider.clone(), model.to_string()));

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Model switch requested",
                "model_provider": model_provider,
                "model": model,
                "note": "The active runtime path will consume this provider-profile/model switch where model_switch is supported. This does not write persisted config."
            }))?.into(),
            error: None,
        })
    }

    fn handle_list_providers(&self) -> anyhow::Result<ToolResult> {
        let providers_list = zeroclaw_providers::list_model_providers();
        let configured_profiles = configured_model_provider_profiles(&self.config);
        let configured_count = configured_profiles.len();

        let model_providers: Vec<serde_json::Value> = providers_list
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "display_name": p.display_name,
                    "local": p.local
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "model_providers": model_providers,
                "count": model_providers.len(),
                "configured_provider_profiles": configured_profiles,
                "configured_count": configured_count,
                "provider_ref_shape": "<type>.<alias>",
                "example": "Use action 'set' with a dotted provider profile ref such as 'openai.default'"
            }))?.into(),
            error: None,
        })
    }

    async fn resolve_catalog(&self, provider_ref: &str) -> anyhow::Result<Vec<String>> {
        #[cfg(test)]
        if let Some(resolver) = &self.catalog_resolver {
            return resolver(provider_ref.to_string()).await;
        }

        let family = provider_ref
            .split_once('.')
            .map_or(provider_ref, |(family, _)| family);
        let provider =
            zeroclaw_providers::create_model_provider_from_ref(&self.config, provider_ref)?;
        let live_result = provider.list_models().await;
        fallback_if_model_listing_unsupported(live_result, || {
            zeroclaw_providers::catalog::list_models_for_family(family)
        })
        .await
    }

    async fn handle_list_models(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let model_provider = args.get("model_provider").and_then(|v| v.as_str());

        let model_provider = match model_provider {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(
                        "Missing 'model_provider' parameter for 'list_models' action".to_string(),
                    ),
                });
            }
        };

        let model_provider = match resolve_model_provider_profile_ref(&self.config, model_provider)
        {
            Ok(model_provider) => model_provider,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: serde_json::to_string_pretty(&json!({
                        "provider_ref_shape": "<type>.<alias>",
                        "configured_provider_profiles": configured_model_provider_profiles(&self.config)
                    }))?.into(),
                    error: Some(error),
                });
            }
        };
        let provider_family = model_provider
            .split_once('.')
            .map(|(family, _alias)| family)
            .unwrap_or(model_provider.as_str());
        let provider_family = provider_family.to_lowercase();

        let models: Vec<String> = match self.resolve_catalog(&model_provider).await {
            Ok(live) => live,
            Err(error) => {
                let error = zeroclaw_providers::sanitize_api_error(&error.to_string());
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "model_provider": model_provider,
                            "provider_family": provider_family,
                            "error": error.to_string(),
                        })),
                    "model_switch list_models: configured profile catalog failed"
                );
                return Ok(ToolResult {
                    success: false,
                    output: serde_json::to_string_pretty(&json!({
                        "model_provider": model_provider,
                        "configured_provider_profiles": configured_model_provider_profiles(&self.config),
                    }))?
                    .into(),
                    error: Some(crate::i18n::get_required_cli_string_with_args(
                        "model-switch-catalog-failed",
                        &[
                            ("provider", &model_provider),
                            ("error", &error),
                        ],
                    )),
                });
            }
        };

        if models.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "model_provider": model_provider,
                    "models": [],
                    "note": "No common models listed for this model_provider family. Check model_provider documentation for available models."
                }))?.into(),
                error: None,
            });
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "model_provider": model_provider,
                "models": models,
                "example": "Use action 'set' with this model_provider and a model ID to switch"
            }))?
            .into(),
            error: None,
        })
    }
}

#[cfg(test)]
impl ModelSwitchTool {
    fn with_catalog_resolver<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<Vec<String>>> + Send + 'static,
    {
        self.catalog_resolver = Some(std::sync::Arc::new(move |fam| Box::pin(f(fam))));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::turn::{
        ModelSwitchCallback, current_model_switch_state, scope_model_switch_state,
    };

    fn test_config() -> Config {
        let mut config = Config::default();
        config.providers.models.ensure("openai", "default").unwrap();
        config.providers.models.ensure("custom", "local").unwrap();
        config
    }

    fn tool() -> ModelSwitchTool {
        ModelSwitchTool::new(Arc::new(SecurityPolicy::default()), Arc::new(test_config()))
    }

    fn pending_switch(state: &ModelSwitchCallback) -> Option<(String, String)> {
        state.lock().unwrap().clone()
    }

    async fn with_switch_state<T>(f: impl FnOnce(ModelSwitchCallback) -> T) -> T {
        let state = Arc::new(std::sync::Mutex::new(None));
        scope_model_switch_state(Arc::clone(&state), async move { f(state) }).await
    }

    #[test]
    fn set_fails_closed_outside_an_active_turn() {
        let error = tool()
            .handle_set(&json!({
                "model_provider": "openai.default",
                "model": "gpt-4o"
            }))
            .expect_err("set must not fall back to process-global state");

        assert!(
            error.to_string().contains("active agent turn"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn set_rejects_bare_provider_family() {
        with_switch_state(|state| {
            let result = tool()
                .handle_set(&json!({
                    "model_provider": "openai",
                    "model": "gpt-4o"
                }))
                .expect("set should return a tool result");

            assert!(!result.success);
            assert!(
                result
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("dotted `<type>.<alias>`"),
                "unexpected error: {:?}",
                result.error
            );
            assert_eq!(pending_switch(&state), None);
        })
        .await;
    }

    #[tokio::test]
    async fn set_accepts_dotted_provider_profile_ref() {
        with_switch_state(|state| {
            let result = tool()
                .handle_set(&json!({
                    "model_provider": "openai.default",
                    "model": "gpt-4o"
                }))
                .expect("set should return a tool result");

            assert!(result.success, "unexpected error: {:?}", result.error);
            assert_eq!(
                pending_switch(&state),
                Some(("openai.default".to_string(), "gpt-4o".to_string()))
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_rejects_unconfigured_provider_profile_ref() {
        with_switch_state(|state| {
            let result = tool()
                .handle_set(&json!({
                    "model_provider": "openai.missing",
                    "model": "gpt-4o"
                }))
                .expect("set should return a tool result");

            assert!(!result.success);
            assert!(
                result
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("configured provider profile"),
                "unexpected error: {:?}",
                result.error
            );
            assert_eq!(pending_switch(&state), None);
        })
        .await;
    }

    #[tokio::test]
    async fn set_accepts_configured_custom_provider_profile_ref() {
        with_switch_state(|state| {
            let result = tool()
                .handle_set(&json!({
                    "model_provider": "custom.local",
                    "model": "local-model"
                }))
                .expect("set should return a tool result");

            assert!(result.success, "unexpected error: {:?}", result.error);
            assert_eq!(
                pending_switch(&state),
                Some(("custom.local".to_string(), "local-model".to_string()))
            );
        })
        .await;
    }

    #[tokio::test]
    async fn set_requests_are_isolated_across_concurrent_turn_scopes() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let run_turn = |model_provider: &'static str, model: &'static str| {
            let barrier = Arc::clone(&barrier);
            async move {
                let state = Arc::new(std::sync::Mutex::new(None));
                scope_model_switch_state(Arc::clone(&state), async move {
                    let result = tool()
                        .handle_set(&json!({
                            "model_provider": model_provider,
                            "model": model
                        }))
                        .expect("set should return a tool result");
                    assert!(result.success, "unexpected error: {:?}", result.error);

                    barrier.wait().await;
                    current_model_switch_state()
                        .expect("turn scope should remain active")
                        .lock()
                        .unwrap()
                        .clone()
                })
                .await
            }
        };

        let (openai, custom) = tokio::join!(
            run_turn("openai.default", "gpt-4o"),
            run_turn("custom.local", "local-model")
        );

        assert_eq!(
            openai,
            Some(("openai.default".to_string(), "gpt-4o".to_string()))
        );
        assert_eq!(
            custom,
            Some(("custom.local".to_string(), "local-model".to_string()))
        );
    }

    #[tokio::test]
    async fn list_models_preserves_hailo_configured_alias_for_catalog_resolution() {
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("hailo_ollama", "edge")
            .unwrap();
        let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()), Arc::new(config))
            .with_catalog_resolver(|provider_ref| async move {
                assert_eq!(provider_ref, "hailo_ollama.edge");
                Ok(vec!["edge-model".to_string()])
            });

        let result = tool
            .handle_list_models(&json!({ "model_provider": "hailo_ollama.edge" }))
            .await
            .expect("list_models should return a tool result");
        assert!(result.success, "unexpected error: {:?}", result.error);
        let output: serde_json::Value =
            serde_json::from_str(&result.output).expect("output should be json");
        assert_eq!(output["models"], json!(["edge-model"]));
    }

    #[tokio::test]
    async fn list_models_queries_configured_hailo_alias_catalog() {
        use axum::{Json, Router, extract::State, http::HeaderMap, routing::get};
        use parking_lot::Mutex;
        use std::collections::HashMap;
        use zeroclaw_config::schema::{HailoOllamaModelProviderConfig, ModelProviderConfig};

        #[derive(Clone)]
        struct CatalogCapture(Arc<Mutex<Option<String>>>);

        async fn tags(
            State(capture): State<CatalogCapture>,
            headers: HeaderMap,
        ) -> Json<serde_json::Value> {
            *capture.0.lock() = headers
                .get("x-route")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            Json(json!({"models": [{"name": "edge-model"}]}))
        }

        let seen_route = Arc::new(Mutex::new(None));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake Hailo endpoint");
        let address = listener.local_addr().expect("fake endpoint address");
        let seen_route_for_server = Arc::clone(&seen_route);
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/tags", get(tags))
                    .with_state(CatalogCapture(seen_route_for_server)),
            )
            .await
            .expect("serve fake Hailo endpoint");
        });

        let mut config = Config::default();
        config.providers.models.hailo_ollama.insert(
            "edge".to_string(),
            HailoOllamaModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some(format!("http://{address}")),
                    model: Some("edge-model".to_string()),
                    extra_headers: HashMap::from([("X-Route".to_string(), "edge".to_string())]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()), Arc::new(config));

        let result = tool
            .handle_list_models(&json!({ "model_provider": "hailo_ollama.edge" }))
            .await
            .expect("list_models should return a tool result");
        assert!(result.success, "unexpected error: {:?}", result.error);
        let output: serde_json::Value =
            serde_json::from_str(&result.output).expect("output should be json");
        assert_eq!(output["models"], json!(["edge-model"]));
        assert_eq!(seen_route.lock().as_deref(), Some("edge"));

        server.abort();
    }

    #[tokio::test]
    async fn list_models_queries_configured_ollama_alias_catalog() {
        use axum::{Json, Router, routing::get};
        use zeroclaw_config::schema::{ModelProviderConfig, OllamaModelProviderConfig};

        async fn models() -> Json<serde_json::Value> {
            Json(json!({"data": [{"id": "llama-local"}]}))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake Ollama endpoint");
        let address = listener.local_addr().expect("fake endpoint address");
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, Router::new().route("/v1/models", get(models)))
                .await
                .expect("serve fake Ollama endpoint");
        });
        let mut config = Config::default();
        config.providers.models.ollama.insert(
            "local".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some(format!("http://{address}")),
                    model: Some("llama-local".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()), Arc::new(config));

        let result = tool
            .handle_list_models(&json!({ "model_provider": "ollama.local" }))
            .await
            .expect("list_models should return a tool result");
        assert!(result.success, "unexpected error: {:?}", result.error);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["models"], json!(["llama-local"]));

        server.abort();
    }

    #[tokio::test]
    async fn configured_profile_catalog_error_remains_actionable() {
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("hailo_ollama", "edge")
            .unwrap();
        let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()), Arc::new(config))
            .with_catalog_resolver(|_provider_ref| async {
                anyhow::bail!("configured catalog denied request")
            });

        let result = tool
            .handle_list_models(&json!({ "model_provider": "hailo_ollama.edge" }))
            .await
            .expect("list_models should return a tool result");
        assert!(
            !result.success,
            "configured profile errors must not become empty success"
        );
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("configured catalog denied request")),
            "unexpected error: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn list_models_accepts_dotted_provider_profile_ref() {
        let tool = tool().with_catalog_resolver(|provider_ref| async move {
            assert_eq!(provider_ref, "openai.default");
            Ok(vec!["gpt-test".to_string()])
        });
        let result = tool
            .handle_list_models(&json!({
                "model_provider": "openai.default"
            }))
            .await
            .expect("list_models should return a tool result");

        assert!(result.success, "unexpected error: {:?}", result.error);
        let output: serde_json::Value =
            serde_json::from_str(&result.output).expect("output should be json");
        assert_eq!(output["model_provider"], "openai.default");
        assert_eq!(output["models"], json!(["gpt-test"]));
    }

    #[tokio::test]
    async fn static_catalog_fallback_only_handles_typed_listing_unsupported() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = calls.clone();
        let models = fallback_if_model_listing_unsupported(
            Err(anyhow::Error::new(
                zeroclaw_api::model_provider::ModelListingUnsupportedError,
            )),
            move || async move {
                fallback_calls.fetch_add(1, Ordering::SeqCst);
                Ok(vec!["static-model".to_string()])
            },
        )
        .await
        .expect("typed unsupported listing should use the family catalog");
        assert_eq!(models, vec!["static-model"]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = calls.clone();
        let error = fallback_if_model_listing_unsupported(
            Err(anyhow::Error::msg("HTTP 401 Unauthorized")),
            move || async move {
                fallback_calls.fetch_add(1, Ordering::SeqCst);
                Ok(vec!["must-not-be-used".to_string()])
            },
        )
        .await
        .expect_err("actionable live-catalog errors must remain fail-closed");
        assert!(error.to_string().contains("401"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = calls.clone();
        let models = fallback_if_model_listing_unsupported(Ok(Vec::new()), move || async move {
            fallback_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["must-not-be-used".to_string()])
        })
        .await
        .expect("reachable empty live catalog is authoritative");
        assert!(models.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn configured_empty_catalog_is_authoritative() {
        let tool = tool().with_catalog_resolver(|_provider_ref| async { Ok(vec![]) });
        let result = tool
            .handle_list_models(&json!({ "model_provider": "openai.default" }))
            .await
            .expect("list_models should return a tool result");
        assert!(result.success, "unexpected error: {:?}", result.error);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(output["models"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_models_redacts_catalog_error_in_tool_result_and_log() {
        const USER: &str = "catalog-user";
        const PASSWORD: &str = "s3cr3t-password";
        const SIGNATURE: &str = "signed-query-value";

        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {} // drain prior events

        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("hailo_ollama", "log_error")
            .unwrap();
        let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()), Arc::new(config))
            .with_catalog_resolver(|_provider_ref| async {
                anyhow::bail!(
                    "GET https://{USER}:{PASSWORD}@api.example.com/v1/models?signature={SIGNATURE} failed"
                )
            });
        let result = tool
            .handle_list_models(&json!({ "model_provider": "hailo_ollama.log_error" }))
            .await
            .expect("list_models should return a tool result");
        assert!(!result.success);
        let tool_error = result
            .error
            .as_deref()
            .expect("tool error should be present");
        let sanitized_diagnostic = zeroclaw_providers::sanitize_api_error(&format!(
            "GET https://{USER}:{PASSWORD}@api.example.com/v1/models?signature={SIGNATURE} failed"
        ));
        let expected_error = crate::i18n::get_required_cli_string_with_args(
            "model-switch-catalog-failed",
            &[
                ("provider", "hailo_ollama.log_error"),
                ("error", &sanitized_diagnostic),
            ],
        );
        assert_eq!(tool_error, expected_error);
        for secret in [USER, PASSWORD, SIGNATURE] {
            assert!(
                !tool_error.contains(secret),
                "tool error leaked {secret}: {tool_error}"
            );
        }
        assert!(
            tool_error.contains("https://[REDACTED]@api.example.com/v1/models"),
            "tool error should retain a safe endpoint diagnostic: {tool_error}"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = false;
        while !found && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    let is_catalog_failure = value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("configured profile catalog failed"))
                        .unwrap_or(false);
                    let is_hailo = value
                        .get("attributes")
                        .and_then(|a| a.get("provider_family"))
                        .and_then(|v| v.as_str())
                        == Some("hailo_ollama");
                    if is_catalog_failure && is_hailo {
                        let attrs = value.get("attributes").expect("attributes present");
                        assert_eq!(
                            attrs.get("provider_family").and_then(|v| v.as_str()),
                            Some("hailo_ollama")
                        );
                        assert_eq!(
                            attrs.get("model_provider").and_then(|v| v.as_str()),
                            Some("hailo_ollama.log_error")
                        );
                        let logged_error = attrs
                            .get("error")
                            .and_then(|v| v.as_str())
                            .expect("logged error attribute");
                        for secret in [USER, PASSWORD, SIGNATURE] {
                            assert!(
                                !logged_error.contains(secret),
                                "log attribute leaked {secret}: {logged_error}"
                            );
                        }
                        assert!(
                            logged_error.contains("https://[REDACTED]@api.example.com/v1/models"),
                            "log should retain a safe endpoint diagnostic: {logged_error}"
                        );
                        assert_eq!(
                            value.get("severity_text").and_then(|v| v.as_str()),
                            Some("WARN")
                        );
                        found = true;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        assert!(
            found,
            "did not capture the configured-profile catalog failure WARN event"
        );
        zeroclaw_log::clear_broadcast_hook();
    }
}
