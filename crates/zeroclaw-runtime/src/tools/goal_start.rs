use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};

use crate::control_plane::{
    GoalAdmission, GoalCommand, GoalCommandAction, admit_goal_command,
    current_goal_admission_context,
};

/// Model-callable entry point into the same goal admission path as `/goal start`.
///
/// The tool does not trust model-supplied route, principal, or agent identity.
/// It accepts only the objective from the model and requires the surrounding
/// runtime to have installed a `GoalAdmissionContext` first.
pub struct GoalStartTool {
    /// Agent alias this tool instance was registered for.
    agent_alias: String,
    /// Shared config used by the control-plane admission function.
    config: std::sync::Arc<zeroclaw_config::schema::Config>,
}

impl GoalStartTool {
    pub const NAME: &'static str = "goal_start";

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

/// JSON arguments accepted from the model-callable `goal_start` tool.
///
/// Keep this intentionally tiny. The model may propose an objective, but every
/// trusted fact needed to bind the goal to an agent, route, principal, and
/// continuation context comes from the task-local `GoalAdmissionContext`.
#[derive(Debug, Deserialize)]
struct GoalStartArgs {
    /// Model-supplied objective text. The controller treats it as untrusted
    /// prompt input, never as routing or authorization data.
    objective: String,
}

#[async_trait]
impl Tool for GoalStartTool {
    fn name(&self) -> &str {
        Self::NAME
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
                output: String::new().into(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-start-error-empty-objective",
                )),
            });
        }

        let Some(ctx) = current_goal_admission_context() else {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-start-error-missing-context",
                )),
            });
        };
        if ctx.agent_alias != self.agent_alias {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
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
                resume_reason: None,
                budgets: Default::default(),
            },
            self.config.as_ref(),
            self.config.agent(&self.agent_alias),
        )
        .await?;
        if admission.continue_goal {
            let task_id = admission.task_id.as_deref().ok_or_else(|| {
                anyhow::Error::msg("continuing goal admission returned no exact task id")
            })?;
            if !crate::control_plane::bind_current_goal_task(task_id) {
                anyhow::bail!("goal admission could not bind its exact live task");
            }
            crate::agent::cost::enable_current_tool_loop_goal_attribution(self.config.as_ref());
            crate::control_plane::mark_current_goal_turn_for_evaluation();
        }
        let output = goal_start_tool_output(&admission);

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

