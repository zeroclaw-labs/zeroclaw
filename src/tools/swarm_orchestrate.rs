use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use futures_util::future::join_all;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
enum SwarmStrategy {
    Sequential,
    Parallel,
    Adaptive,
}

impl SwarmStrategy {
    fn parse(value: Option<&str>) -> Self {
        match value
            .unwrap_or("adaptive")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "sequential" => Self::Sequential,
            "parallel" => Self::Parallel,
            _ => Self::Adaptive,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Parallel => "parallel",
            Self::Adaptive => "adaptive",
        }
    }
}

#[derive(Debug, Clone)]
struct AgentExecutionResult {
    agent: String,
    success: bool,
    output: String,
    error: Option<String>,
}

pub struct SwarmOrchestrateTool {
    agents: Vec<String>,
    delegate: Option<Arc<dyn Tool>>,
}

impl SwarmOrchestrateTool {
    pub fn new(
        agents: HashMap<String, crate::config::DelegateAgentConfig>,
        delegate: Option<Arc<dyn Tool>>,
    ) -> Self {
        let mut names = agents.keys().cloned().collect::<Vec<_>>();
        names.sort();
        Self {
            agents: names,
            delegate,
        }
    }

    async fn run_delegate(
        delegate: Arc<dyn Tool>,
        agent: String,
        prompt: String,
        context: Option<String>,
    ) -> anyhow::Result<AgentExecutionResult> {
        let mut args = serde_json::json!({
            "agent": agent,
            "prompt": prompt,
        });
        if let Some(ctx) = context.filter(|c| !c.trim().is_empty()) {
            args["context"] = serde_json::Value::String(ctx);
        }

        let result = delegate.execute(args).await?;
        Ok(AgentExecutionResult {
            agent,
            success: result.success,
            output: result.output,
            error: result.error,
        })
    }

    async fn execute_sequential(
        &self,
        task: &str,
        agents: &[String],
    ) -> anyhow::Result<Vec<AgentExecutionResult>> {
        let delegate = self
            .delegate
            .clone()
            .ok_or_else(|| anyhow::anyhow!("delegate tool is not available"))?;

        let mut context = String::new();
        let mut results = Vec::new();
        for (idx, agent) in agents.iter().enumerate() {
            let prompt = format!(
                "Swarm task: {task}\nStep {} / {}\nAgent role: {}\nReturn concise result.",
                idx + 1,
                agents.len(),
                agent
            );
            let result = Self::run_delegate(
                delegate.clone(),
                agent.clone(),
                prompt,
                Some(context.clone()),
            )
            .await?;
            if result.success {
                context.push_str(&format!("\n[{agent}] {}", result.output));
            }
            results.push(result);
        }
        Ok(results)
    }

    async fn execute_parallel(
        &self,
        task: &str,
        agents: &[String],
    ) -> anyhow::Result<Vec<AgentExecutionResult>> {
        let delegate = self
            .delegate
            .clone()
            .ok_or_else(|| anyhow::anyhow!("delegate tool is not available"))?;

        let futures = agents.iter().map(|agent| {
            let prompt = format!(
                "Swarm task: {task}\nParallel role: {}\nReturn concise result.",
                agent
            );
            Self::run_delegate(delegate.clone(), agent.clone(), prompt, None)
        });

        let mut results = Vec::new();
        for item in join_all(futures).await {
            results.push(item?);
        }
        Ok(results)
    }
}

#[async_trait]
impl Tool for SwarmOrchestrateTool {
    fn name(&self) -> &str {
        "swarm_orchestrate"
    }

    fn description(&self) -> &str {
        "Coordinate master/slave agent swarm execution using configured delegate agents. Supports sequential, parallel, and adaptive strategies and returns a synthesized final report."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Main task for swarm coordination"
                },
                "agents": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional subset of agents to use"
                },
                "strategy": {
                    "type": "string",
                    "enum": ["sequential", "parallel", "adaptive"],
                    "default": "adaptive"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let task = match args.get("task").and_then(|v| v.as_str()) {
            Some(value) if !value.trim().is_empty() => value.trim(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: task".to_string()),
                });
            }
        };

        let strategy = SwarmStrategy::parse(args.get("strategy").and_then(|v| v.as_str()));

        let requested_agents = args
            .get("agents")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| self.agents.clone());

        let selected_agents = requested_agents
            .into_iter()
            .filter(|agent| self.agents.iter().any(|name| name == agent))
            .collect::<Vec<_>>();

        if selected_agents.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("no valid delegate agents selected".to_string()),
            });
        }

        if self.delegate.is_none() {
            let output = serde_json::json!({
                "task": task,
                "strategy": strategy.as_str(),
                "agents": selected_agents,
                "note": "Delegate tool unavailable; returning swarm plan only"
            });
            return Ok(ToolResult {
                success: true,
                output: output.to_string(),
                error: None,
            });
        }

        let effective = match strategy {
            SwarmStrategy::Adaptive if selected_agents.len() <= 2 => SwarmStrategy::Sequential,
            SwarmStrategy::Adaptive => SwarmStrategy::Parallel,
            other => other,
        };

        let results = match effective {
            SwarmStrategy::Sequential => self.execute_sequential(task, &selected_agents).await?,
            SwarmStrategy::Parallel => self.execute_parallel(task, &selected_agents).await?,
            SwarmStrategy::Adaptive => unreachable!(),
        };

        let success = results.iter().all(|r| r.success);
        let summary = results
            .iter()
            .map(|result| {
                serde_json::json!({
                    "agent": result.agent,
                    "success": result.success,
                    "output": result.output,
                    "error": result.error,
                })
            })
            .collect::<Vec<_>>();

        let output = serde_json::json!({
            "task": task,
            "strategy_requested": strategy.as_str(),
            "strategy_executed": effective.as_str(),
            "agent_count": selected_agents.len(),
            "success": success,
            "results": summary,
        });

        Ok(ToolResult {
            success,
            output: output.to_string(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent_config() -> crate::config::DelegateAgentConfig {
        crate::config::DelegateAgentConfig {
            provider: "openrouter".to_string(),
            model: "gpt-4o-mini".to_string(),
            system_prompt: None,
            api_key: None,
            temperature: None,
            max_depth: 3,
            agentic: false,
            allowed_tools: Vec::new(),
            max_iterations: 10,
        }
    }

    fn tool_without_delegate() -> SwarmOrchestrateTool {
        let mut agents = HashMap::new();
        agents.insert("researcher".to_string(), test_agent_config());
        agents.insert("writer".to_string(), test_agent_config());
        SwarmOrchestrateTool::new(agents, None)
    }

    #[tokio::test]
    async fn execute_requires_task() {
        let result = tool_without_delegate()
            .execute(serde_json::json!({}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn execute_returns_plan_without_delegate() {
        let result = tool_without_delegate()
            .execute(serde_json::json!({ "task": "research rust", "strategy": "parallel" }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("research rust"));
        assert!(result.output.contains("plan only"));
    }
}
