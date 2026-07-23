use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};

use crate::control_plane::{
    GoalAdmission, GoalCommand, GoalCommandAction, admit_goal_command,
    current_goal_admission_context,
};

/// Model-callable entry point for amending the current durable goal objective.
///
/// The model supplies only replacement objective text. The current goal,
/// principal, route, and agent identity are resolved from the trusted runtime
/// `GoalAdmissionContext`; this tool must not accept those facts as arguments.
pub struct GoalObjectiveTool {
    /// Agent alias this tool instance was registered for.
    agent_alias: String,
    /// Shared config used by the control-plane admission function.
    config: std::sync::Arc<zeroclaw_config::schema::Config>,
}

impl GoalObjectiveTool {
    pub const NAME: &'static str = "goal_objective";

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

/// JSON arguments accepted from the model-callable `goal_objective` tool.
///
/// Objective text is untrusted prompt input. The tool updates only the
/// canonical goal extension row selected by trusted runtime context.
#[derive(Debug, Deserialize)]
struct GoalObjectiveArgs {
    /// Replacement objective text for the current non-terminal goal.
    objective: String,
}

#[async_trait]
impl Tool for GoalObjectiveTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        crate::i18n::get_tool_description("goal_objective")
            .expect("goal_objective tool description must be in Fluent catalogue")
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "objective": {
                    "type": "string",
                    "description": crate::i18n::get_required_tool_string(
                        "tool-goal-objective-objective-description",
                    )
                }
            },
            "required": ["objective"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let args: GoalObjectiveArgs = serde_json::from_value(args)?;
        let objective = args.objective.trim();
        if objective.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-objective-error-empty-objective",
                )),
            });
        }

        let Some(ctx) = current_goal_admission_context() else {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-objective-error-missing-context",
                )),
            });
        };
        if ctx.agent_alias != self.agent_alias {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-objective-error-agent-context-mismatch",
                )),
            });
        }

        let admission = admit_goal_command(
            ctx,
            GoalCommand {
                action: GoalCommandAction::Objective,
                objective: Some(objective.to_string()),
                task_id: None,
                resume_reason: None,
                budgets: Default::default(),
            },
            self.config.as_ref(),
            self.config.agent(&self.agent_alias),
        )
        .await?;
        let output = goal_objective_tool_output(&admission);

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

fn goal_objective_tool_output(admission: &GoalAdmission) -> String {
    crate::i18n::get_required_tool_string_with_args(
        "tool-goal-objective-success",
        &[("message", &admission.message)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::{
        GoalAdmissionContext, GoalTaskRecord, GoalTaskRegistry, TaskRecord, TaskRegistry,
        TaskStatus, control_plane, init_control_plane, scope_goal_admission_context,
    };
    use std::sync::Arc;

    #[test]
    fn tool_schema_requires_only_untrusted_objective() {
        let tool = GoalObjectiveTool::new("agent-a", std::sync::Arc::new(Default::default()));
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "objective");
        assert!(schema["properties"].get("task_id").is_none());
        assert!(schema["properties"].get("agent_alias").is_none());
        assert!(schema["properties"].get("principal_id").is_none());
        assert!(schema["properties"].get("originator_route").is_none());
    }

    #[tokio::test]
    async fn tool_updates_current_goal_objective_with_trusted_context() {
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        ensure_control_plane();
        let control_plane = control_plane().unwrap();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        control_plane
            .goal_store
            .create_goal(
                TaskRecord {
                    id: task_id.clone(),
                    kind: crate::control_plane::TaskKind::Goal,
                    agent: agent.clone(),
                    status: TaskStatus::Running,
                    owner_pid: std::process::id(),
                    owner_boot_id: "test-boot".into(),
                    heartbeat_at: None,
                    depth: 0,
                    parent_id: None,
                    originator_route: Some(route.clone()),
                    delivered: false,
                    idem_key: None,
                    principal_id: Some(principal.clone()),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    finished_at: None,
                },
                GoalTaskRecord {
                    task_id: task_id.clone(),
                    objective: "ship initial scope".into(),
                    effective_token_limit: None,
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();

        let mut config = zeroclaw_config::schema::Config::default();
        config.goal.enabled = true;
        config.goal.allowed_channel_types = vec!["test-channel".into()];
        let tool = GoalObjectiveTool::new(agent.clone(), std::sync::Arc::new(config));
        let owner = GoalAdmissionContext::new(agent)
            .with_channel_type(Some("test-channel".into()))
            .with_originator_route(Some(route))
            .with_principal_id(Some(principal));

        let result = scope_goal_admission_context(
            Some(owner),
            tool.execute(serde_json::json!({
                "objective": "ship amended scope after evidence"
            })),
        )
        .await
        .unwrap();

        assert!(result.success, "{result:?}");
        assert!(result.output.contains("objective updated"));
        assert!(
            result
                .output
                .contains("Continue under the amended objective")
        );
        let goal = control_plane
            .goal_store
            .get_goal_task(&task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.objective, "ship amended scope after evidence");
    }

    fn ensure_control_plane() {
        if control_plane().is_some() {
            return;
        }
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
}