fn goal_start_tool_output(admission: &GoalAdmission) -> String {
    let message_key = if admission.continue_goal {
        "tool-goal-start-success-continue"
    } else {
        "tool-goal-start-success-paused"
    };
    crate::i18n::get_required_tool_string_with_args(message_key, &[("message", &admission.message)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::{
        GoalAdmissionContext, GoalCommand, GoalCommandAction, GoalTaskRegistry,
        TaskContinuationContext, TaskContinuationConversationScope, TaskRegistry,
        admit_goal_command, control_plane, init_control_plane, scope_goal_admission_context,
        scope_goal_turn_evaluation_marker,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

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
        let (_store, goal_store): (Arc<dyn TaskRegistry>, Arc<dyn GoalTaskRegistry>) =
            match control_plane() {
                Some(control_plane) => (
                    Arc::clone(&control_plane.store),
                    Arc::clone(&control_plane.goal_store),
                ),
                None => {
                    let sqlite_store =
                        Arc::new(crate::control_plane::SqliteTaskStore::new_in_memory().unwrap());
                    let store: Arc<dyn TaskRegistry> = sqlite_store.clone();
                    let goal_store: Arc<dyn GoalTaskRegistry> = sqlite_store;
                    let _ = init_control_plane(crate::control_plane::ControlPlaneHandle {
                        store: Arc::clone(&store),
                        goal_store: Arc::clone(&goal_store),
                        boot_id: "test-boot".into(),
                        recovered_goal_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
                        data_dir_lock: None,
                    });
                    (
                        Arc::clone(&control_plane().unwrap().store),
                        Arc::clone(&control_plane().unwrap().goal_store),
                    )
                }
            };
        let mut config = zeroclaw_config::schema::Config::default();
        config.goal.enabled = true;
        let tool = GoalStartTool::new(agent.clone(), std::sync::Arc::new(config.clone()));
        let continuation_context = TaskContinuationContext {
            channel: "matrix".into(),
            channel_alias: Some("default".into()),
            reply_target: "room-a".into(),
            sender: "operator-a".into(),
            thread_ts: None,
            interruption_scope_id: None,
            conversation_scope: TaskContinuationConversationScope::ReplyTarget,
        };
        let owner = GoalAdmissionContext::new(agent.clone())
            .with_channel_type(Some("matrix".into()))
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
        assert!(
            result
                .output
                .contains("Continue working on this active goal now")
        );

        let task = goal_store
            .latest_active_goal_for_agent(&agent)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.originator_route.as_deref(), Some("channel:route-a"));
        assert_eq!(task.principal_id.as_deref(), Some("principal-a"));
        assert_eq!(
            goal_store.get_continuation_context(&task.id).await.unwrap(),
            Some(continuation_context)
        );

        let wrong_route = GoalAdmissionContext::new(agent)
            .with_channel_type(Some("matrix".into()))
            .with_originator_route(Some("channel:route-b".into()))
            .with_principal_id(Some("principal-a".into()));
        let err = admit_goal_command(
            wrong_route,
            GoalCommand {
                action: GoalCommandAction::Status,
                objective: None,
                task_id: Some(task.id),
                resume_reason: None,
                budgets: Default::default(),
            },
            &config,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not visible from this route"));
    }

    #[tokio::test]
    async fn continued_goal_start_marks_current_turn_for_evaluation() {
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        if control_plane().is_none() {
            let sqlite_store =
                Arc::new(crate::control_plane::SqliteTaskStore::new_in_memory().unwrap());
            let store: Arc<dyn TaskRegistry> = sqlite_store.clone();
            let goal_store: Arc<dyn GoalTaskRegistry> = sqlite_store;
            let _ = init_control_plane(crate::control_plane::ControlPlaneHandle {
                store,
                goal_store,
                boot_id: "test-boot".into(),
                recovered_goal_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
                data_dir_lock: None,
            });
        }
        let mut config = zeroclaw_config::schema::Config::default();
        config.goal.enabled = true;
        let tool = GoalStartTool::new(agent.clone(), std::sync::Arc::new(config));
        let owner = GoalAdmissionContext::new(agent)
            .with_channel_type(Some("matrix".into()))
            .with_originator_route(Some(format!("route-{}", uuid::Uuid::new_v4())))
            .with_principal_id(Some(format!("principal-{}", uuid::Uuid::new_v4())));
        let marker = Arc::new(AtomicBool::new(false));

        let result = scope_goal_turn_evaluation_marker(
            Some(Arc::clone(&marker)),
            scope_goal_admission_context(
                Some(owner),
                tool.execute(serde_json::json!({"objective": "ship trusted goal"})),
            ),
        )
        .await
        .unwrap();

        assert!(result.success, "{result:?}");
        assert!(
            marker.load(Ordering::Acquire),
            "continued model goal_start admission must mark this tool loop for goal evaluation"
        );
    }

    #[test]
    fn paused_goal_start_output_does_not_instruct_continuation() {
        let output = goal_start_tool_output(&GoalAdmission {
            task_id: Some("goal-paused".into()),
            status: crate::control_plane::TaskStatus::Paused,
            message: "⏸️ Goal `goal-paused` started but paused.".into(),
            continuation_reason: None,
            continue_goal: false,
        });

        assert!(output.contains("Goal `goal-paused` started but paused."));
        assert!(
            output.contains("Do not continue this goal now"),
            "paused admission must not instruct the model to spend another turn: {output}"
        );
        assert!(!output.contains("Continue working on this active goal now"));
    }
}
