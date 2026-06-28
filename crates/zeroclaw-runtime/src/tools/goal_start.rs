use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};

use crate::control_plane::{
    GoalAdmissionContext, GoalCommand, GoalCommandAction, admit_goal_command,
};

pub struct GoalStartTool {
    agent_alias: String,
}

impl GoalStartTool {
    pub fn new(agent_alias: impl Into<String>) -> Self {
        Self {
            agent_alias: agent_alias.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GoalStartArgs {
    objective: String,
}

#[async_trait]
impl Tool for GoalStartTool {
    fn name(&self) -> &str {
        "goal.start"
    }

    fn description(&self) -> &str {
        "Start a durable goal run. The objective is untrusted user/model text; runtime-owned agent, route, owner, and principal facts are supplied by ZeroClaw."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": "Goal objective to pursue."
                }
            },
            "required": ["objective"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let args: GoalStartArgs = serde_json::from_value(args)?;
        let objective = args.objective.trim();
        if objective.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-start-error-empty-objective",
                )),
            });
        }

        let admission = admit_goal_command(
            GoalAdmissionContext::new(self.agent_alias.clone()),
            GoalCommand {
                action: GoalCommandAction::Start,
                objective: Some(objective.to_string()),
                task_id: None,
            },
        )
        .await?;

        Ok(ToolResult {
            success: true,
            output: admission.message,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schema_requires_only_untrusted_objective() {
        let tool = GoalStartTool::new("agent-a");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "objective");
        assert!(schema["properties"].get("agent_alias").is_none());
        assert!(schema["properties"].get("principal_id").is_none());
        assert!(schema["properties"].get("originator_route").is_none());
    }
}
