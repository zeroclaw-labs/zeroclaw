use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};

use crate::control_plane::{
    GoalCommand, GoalCommandAction, admit_goal_command, current_goal_admission_context,
};

pub struct GoalStartTool {
    agent_alias: String,
    config: std::sync::Arc<zeroclaw_config::schema::Config>,
}

impl GoalStartTool {
    pub fn new(
        agent_alias: impl Into<String>,
        config: std::sync::Arc<zeroclaw_config::schema::Config>,
    ) -> Self {
        Self {
            agent_alias: agent_alias.into(),
            config,
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
        "goal_start"
    }

    fn description(&self) -> &str {
        crate::i18n::get_tool_description("goal_start")
            .expect("goal_start tool description must be in Fluent catalogue")
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": crate::i18n::get_required_tool_string(
                        "tool-goal-start-objective-description",
                    )
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

        let Some(ctx) = current_goal_admission_context() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-start-error-missing-context",
                )),
            });
        };
        if ctx.agent_alias != self.agent_alias {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-start-error-agent-context-mismatch",
                )),
            });
        }

        let admission = admit_goal_command(
            ctx,
            GoalCommand {
                action: GoalCommandAction::Start,
                objective: Some(objective.to_string()),
                task_id: None,
                budgets: Default::default(),
            },
            self.config.as_ref(),
            self.config.agent(&self.agent_alias),
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
    use crate::control_plane::{
        GoalAdmissionContext, GoalCommand, GoalCommandAction, TaskContinuationContext,
        TaskContinuationConversationScope, TaskRegistry, admit_goal_command, control_plane,
        init_control_plane, scope_goal_admission_context,
    };
    use std::sync::Arc;

    #[test]
    fn tool_schema_requires_only_untrusted_objective() {
        let tool = GoalStartTool::new("agent-a", std::sync::Arc::new(Default::default()));
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "objective");
        assert!(schema["properties"].get("agent_alias").is_none());
        assert!(schema["properties"].get("principal_id").is_none());
        assert!(schema["properties"].get("originator_route").is_none());
    }

    #[tokio::test]
    async fn tool_started_goal_uses_scoped_trusted_route() {
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let store: Arc<dyn TaskRegistry> = match control_plane() {
            Some(control_plane) => Arc::clone(&control_plane.store),
            None => {
                let store: Arc<dyn TaskRegistry> =
                    Arc::new(crate::control_plane::SqliteTaskStore::new_in_memory().unwrap());
                let _ = init_control_plane(crate::control_plane::ControlPlaneHandle {
                    store: Arc::clone(&store),
                    boot_id: "test-boot".into(),
                    recovered_goal_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
                });
                Arc::clone(&control_plane().unwrap().store)
            }
        };
        let mut config = zeroclaw_config::schema::Config::default();
        config.goal.allowed_channel_types.push("channel".into());
        let tool = GoalStartTool::new(agent.clone(), std::sync::Arc::new(config.clone()));
        let continuation_context = TaskContinuationContext {
            channel: "channel".into(),
            channel_alias: Some("default".into()),
            reply_target: "room-a".into(),
            sender: "operator-a".into(),
            thread_ts: None,
            interruption_scope_id: None,
            conversation_scope: TaskContinuationConversationScope::ReplyTarget,
        };
        let owner = GoalAdmissionContext::new(agent.clone())
            .with_originator_route(Some("channel:route-a".into()))
            .with_principal_id(Some("principal-a".into()))
            .with_continuation_context(Some(continuation_context.clone()));

        let result = scope_goal_admission_context(
            Some(owner.clone()),
            tool.execute(serde_json::json!({"objective": "ship trusted goal"})),
        )
        .await
        .unwrap();
        assert!(result.success, "{result:?}");

        let task = store
            .latest_active_goal_for_agent(&agent)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.originator_route.as_deref(), Some("channel:route-a"));
        assert_eq!(task.principal_id.as_deref(), Some("principal-a"));
        assert_eq!(
            store.get_continuation_context(&task.id).await.unwrap(),
            Some(continuation_context)
        );

        let wrong_route = GoalAdmissionContext::new(agent)
            .with_originator_route(Some("channel:route-b".into()))
            .with_principal_id(Some("principal-a".into()));
        let err = admit_goal_command(
            wrong_route,
            GoalCommand {
                action: GoalCommandAction::Status,
                objective: None,
                task_id: Some(task.id),
                budgets: Default::default(),
            },
            &config,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not visible from this route"));
    }
}
