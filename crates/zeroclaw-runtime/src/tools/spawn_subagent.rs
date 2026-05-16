//! Agent-loop tool that spawns an ephemeral SubAgent inheriting the
//! parent's identity, security policy, and memory allowlist, runs a
//! focused prompt, and returns the response. Cron's `JobType::Agent`
//! dispatch is the other SubAgent spawn site; both funnel through
//! [`crate::subagent::SubAgentSpawn`] so permission inheritance,
//! tracing-span shape, and audit attribution stay uniform.

use crate::agent::loop_::AgentRunOverrides;
use crate::subagent::{SubAgentOverrides, SubAgentSpawn};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::Config;
use zeroclaw_log::scope;

/// Spawn an ephemeral SubAgent that inherits the parent agent's
/// identity and runs a focused prompt under the same alias.
pub struct SpawnSubagentTool {
    config: Arc<Config>,
    parent_alias: String,
}

impl SpawnSubagentTool {
    pub fn new(config: Arc<Config>, parent_alias: impl Into<String>) -> Self {
        Self {
            config,
            parent_alias: parent_alias.into(),
        }
    }
}

#[async_trait]
impl Tool for SpawnSubagentTool {
    fn name(&self) -> &str {
        "spawn_subagent"
    }

    fn description(&self) -> &str {
        "Spawn an ephemeral SubAgent that inherits this agent's identity, \
         security policy, and memory allowlist. The SubAgent runs the supplied \
         prompt to completion under the parent's permissions envelope and \
         returns its response. Use for focused subtasks (research lookup, \
         multi-step reasoning, etc.) that should not pollute this agent's main \
         conversation history. Cost-aware: each SubAgent run is a full agent \
         loop and consumes provider tokens."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task or question for the SubAgent. Be specific and self-contained — the SubAgent does not see this conversation's history."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        // Argument validation surfaces as a structured `ToolResult`
        // failure (matching the unknown-parent and run-failure shapes
        // below) so the agent loop receives a uniform "tool reported
        // failure" signal regardless of which step rejected the call.
        let prompt = match args
            .get("prompt")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(p) => p.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing or empty 'prompt' parameter".into()),
                });
            }
        };

        // The agent-loop tool inherits the parent's identity verbatim;
        // narrowing-override knobs land on the tool argument schema
        // alongside the [agents.<alias>].subagent_* config block.
        let subagent_ctx = match SubAgentSpawn::for_agent(&self.config, &self.parent_alias)
            .and_then(|spawn| spawn.build(SubAgentOverrides::default()))
        {
            Ok(ctx) => ctx,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("subagent spawn failed: {e:#}")),
                });
            }
        };

        let run_id = uuid::Uuid::new_v4().to_string();

        let temperature: Option<f64> = self
            .config
            .model_provider_for_agent(&self.parent_alias)
            .and_then(|e| e.temperature);
        let session_path = std::path::PathBuf::from(format!("subagent-{run_id}"));

        // Pass the validated SubAgent context as run-time overrides so
        // the subset-confirmed policy reaches the agent loop instead
        // of being silently re-derived from config. Memory override
        // stays `None` for v0.8.0 inherits-verbatim — once the
        // `[agents.<alias>].subagent_*` config block lands, the
        // validated allowlist will plumb a narrowed AgentScopedMemory
        // into this slot.
        let run_overrides = AgentRunOverrides {
            security: Some(subagent_ctx.policy.clone()),
            memory: None,
        };
        let parent_alias = subagent_ctx.parent_alias.clone();
        let run_result = Box::pin(scope!(
            agent_alias: parent_alias,
            session_key: run_id,
            =>
            crate::agent::run(
                (*self.config).clone(),
                &self.parent_alias,
                Some(prompt),
                None,
                None,
                temperature,
                vec![],
                false,
                Some(session_path),
                None,
                run_overrides,
            )
        ))
        .await;

        match run_result {
            Ok(response) => Ok(ToolResult {
                success: true,
                output: if response.trim().is_empty() {
                    "subagent completed without output".to_string()
                } else {
                    response
                },
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("subagent run failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

    fn config_with_agent(alias: &str) -> Config {
        let mut config = Config::default();
        config
            .risk_profiles
            .insert("default".to_string(), RiskProfileConfig::default());
        config.agents.insert(
            alias.to_string(),
            AliasedAgentConfig {
                risk_profile: "default".to_string(),
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    #[tokio::test]
    async fn empty_or_missing_prompt_is_rejected() {
        let tool = SpawnSubagentTool::new(Arc::new(config_with_agent("alpha")), "alpha");
        for args in [json!({}), json!({ "prompt": "   " })] {
            let result = tool
                .execute(args)
                .await
                .expect("execute returns Ok with structured failure");
            assert!(!result.success);
            assert!(
                result
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("prompt"),
                "expected prompt-validation error, got: {:?}",
                result.error
            );
        }
    }

    #[tokio::test]
    async fn unknown_parent_alias_surfaces_spawn_failure() {
        // Parent alias that is not configured: SubAgentSpawn::for_agent
        // returns Err, the tool reports a structured spawn failure
        // (no panic, no recursion attempt).
        let tool = SpawnSubagentTool::new(Arc::new(Config::default()), "missing-alpha");
        let result = tool
            .execute(json!({ "prompt": "hello" }))
            .await
            .expect("execute returns Ok with structured failure");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("subagent spawn failed"),
            "expected spawn-failure error, got: {:?}",
            result.error
        );
    }
}
