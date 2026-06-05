use crate::agent::loop_::get_model_switch_state;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use daemonclaw_api::tool::{Tool, ToolResult};

pub struct ModelSwitchTool {
    security: Arc<SecurityPolicy>,
}

impl ModelSwitchTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
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
                    "description": "Provider name (e.g., 'openai', 'glm', 'minimax'). For 'set': optional if model is given (auto-resolved from model name). For 'list_models': required."
                },
                "model": {
                    "type": "string",
                    "description": "Model ID (e.g., 'gpt-4o', 'glm-5.1', 'MiniMax-M3'). For 'set': optional if provider is given (uses provider's default model). At least one of provider or model is required for 'set'."
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
            "list_models" => self.handle_list_models(&args),
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
        let raw_provider = args.get("provider").and_then(|v| v.as_str());
        let raw_model = args.get("model").and_then(|v| v.as_str());

        if raw_provider.is_none() && raw_model.is_none() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "At least one of 'provider' or 'model' is required for 'set' action. \
                     You can specify just a model (e.g. 'glm-5.1') and the provider will \
                     be auto-resolved, or just a provider to use its default model."
                        .to_string(),
                ),
            });
        }

        // --- Resolve provider from model when not explicitly given ---
        let resolved_provider: String = if let Some(p) = raw_provider {
            p.to_string()
        } else {
            let model = raw_model.unwrap(); // safe: at least one is Some
            match daemonclaw_providers::resolve_provider_for_model(model) {
                Some(p) => p.to_string(),
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: serde_json::to_string_pretty(&json!({
                            "model": model,
                            "hint": "Could not auto-detect the provider for this model. \
                                     Please specify the 'provider' parameter explicitly."
                        }))?,
                        error: Some(format!(
                            "Cannot resolve provider for model '{}'. Specify 'provider' explicitly.",
                            model
                        )),
                    });
                }
            }
        };

        // --- Resolve model from provider when not explicitly given ---
        let resolved_model: String = if let Some(m) = raw_model {
            m.to_string()
        } else {
            // Check the provider store for a configured default model first.
            let store_model = daemonclaw_config::provider_store::try_provider_store()
                .and_then(|store| store.get_provider(&resolved_provider))
                .and_then(|entry| entry.model);

            if let Some(m) = store_model {
                m
            } else if let Some(m) = daemonclaw_providers::default_model_for_provider(&resolved_provider) {
                m.to_string()
            } else {
                return Ok(ToolResult {
                    success: false,
                    output: serde_json::to_string_pretty(&json!({
                        "provider": resolved_provider,
                        "hint": "No default model known for this provider. \
                                 Please specify the 'model' parameter explicitly."
                    }))?,
                    error: Some(format!(
                        "No default model for provider '{}'. Specify 'model' explicitly.",
                        resolved_provider
                    )),
                });
            }
        };

        // Validate the provider exists (skip for custom URL-based providers).
        let is_custom_provider = resolved_provider.starts_with("custom:")
            || resolved_provider.starts_with("anthropic-custom:");

        if !is_custom_provider {
            let known_providers = daemonclaw_providers::list_providers();
            let provider_valid = known_providers.iter().any(|p| {
                p.name.eq_ignore_ascii_case(&resolved_provider)
                    || p.aliases
                        .iter()
                        .any(|a| a.eq_ignore_ascii_case(&resolved_provider))
            });

            if !provider_valid {
                return Ok(ToolResult {
                    success: false,
                    output: serde_json::to_string_pretty(&json!({
                        "available_providers": known_providers.iter().map(|p| p.name).collect::<Vec<_>>()
                    }))?,
                    error: Some(format!(
                        "Unknown provider: {}. Use 'list_providers' to see available options, \
                         or use 'custom:<url>' for custom endpoints.",
                        resolved_provider
                    )),
                });
            }
        }

        let mut notes = Vec::new();
        if raw_provider.is_none() {
            notes.push(format!(
                "Provider auto-resolved from model name: '{}'",
                resolved_provider
            ));
        }
        if raw_model.is_none() {
            notes.push(format!(
                "Model auto-resolved from provider default: '{}'",
                resolved_model
            ));
        }

        // Set the global model switch request
        let switch_state = get_model_switch_state();
        *switch_state.lock().unwrap() =
            Some((resolved_provider.clone(), resolved_model.clone()));

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Model switch requested",
                "provider": resolved_provider,
                "model": resolved_model,
                "auto_resolved": notes,
                "note": "The agent will switch to this model on the next turn."
            }))?,
            error: None,
        })
    }

    fn handle_list_providers(&self) -> anyhow::Result<ToolResult> {
        let providers_list = daemonclaw_providers::list_providers();

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

    fn handle_list_models(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let provider = args.get("provider").and_then(|v| v.as_str());

        let provider = match provider {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Missing 'provider' parameter for 'list_models' action".to_string(),
                    ),
                });
            }
        };

        // Return common models for known providers
        let models = match provider.to_lowercase().as_str() {
            "openai" => vec![
                "gpt-4o",
                "gpt-4o-mini",
                "gpt-4.1",
                "gpt-4-turbo",
                "gpt-3.5-turbo",
            ],
            "anthropic" => vec![
                "claude-sonnet-4-6",
                "claude-opus-4",
                "claude-3.5-sonnet",
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
            "deepseek" => vec!["deepseek-chat", "deepseek-coder", "deepseek-r1"],
            "mistral" => vec![
                "mistral-large-latest",
                "mistral-small-latest",
                "mistral-nemo",
                "codestral",
            ],
            "google" | "gemini" => vec![
                "gemini-2.5-flash",
                "gemini-2.5-pro",
                "gemini-2.0-flash",
                "gemini-1.5-pro",
            ],
            "xai" | "grok" => vec!["grok-3", "grok-2", "grok-2-vision"],
            "glm" | "zhipu" | "zai" | "z.ai" => vec![
                "glm-5",
                "glm-5-plus",
                "glm-5-turbo",
                "glm-5-air",
                "glm-4-long",
                "glm-4-plus",
            ],
            "minimax" | "minimax-intl" | "minimaxi" => vec![
                "MiniMax-M3",
                "MiniMax-Text-01",
                "abab7",
                "abab6.5",
            ],
            "qwen" | "dashscope" => vec![
                "qwen3-235b",
                "qwen3-32b",
                "qwen-turbo",
                "qwen-plus",
                "qwen-max",
            ],
            "moonshot" | "kimi" => vec![
                "moonshot-v1-128k",
                "moonshot-v1-32k",
                "moonshot-v1-8k",
            ],
            "cohere" => vec!["command-a", "command-r-plus", "command-r"],
            _ => vec![],
        };

        if models.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "provider": provider,
                    "models": [],
                    "note": "No common models listed for this provider. Check provider documentation for available models."
                }))?,
                error: None,
            });
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "provider": provider,
                "models": models,
                "example": "Use action 'set' with this provider and a model ID to switch"
            }))?,
            error: None,
        })
    }
}
