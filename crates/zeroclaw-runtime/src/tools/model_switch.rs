use crate::agent::loop_::get_model_switch_state;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_providers::ModelCatalogClient;

pub struct ModelSwitchTool {
    security: Arc<SecurityPolicy>,
    catalog: Option<Arc<ModelCatalogClient>>,
}

impl ModelSwitchTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            catalog: None,
        }
    }

    pub fn with_catalog(mut self, catalog: Arc<ModelCatalogClient>) -> Self {
        self.catalog = Some(catalog);
        self
    }
}

#[async_trait]
impl Tool for ModelSwitchTool {
    fn name(&self) -> &str {
        "model_switch"
    }

    fn description(&self) -> &str {
        "Switch the AI model at runtime. Use 'get' to see current model, 'list_providers' to see available providers, 'list_models' to see models for a provider, or 'set' to switch to a different model. The switch takes effect immediately for the current conversation."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "list_providers", "list_models"],
                    "description": "Action to perform: get current model, set a new model, list available providers, or list models for a provider"
                },
                "provider": {
                    "type": "string",
                    "description": "Provider name (e.g., 'openai', 'anthropic', 'groq', 'ollama'). Required for 'set' and 'list_models' actions."
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
                output: String::new(),
                error: Some(error),
            });
        }

        match action {
            "get" => self.handle_get(),
            "set" => self.handle_set(&args),
            "list_providers" => self.handle_list_providers(),
            "list_models" => self.handle_list_models(&args).await,
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: {}. Valid actions: get, set, list_providers, list_models",
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
                "note": "To switch models, use action 'set' with provider and model parameters"
            }))?,
            error: None,
        })
    }

    fn handle_set(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let provider = args.get("provider").and_then(|v| v.as_str());

        let provider = match provider {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'provider' parameter for 'set' action".to_string()),
                });
            }
        };

        let model = args.get("model").and_then(|v| v.as_str());

        let model = match model {
            Some(m) => m,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'model' parameter for 'set' action".to_string()),
                });
            }
        };

        // Validate the provider exists.
        // Custom URL-based providers (e.g. "custom:https://api.nvidia.com/v1")
        // and Anthropic-compatible custom endpoints bypass the known-provider
        // check because they are not in the static provider list.
        let is_custom_provider =
            provider.starts_with("custom:") || provider.starts_with("anthropic-custom:");

        if !is_custom_provider {
            let known_providers = zeroclaw_providers::list_providers();
            let provider_valid = known_providers.iter().any(|p| {
                p.name.eq_ignore_ascii_case(provider)
                    || p.aliases.iter().any(|a| a.eq_ignore_ascii_case(provider))
            });

            if !provider_valid {
                return Ok(ToolResult {
                    success: false,
                    output: serde_json::to_string_pretty(&json!({
                        "available_providers": known_providers.iter().map(|p| p.name).collect::<Vec<_>>()
                    }))?,
                    error: Some(format!(
                        "Unknown provider: {}. Use 'list_providers' to see available options, or use 'custom:<url>' for custom endpoints.",
                        provider
                    )),
                });
            }
        }

        // Set the global model switch request
        let switch_state = get_model_switch_state();
        *switch_state.lock().unwrap() = Some((provider.to_string(), model.to_string()));

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Model switch requested",
                "provider": provider,
                "model": model,
                "note": "The agent will switch to this model on the next turn. Use 'get' to check pending switch."
            }))?,
            error: None,
        })
    }

    fn handle_list_providers(&self) -> anyhow::Result<ToolResult> {
        let providers_list = zeroclaw_providers::list_providers();

        let providers: Vec<serde_json::Value> = providers_list
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "display_name": p.display_name,
                    "aliases": p.aliases,
                    "local": p.local
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "providers": providers,
                "count": providers.len(),
                "example": "Use action 'set' with provider and model to switch"
            }))?,
            error: None,
        })
    }

    async fn handle_list_models(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let provider = args.get("provider").and_then(|v| v.as_str()).unwrap_or("");

        // If a catalog is wired and the caller either asked for the catalog-
        // backed provider or omitted the parameter, return the live list.
        if let Some(catalog) = &self.catalog {
            if provider.is_empty() || provider.starts_with("custom:") {
                match catalog.list_models().await {
                    Ok(models) => {
                        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
                        return Ok(ToolResult {
                            success: true,
                            output: serde_json::to_string_pretty(&json!({
                                "provider": if provider.is_empty() { "catalog" } else { provider },
                                "models": ids,
                                "source": "live",
                                "count": models.len()
                            }))?,
                            error: None,
                        });
                    }
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("catalog unavailable: {e:#}")),
                        });
                    }
                }
            }
        }

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "No live catalog configured for provider '{provider}'. Use action 'list_providers' or set action='list_models' without specifying a provider to query the catalog."
            )),
        })
    }
}
