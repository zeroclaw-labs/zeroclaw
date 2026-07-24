use crate::agent::loop_::get_model_switch_state;
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
        let switch_state = get_model_switch_state();
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

        // Set the global model switch request
        let switch_state = get_model_switch_state();
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

    async fn resolve_catalog(&self, family: &str) -> anyhow::Result<Vec<String>> {
        #[cfg(test)]
        if let Some(resolver) = &self.catalog_resolver {
            return resolver(family.to_string()).await;
        }
        zeroclaw_providers::catalog::list_models_for_family(family).await
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

        let models: Vec<String> = match self.resolve_catalog(&provider_family).await {
            Ok(live) if !live.is_empty() => live,
            Ok(_) => hardcoded_models_for(&provider_family),
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "model_provider": model_provider,
                            "provider_family": provider_family,
                            "error": error.to_string(),
                        })),
                    "model_switch list_models: live catalog unavailable, using hardcoded fallback"
                );
                hardcoded_models_for(&provider_family)
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

/// Offline fallback catalog for known provider families. Used only when the
/// live `list_models_for_family` catalog is unreachable or empty. Kept in
/// sync with the families in `list_model_providers`; intentionally minimal —
/// the live catalog is authoritative when reachable
fn hardcoded_models_for(provider_family: &str) -> Vec<String> {
    let models: Vec<&'static str> = match provider_family {
        "openai" => vec![
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4-turbo",
            "gpt-4",
            "gpt-3.5-turbo",
        ],
        "anthropic" => vec![
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-3-5-sonnet",
            "claude-3-opus",
            "claude-3-haiku",
        ],
        "openrouter" => vec![
            "anthropic/claude-sonnet-4-6",
            "openai/gpt-4o",
            "google/gemini-pro",
            "meta-llama/llama-3-70b-instruct",
        ],
        "groq" => vec![
            "llama-3.3-70b-versatile",
            "mixtral-8x7b-32768",
            "llama-3.1-70b-speculative",
        ],
        "ollama" => vec!["llama3", "llama3.1", "mistral", "codellama", "phi3"],
        "deepseek" => vec!["deepseek-chat", "deepseek-coder"],
        "mistral" => vec![
            "mistral-large-latest",
            "mistral-small-latest",
            "mistral-nemo",
        ],
        "gemini" => vec!["gemini-2.0-flash", "gemini-1.5-pro", "gemini-1.5-flash"],
        "xai" => vec!["grok-2", "grok-2-vision", "grok-beta"],
        _ => vec![],
    };
    models.into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::loop_::{
        MODEL_SWITCH_TEST_LOCK, clear_model_switch_request, get_model_switch_state,
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

    fn pending_switch() -> Option<(String, String)> {
        get_model_switch_state().lock().unwrap().clone()
    }

    #[test]
    fn set_rejects_bare_provider_family() {
        let _guard = MODEL_SWITCH_TEST_LOCK.lock().unwrap();
        clear_model_switch_request();

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
        assert_eq!(pending_switch(), None);
    }

    #[test]
    fn set_accepts_dotted_provider_profile_ref() {
        let _guard = MODEL_SWITCH_TEST_LOCK.lock().unwrap();
        clear_model_switch_request();

        let result = tool()
            .handle_set(&json!({
                "model_provider": "openai.default",
                "model": "gpt-4o"
            }))
            .expect("set should return a tool result");

        assert!(result.success, "unexpected error: {:?}", result.error);
        assert_eq!(
            pending_switch(),
            Some(("openai.default".to_string(), "gpt-4o".to_string()))
        );

        clear_model_switch_request();
    }

    #[test]
    fn set_rejects_unconfigured_provider_profile_ref() {
        let _guard = MODEL_SWITCH_TEST_LOCK.lock().unwrap();
        clear_model_switch_request();

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
        assert_eq!(pending_switch(), None);
    }

    #[test]
    fn set_accepts_configured_custom_provider_profile_ref() {
        let _guard = MODEL_SWITCH_TEST_LOCK.lock().unwrap();
        clear_model_switch_request();

        let result = tool()
            .handle_set(&json!({
                "model_provider": "custom.local",
                "model": "local-model"
            }))
            .expect("set should return a tool result");

        assert!(result.success, "unexpected error: {:?}", result.error);
        assert_eq!(
            pending_switch(),
            Some(("custom.local".to_string(), "local-model".to_string()))
        );

        clear_model_switch_request();
    }

    #[tokio::test]
    async fn list_models_accepts_dotted_provider_profile_ref() {
        let result = tool()
            .handle_list_models(&json!({
                "model_provider": "openai.default"
            }))
            .await
            .expect("list_models should return a tool result");

        assert!(result.success, "unexpected error: {:?}", result.error);
        let output: serde_json::Value =
            serde_json::from_str(&result.output).expect("output should be json");
        assert_eq!(output["model_provider"], "openai.default");
        // Whether the live models.dev catalog is reachable or we fell back to
        // the offline list, a configured OpenAI profile must yield a non-empty
        // model list.
        assert!(
            !output["models"]
                .as_array()
                .expect("models should be an array")
                .is_empty(),
            "expected a non-empty model list, got: {}",
            result.output
        );
    }

    #[test]
    fn hardcoded_fallback_covers_known_families() {
        // The nine families that have hardcoded fallback arms.
        for family in [
            "openai",
            "anthropic",
            "openrouter",
            "groq",
            "ollama",
            "deepseek",
            "mistral",
            "gemini",
            "xai",
        ] {
            assert!(
                !hardcoded_models_for(family).is_empty(),
                "expected a non-empty offline fallback for family `{family}`"
            );
        }
        // OpenAI's stale fallback set still contains gpt-4o.
        assert!(hardcoded_models_for("openai").iter().any(|m| m == "gpt-4o"));
        // Unknown families have no fallback.
        assert!(hardcoded_models_for("not_a_real_family").is_empty());
    }

    #[tokio::test]
    async fn list_models_prefers_live_catalog_when_reachable() {
        let live = match zeroclaw_providers::catalog::list_models_for_family("openai").await {
            Ok(live) if !live.is_empty() => live,
            _ => {
                eprintln!("skipping: models.dev catalog unreachable (offline)");
                return;
            }
        };

        let result = tool()
            .handle_list_models(&json!({ "model_provider": "openai.default" }))
            .await
            .expect("list_models should return a tool result");
        assert!(result.success, "unexpected error: {:?}", result.error);
        let output: serde_json::Value =
            serde_json::from_str(&result.output).expect("output should be json");
        let models: Vec<String> = output["models"]
            .as_array()
            .expect("models should be an array")
            .iter()
            .map(|m| m.as_str().unwrap_or_default().to_string())
            .collect();

        // The returned set must be the live catalog, not the stale hardcoded
        // five-element list.
        assert_eq!(
            models, live,
            "list_models should return the live catalog when reachable"
        );
        assert_ne!(
            models,
            hardcoded_models_for("openai"),
            "live catalog should differ from the stale hardcoded fallback"
        );
    }

    #[tokio::test]
    async fn list_models_falls_back_to_hardcoded_on_real_offline_err() {
        let mut config = Config::default();
        config.providers.models.ensure("ollama", "local").unwrap();
        let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()), Arc::new(config));
        let result = tool
            .handle_list_models(&json!({ "model_provider": "ollama.local" }))
            .await
            .expect("list_models should return a tool result");
        assert!(result.success, "unexpected error: {:?}", result.error);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["model_provider"], "ollama.local");
        let models: Vec<String> = out["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap().to_string())
            .collect();
        assert_eq!(models, hardcoded_models_for("ollama")); // real offline Err served the hardcoded list
    }

    #[tokio::test]
    async fn list_models_falls_back_to_hardcoded_on_empty_ok() {
        let tool = tool().with_catalog_resolver(|_fam| async { Ok(vec![]) }); // empty-Ok arm (292)
        let result = tool
            .handle_list_models(&json!({ "model_provider": "openai.default" }))
            .await
            .expect("list_models should return a tool result");
        assert!(result.success, "unexpected error: {:?}", result.error);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let models: Vec<String> = out["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap().to_string())
            .collect();
        assert_eq!(models, hardcoded_models_for("openai"));
    }

    #[tokio::test]
    async fn list_models_returns_empty_when_no_hardcoded_fallback() {
        let result = tool()
            .handle_list_models(&json!({ "model_provider": "custom.local" }))
            .await
            .expect("list_models should return a tool result");
        assert!(result.success, "unexpected error: {:?}", result.error);
        assert!(
            result.error.is_none(),
            "expected no error, got: {:?}",
            result.error
        );
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["model_provider"], "custom.local");
        assert!(out["models"].as_array().unwrap().is_empty());
        assert!(
            out["note"]
                .as_str()
                .unwrap()
                .contains("No common models listed")
        );
    }

    #[tokio::test]
    async fn list_models_logs_warn_on_catalog_err() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {} // drain prior events

        let mut config = Config::default();
        config.providers.models.ensure("ollama", "local").unwrap();
        let tool = ModelSwitchTool::new(Arc::new(SecurityPolicy::default()), Arc::new(config));
        let _ = tool
            .handle_list_models(&json!({ "model_provider": "ollama.local" }))
            .await
            .expect("list_models should return a tool result");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = false;
        while !found && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    let is_fallback_warn = value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("live catalog unavailable, using hardcoded fallback"))
                        .unwrap_or(false);
                    let is_ollama = value
                        .get("attributes")
                        .and_then(|a| a.get("provider_family"))
                        .and_then(|v| v.as_str())
                        == Some("ollama");
                    if is_fallback_warn && is_ollama {
                        let attrs = value.get("attributes").expect("attributes present");
                        assert_eq!(
                            attrs.get("provider_family").and_then(|v| v.as_str()),
                            Some("ollama")
                        );
                        assert_eq!(
                            attrs.get("model_provider").and_then(|v| v.as_str()),
                            Some("ollama.local")
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
            "did not capture the model_switch WARN fallback event"
        );
        zeroclaw_log::clear_broadcast_hook();
    }
}
