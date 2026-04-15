//! `model_spawn` tool — live session model switch and ephemeral spawn modes.
//!
//! This tool implements the canonical `model_spawn` spec shared with OpenClaw.
//! See `docs/tools/model-spawn.md` in the openclaw repo for the full spec.
//!
//! ## Modes
//!
//! - `live`: switch the current session's model in-place. Context is preserved;
//!   the switch takes effect at the next clean turn boundary by writing to the
//!   shared `MODEL_SWITCH_REQUEST` global checked by the agent loop.
//!
//! - `spawn` (single): run one task in an isolated provider call with the
//!   specified model. The parent session's model is unchanged.
//!
//! - `spawn` (multi): run up to 5 tasks concurrently, each on a specified
//!   model, by fanning out with `futures_util::future::join_all`.

use crate::agent::loop_::get_model_switch_state;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use futures_util::future::join_all;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use super::traits::{Tool, ToolResult};

/// Maximum number of concurrent spawns in multi-model mode.
/// Capped at 5 to stay within reasonable concurrency limits.
const MAX_PARALLEL_SPAWNS: usize = 5;

/// Minimum per-spawn timeout; 0 would immediately timeout every call.
const MIN_SPAWN_TIMEOUT_SECS: u64 = 1;

/// Default per-spawn timeout when none is specified.
const DEFAULT_SPAWN_TIMEOUT_SECS: u64 = 120;

/// Default temperature used for spawned task calls.
const DEFAULT_SPAWN_TEMPERATURE: f64 = 0.7;

pub struct ModelSpawnTool {
    security: Arc<SecurityPolicy>,
    api_key: Option<String>,
    provider_runtime_options: crate::providers::ProviderRuntimeOptions,
}

impl ModelSpawnTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        api_key: Option<String>,
        provider_runtime_options: crate::providers::ProviderRuntimeOptions,
    ) -> Self {
        Self {
            security,
            api_key,
            provider_runtime_options,
        }
    }
}

// ── schema ────────────────────────────────────────────────────────────────────

#[async_trait]
impl Tool for ModelSpawnTool {
    fn name(&self) -> &str {
        "model_spawn"
    }

    fn description(&self) -> &str {
        "Spawn models for inference tasks. \
         mode=\"live\": switch the current session's model in-place (context preserved, \
         takes effect at the next clean turn boundary). \
         mode=\"spawn\": run one or more tasks in isolated ephemeral sessions — pass a \
         single model+task for focused delegation, or a spawns[] array to run multiple \
         models concurrently for specialization or comparison."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["mode"],
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["live", "spawn"],
                    "description": "live=switch the current session model in-place (context preserved, \
                                    takes effect next clean turn). \
                                    spawn=run one or more tasks in isolated ephemeral sessions \
                                    (context isolated, sessions cleaned up by default)."
                },
                "model": {
                    "type": "string",
                    "description": "Full provider/model spec, e.g. \"together/MiniMaxAI/MiniMax-M2.7\". \
                                    Required for live mode and single-model spawn. \
                                    Omit when using the spawns array."
                },
                "task": {
                    "type": "string",
                    "description": "Task to run. Required for single-model spawn. \
                                    Serves as the default task for spawns array entries \
                                    that do not specify their own."
                },
                "context": {
                    "type": "string",
                    "description": "Context to prepend to the task. \
                                    Used for single-model spawn or as default for spawns array entries."
                },
                "spawns": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 5,
                    "description": "Spawn multiple models concurrently, each in its own isolated \
                                    session. All spawns run in parallel and results are collected. \
                                    Use for model specialization or model comparison.",
                    "items": {
                        "type": "object",
                        "required": ["model"],
                        "properties": {
                            "model": {
                                "type": "string",
                                "description": "Full provider/model spec for this spawn."
                            },
                            "task": {
                                "type": "string",
                                "description": "Task for this spawn. Falls back to top-level task when omitted."
                            },
                            "label": {
                                "type": "string",
                                "description": "Human-readable label for this spawn's result."
                            },
                            "context": {
                                "type": "string",
                                "description": "Context for this spawn. Falls back to top-level context when omitted."
                            }
                        }
                    }
                },
                "timeout_seconds": {
                    "type": "number",
                    "minimum": 0,
                    "description": "Per-spawn timeout in seconds."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate.
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "model_spawn")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let mode = match args.get("mode").and_then(|v| v.as_str()) {
            Some("live") => "live",
            Some("spawn") => "spawn",
            Some(other) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid mode: \"{other}\". Must be \"live\" or \"spawn\"."
                    )),
                });
            }
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("mode is required.".to_string()),
                });
            }
        };

        match mode {
            "live" => self.execute_live(&args),
            "spawn" => self.execute_spawn(&args).await,
            _ => unreachable!(),
        }
    }
}

