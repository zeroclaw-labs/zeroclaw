use crate::agent::loop_::get_model_switch_state;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};

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
        "Switch the AI model at runtime. Use 'get' to see current model, 'list_model_providers' to see available model_providers, 'list_models' to see models for a model_provider, or 'set' to switch to a different model. The switch takes effect immediately for the current conversation."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "list_model_providers", "list_models"],
                    "description": "Action to perform: get current model, set a new model, list available model_providers, or list models for a model_provider"
                },
                "model_provider": {
                    "type": "string",
                    "description": "ModelProvider name (e.g., 'openai', 'anthropic', 'groq', 'ollama'). Required for 'set' and 'list_models' actions."
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
            "list_model_providers" => self.handle_list_providers(),
            "list_models" => self.handle_list_models(&args),
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
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
                "note": "To switch models, use action 'set' with model_provider and model parameters"
            }))?,
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
                    output: String::new(),
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
                    output: String::new(),
                    error: Some("Missing 'model' parameter for 'set' action".to_string()),
                });
            }
        };

        // Validate the model_provider exists. Legacy colon-URL forms
        // ("custom:https://..." and "anthropic-custom:...") are collapsed at
        // TOML load by `normalize_model_provider_type` in `schema/v2.rs` into
        // the typed `custom` family slot, so the runtime only sees canonical
        // model-provider names. Validate against the static catalog directly.
        let known_model_providers = zeroclaw_providers::list_model_providers();
        let model_provider_valid = known_model_providers
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(model_provider));

        if !model_provider_valid {
            return Ok(ToolResult {
                success: false,
                output: serde_json::to_string_pretty(&json!({
                    "available_model_providers": known_model_providers.iter().map(|p| p.name).collect::<Vec<_>>()
                }))?,
                error: Some(format!(
                    "Unknown model model_provider: {}. Use 'list_model_providers' to see available options.",
                    model_provider
                )),
            });
        }

        // Set the global model switch request
        let switch_state = get_model_switch_state();
        *switch_state.lock().unwrap() = Some((model_provider.to_string(), model.to_string()));

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Model switch requested",
                "model_provider": model_provider,
                "model": model,
                "note": "The agent will switch to this model on the next turn. Use 'get' to check pending switch."
            }))?,
            error: None,
        })
    }

    fn handle_list_providers(&self) -> anyhow::Result<ToolResult> {
        let providers_list = zeroclaw_providers::list_model_providers();

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
                "example": "Use action 'set' with model_provider and model to switch"
            }))?,
            error: None,
        })
    }

    fn handle_list_models(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let model_provider = args.get("model_provider").and_then(|v| v.as_str());

        let model_provider = match model_provider {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Missing 'model_provider' parameter for 'list_models' action".to_string(),
                    ),
                });
            }
        };

        // Return common models for known model_providers
        let models = match model_provider.to_lowercase().as_str() {
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
            "google" | "gemini" => vec!["gemini-2.0-flash", "gemini-1.5-pro", "gemini-1.5-flash"],
            "xai" | "grok" => vec!["grok-2", "grok-2-vision", "grok-beta"],
            _ => vec![],
        };

        if models.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "model_provider": model_provider,
                    "models": [],
                    "note": "No common models listed for this model_provider. Check model_provider documentation for available models."
                }))?,
                error: None,
            });
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "model_provider": model_provider,
                "models": models,
                "example": "Use action 'set' with this model_provider and a model ID to switch"
            }))?,
            error: None,
        })
    }
}
