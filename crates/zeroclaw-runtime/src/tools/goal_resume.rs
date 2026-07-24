use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};

use crate::control_plane::{
    GoalAdmission, GoalCommand, GoalCommandAction, admit_goal_command,
    current_goal_admission_context,
};

/// Model-callable entry point into the same goal admission path as `/goal resume`.
///
/// The tool accepts only an optional untrusted resume reason from the model.
/// Agent identity, route, principal, current-goal selection, and continuation
/// context must come from the surrounding runtime `GoalAdmissionContext`.
pub struct GoalResumeTool {
    /// Agent alias this tool instance was registered for.
    agent_alias: String,
    /// Shared config used by the control-plane admission function.
    config: std::sync::Arc<zeroclaw_config::schema::Config>,
}

impl GoalResumeTool {
    pub const NAME: &'static str = "goal_resume";

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

/// JSON arguments accepted from the model-callable `goal_resume` tool.
///
/// `reason` is an untrusted operator/model statement about what changed since
/// the pause. It is prompt input for the next continuation only, not a durable
/// update to the goal's blocker or pause metadata.
#[derive(Debug, Default, Deserialize)]
struct GoalResumeArgs {
    /// Optional untrusted reason to include in the continuation prompt.
    reason: Option<String>,
}

#[async_trait]
impl Tool for GoalResumeTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        crate::i18n::get_tool_description("goal_resume")
            .expect("goal_resume tool description must be in Fluent catalogue")
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": crate::i18n::get_required_tool_string(
                        "tool-goal-resume-reason-description",
                    )
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let args: GoalResumeArgs = serde_json::from_value(args)?;
        let reason = nonempty(args.reason);

        let Some(ctx) = current_goal_admission_context() else {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-resume-error-missing-context",
                )),
            });
        };
        if ctx.agent_alias != self.agent_alias {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-goal-resume-error-agent-context-mismatch",
                )),
            });
        }

        let admission = admit_goal_command(
            ctx,
            GoalCommand {
                action: GoalCommandAction::Resume,
                objective: None,
                task_id: None,
                resume_reason: reason,
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
        let output = goal_resume_tool_output(&admission);

        Ok(ToolResult {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn goal_resume_tool_output(admission: &GoalAdmission) -> String {
    let message_key = if admission.continue_goal {
        "tool-goal-resume-success-continue"
    } else {
        "tool-goal-resume-success-paused"
    };
    crate::i18n::get_required_tool_string_with_args(message_key, &[("message", &admission.message)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::{
        GoalAdmissionContext, GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalTaskRecord,
        GoalTaskRegistry, TaskRecord, TaskRegistry, TaskStatus, control_plane, init_control_plane,
        scope_goal_admission_context, scope_goal_turn_evaluation_marker,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn tool_schema_accepts_only_untrusted_resume_inputs() {
        let tool = GoalResumeTool::new("agent-a", std::sync::Arc::new(Default::default()));
        let schema = tool.parameters_schema();
        assert!(schema.get("required").is_none());
        assert!(schema["properties"].get("reason").is_some());
        assert!(schema["properties"].get("task_id").is_none());
        assert!(schema["properties"].get("agent_alias").is_none());
        assert!(schema["properties"].get("principal_id").is_none());
        assert!(schema["properties"].get("originator_route").is_none());
    }

    #[tokio::test]
    async fn tool_resumes_goal_with_untrusted_reason() {
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
                    status: TaskStatus::Paused,
                    owner_pid: 0,
                    owner_boot_id: "old-boot".into(),
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
                    objective: "finish blocked work".into(),
                    effective_token_limit: None,
                    effective_cost_limit_usd: None,
                    pause_reason: Some(GoalPauseReason::NeedsUserInput),
                    pause_description: Some("waiting for operator".into()),
                    blockers: vec![GoalBlocker {
                        kind: GoalBlockerKind::NeedsUserInput,
                        message: "Need operator action".into(),
                        payload: None,
                    }],
                },
                None,
            )
            .await
            .unwrap();
        let mut config = zeroclaw_config::schema::Config::default();
        config.goal.enabled = true;
        config.goal.allowed_channel_types = vec!["test-channel".into()];
        let tool = GoalResumeTool::new(agent.clone(), std::sync::Arc::new(config));
        let owner = GoalAdmissionContext::new(agent)
            .with_channel_type(Some("test-channel".into()))
            .with_originator_route(Some(route))
            .with_principal_id(Some(principal));
        let marker = Arc::new(AtomicBool::new(false));

        let result = scope_goal_turn_evaluation_marker(
            Some(Arc::clone(&marker)),
            scope_goal_admission_context(
                Some(owner),
                tool.execute(serde_json::json!({
                    "reason": "The external blocker is fixed; retry the blocked action."
                })),
            ),
        )
        .await
        .unwrap();

        assert!(result.success, "{result:?}");
        assert!(
            result
                .output
                .contains("Continue working on this resumed goal now")
        );
        assert!(
            marker.load(Ordering::Acquire),
            "continued model goal_resume admission must mark this tool loop for goal evaluation"
        );
    }

    #[test]
    fn paused_goal_resume_output_does_not_instruct_continuation() {
        let output = goal_resume_tool_output(&GoalAdmission {
            task_id: Some("goal-paused".into()),
            status: TaskStatus::Paused,
            message: "⏸️ Goal `goal-paused` remains paused.".into(),
            continuation_reason: None,
            continue_goal: false,
        });

        assert!(output.contains("Goal `goal-paused` remains paused."));
        assert!(
            output.contains("Do not continue this goal now"),
            "paused admission must not instruct the model to spend another turn: {output}"
        );
        assert!(!output.contains("Continue working on this resumed goal now"));
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