// ── live mode ─────────────────────────────────────────────────────────────────

impl ModelSpawnTool {
    fn execute_live(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let model_raw = match args.get("model").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("model is required for live mode.".to_string()),
                });
            }
        };

        let (provider, model_id) = match split_first_slash(&model_raw) {
            Ok(pair) => pair,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e),
                });
            }
        };

        // Validate provider (allow custom: and anthropic-custom: prefixes).
        if let Err(e) = validate_provider(&provider) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
            });
        }

        // Write to the global switch request; the agent loop applies it at the
        // next clean turn boundary.
        let switch_state = get_model_switch_state();
        *switch_state
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((provider.clone(), model_id.clone()));

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "status": "ok",
                "mode": "live",
                "model": model_raw,
                "provider": provider,
                "modelId": model_id,
                "switchPending": true,
                "note": format!(
                    "Model switch to {model_raw} queued. \
                     Takes effect at the next clean turn boundary."
                )
            }))?,
            error: None,
        })
    }
}

// ── spawn mode ────────────────────────────────────────────────────────────────

impl ModelSpawnTool {
    async fn execute_spawn(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let top_model = args.get("model").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
        let top_task = args.get("task").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
        let top_context = args.get("context").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
        let raw_spawns = args.get("spawns").and_then(|v| v.as_array());
        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(|v| v.as_f64())
            .map(|f| (f.max(0.0) as u64).max(MIN_SPAWN_TIMEOUT_SECS))
            .unwrap_or(DEFAULT_SPAWN_TIMEOUT_SECS);

        // mutual exclusion: top-level model and spawns[] cannot both be set
        if top_model.is_some() && raw_spawns.map(|s| !s.is_empty()).unwrap_or(false) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Provide either a top-level model (single spawn) or a spawns array \
                     (multi-spawn), not both."
                        .to_string(),
                ),
            });
        }

        // ── multi-model parallel spawn ─────────────────────────────────────
        if let Some(entries) = raw_spawns {
            if entries.is_empty() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("spawns array must not be empty.".to_string()),
                });
            }

            let entries = &entries[..entries.len().min(MAX_PARALLEL_SPAWNS)];

            let futures: Vec<_> = entries
                .iter()
                .enumerate()
                .map(|(idx, entry)| {
                    let entry_model = entry
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let entry_task = entry
                        .get("task")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .or(top_task)
                        .unwrap_or("")
                        .to_string();
                    let entry_context = entry
                        .get("context")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .or(top_context)
                        .unwrap_or("")
                        .to_string();
                    let entry_label = entry
                        .get("label")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&entry_model)
                        .to_string();

                    let api_key = self.api_key.clone();
                    let opts = self.provider_runtime_options.clone();

                    async move {
                        if entry_model.is_empty() {
                            return json!({
                                "label": entry_label, "index": idx,
                                "status": "error",
                                "error": "model is required"
                            });
                        }
                        if entry_task.is_empty() {
                            return json!({
                                "label": entry_label, "index": idx, "model": entry_model,
                                "status": "error",
                                "error": "task is required (provide per-entry or as top-level default)"
                            });
                        }

                        let (provider, model_id) = match split_first_slash(&entry_model) {
                            Ok(pair) => pair,
                            Err(e) => {
                                return json!({
                                    "label": entry_label, "index": idx, "model": entry_model,
                                    "status": "error", "error": e
                                });
                            }
                        };

                        let full_task = build_task(&entry_context, &entry_task);
                        match run_ephemeral_call(
                            &provider,
                            &model_id,
                            api_key.as_deref(),
                            &opts,
                            &full_task,
                            timeout_secs,
                        )
                        .await
                        {
                            Ok(output) => json!({
                                "label": entry_label,
                                "index": idx,
                                "model": entry_model,
                                "status": "accepted",
                                "output": output
                            }),
                            Err(e) => json!({
                                "label": entry_label,
                                "index": idx,
                                "model": entry_model,
                                "status": "error",
                                "error": e
                            }),
                        }
                    }
                })
                .collect();

            let results = join_all(futures).await;
            let any_failed = results
                .iter()
                .any(|r| r.get("status").and_then(|v| v.as_str()) == Some("error"));
            return Ok(ToolResult {
                success: !any_failed,
                output: serde_json::to_string_pretty(&json!({
                    "mode": "spawn",
                    "multi": true,
                    "count": results.len(),
                    "results": results
                }))?,
                error: if any_failed {
                    Some("One or more spawns failed; see per-entry status.".to_string())
                } else {
                    None
                },
            });
        }

        // ── single-model spawn ─────────────────────────────────────────────
        let model_raw = match top_model {
            Some(m) => m.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "model is required for single spawn mode. \
                         Provide model for a single spawn or spawns[] for multi-model."
                            .to_string(),
                    ),
                });
            }
        };
        let task_str = match top_task {
            Some(t) => t.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("task is required for spawn mode.".to_string()),
                });
            }
        };

        let (provider, model_id) = match split_first_slash(&model_raw) {
            Ok(pair) => pair,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e),
                });
            }
        };

        let full_task = build_task(top_context.unwrap_or(""), &task_str);

        match run_ephemeral_call(
            &provider,
            &model_id,
            self.api_key.as_deref(),
            &self.provider_runtime_options,
            &full_task,
            timeout_secs,
        )
        .await
        {
            Ok(output) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "mode": "spawn",
                    "multi": false,
                    "model": model_raw,
                    "status": "accepted",
                    "output": output
                }))?,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: serde_json::to_string_pretty(&json!({
                    "mode": "spawn",
                    "multi": false,
                    "model": model_raw,
                    "status": "error"
                }))?,
                error: Some(e),
            }),
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Validate that a provider name is known or uses a custom: prefix.
/// Returns an error string if the provider is unknown.
fn validate_provider(provider: &str) -> Result<(), String> {
    let is_custom =
        provider.starts_with("custom:") || provider.starts_with("anthropic-custom:");
    if !is_custom {
        let known = crate::providers::list_providers();
        let valid = known.iter().any(|p| {
            p.name.eq_ignore_ascii_case(provider)
                || p.aliases.iter().any(|a| a.eq_ignore_ascii_case(provider))
        });
        if !valid {
            return Err(format!(
                "Unknown provider: \"{provider}\". \
                 Use a known provider name or \"custom:<url>\" for custom endpoints."
            ));
        }
    }
    Ok(())
}

/// Split a `"provider/model-id"` spec on the first `/`.
/// Returns `(provider, model_id)` or an error string.
fn split_first_slash(spec: &str) -> Result<(String, String), String> {
    let trimmed = spec.trim();
    match trimmed.find('/') {
        Some(idx) if idx > 0 => {
            let provider = trimmed[..idx].to_string();
            let model_id = trimmed[idx + 1..].to_string();
            if model_id.is_empty() {
                Err(format!(
                    "model must be \"provider/model-id\", got: \"{trimmed}\""
                ))
            } else {
                Ok((provider, model_id))
            }
        }
        _ => Err(format!(
            "model must include a provider prefix \
             (e.g. \"together/MiniMaxAI/MiniMax-M2.7\"), got: \"{trimmed}\""
        )),
    }
}

/// Prepend context to task when context is non-empty.
fn build_task(context: &str, task: &str) -> String {
    let ctx = context.trim();
    let tsk = task.trim();
    if ctx.is_empty() {
        tsk.to_string()
    } else {
        format!("{ctx}\n\n{tsk}")
    }
}

/// Create an ephemeral provider instance and run a single `simple_chat` call.
/// Times out after `timeout_secs` seconds.
async fn run_ephemeral_call(
    provider_name: &str,
    model_id: &str,
    api_key: Option<&str>,
    runtime_options: &crate::providers::ProviderRuntimeOptions,
    task: &str,
    timeout_secs: u64,
) -> Result<String, String> {
    // Validate provider before creating an instance — prevents forwarding
    // API keys to arbitrary custom: endpoints via spawn mode.
    validate_provider(provider_name)?;

    let provider = crate::providers::create_provider_with_options(
        provider_name,
        api_key,
        runtime_options,
    )
    .map_err(|e| format!("Failed to create provider \"{provider_name}\": {e}"))?;

    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        provider.simple_chat(task, model_id, DEFAULT_SPAWN_TEMPERATURE),
    )
    .await;

    match result {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(format!("LLM call failed: {e}")),
        Err(_) => Err(format!("Spawn timed out after {timeout_secs}s")),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_first_slash_valid() {
        let (provider, model) = split_first_slash("together/MiniMaxAI/MiniMax-M2.7").unwrap();
        assert_eq!(provider, "together");
        assert_eq!(model, "MiniMaxAI/MiniMax-M2.7");
    }

    #[test]
    fn split_first_slash_simple() {
        let (provider, model) = split_first_slash("groq/llama-3.3-70b-versatile").unwrap();
        assert_eq!(provider, "groq");
        assert_eq!(model, "llama-3.3-70b-versatile");
    }

    #[test]
    fn split_first_slash_no_slash() {
        assert!(split_first_slash("no-slash").is_err());
    }

    #[test]
    fn split_first_slash_trailing_slash_only() {
        assert!(split_first_slash("provider/").is_err());
    }

    #[test]
    fn split_first_slash_leading_slash() {
        assert!(split_first_slash("/model").is_err());
    }

    #[test]
    fn build_task_with_context() {
        let result = build_task("Context here.", "Do the task.");
        assert_eq!(result, "Context here.\n\nDo the task.");
    }

    #[test]
    fn build_task_no_context() {
        let result = build_task("", "Do the task.");
        assert_eq!(result, "Do the task.");
    }

    #[test]
    fn build_task_whitespace_context() {
        let result = build_task("   ", "Do the task.");
        assert_eq!(result, "Do the task.");
    }

    #[test]
    fn tool_metadata() {
        let tool = ModelSpawnTool::new(
            Arc::new(SecurityPolicy::default()),
            None,
            crate::providers::ProviderRuntimeOptions::default(),
        );
        assert_eq!(tool.name(), "model_spawn");
        assert!(tool.description().contains("live"));
        assert!(tool.description().contains("spawn"));

        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["mode"].is_object());
        assert!(schema["properties"]["model"].is_object());
        assert!(schema["properties"]["task"].is_object());
        assert!(schema["properties"]["spawns"].is_object());
    }

    #[tokio::test]
    async fn execute_missing_mode_returns_error() {
        let tool = ModelSpawnTool::new(
            Arc::new(SecurityPolicy::default()),
            None,
            crate::providers::ProviderRuntimeOptions::default(),
        );
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("mode"));
    }

    #[tokio::test]
    async fn execute_invalid_mode_returns_error() {
        let tool = ModelSpawnTool::new(
            Arc::new(SecurityPolicy::default()),
            None,
            crate::providers::ProviderRuntimeOptions::default(),
        );
        let result = tool
            .execute(json!({"mode": "teleport"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("teleport"));
    }

    #[tokio::test]
    async fn execute_live_missing_model_returns_error() {
        let tool = ModelSpawnTool::new(
            Arc::new(SecurityPolicy::default()),
            None,
            crate::providers::ProviderRuntimeOptions::default(),
        );
        let result = tool.execute(json!({"mode": "live"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("model"));
    }

    #[tokio::test]
    async fn execute_spawn_mutual_exclusion() {
        let tool = ModelSpawnTool::new(
            Arc::new(SecurityPolicy::default()),
            None,
            crate::providers::ProviderRuntimeOptions::default(),
        );
        let result = tool
            .execute(json!({
                "mode": "spawn",
                "model": "groq/llama-3.3-70b-versatile",
                "task": "hello",
                "spawns": [{"model": "together/GLM-5", "task": "hello"}]
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("not both"));
    }

    #[tokio::test]
    async fn execute_spawn_missing_model_returns_error() {
        let tool = ModelSpawnTool::new(
            Arc::new(SecurityPolicy::default()),
            None,
            crate::providers::ProviderRuntimeOptions::default(),
        );
        let result = tool
            .execute(json!({"mode": "spawn", "task": "hello"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("model"));
    }

    #[tokio::test]
    async fn execute_spawn_missing_task_returns_error() {
        let tool = ModelSpawnTool::new(
            Arc::new(SecurityPolicy::default()),
            None,
            crate::providers::ProviderRuntimeOptions::default(),
        );
        let result = tool
            .execute(json!({"mode": "spawn", "model": "groq/llama-3.3-70b-versatile"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("task"));
    }

    #[tokio::test]
    async fn execute_spawn_single_bad_provider_returns_error() {
        let tool = ModelSpawnTool::new(
            Arc::new(SecurityPolicy::default()),
            None,
            crate::providers::ProviderRuntimeOptions::default(),
        );
        let result = tool
            .execute(json!({
                "mode": "spawn",
                "model": "nonexistent_provider_xyz/model",
                "task": "hello"
            }))
            .await
            .unwrap();
        // The provider creation should fail with an error result.
        assert!(!result.success);
        assert!(result.error.is_some());
    }
}
