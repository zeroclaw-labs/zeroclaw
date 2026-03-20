use anyhow::Result;
use serde::Serialize;

use super::models::NewSprint;
use super::CreateOsDb;

/// Result of a single orchestrator tick.
#[derive(Debug, Clone, Serialize)]
pub struct OrchestratorResult {
    /// Sprints that were completed (all tasks done).
    pub sprints_completed: Vec<String>,
    /// Sprint that was activated (planned -> active).
    pub sprint_activated: Option<String>,
    /// Sprint that was auto-planned (created because approved tasks had no sprint).
    pub sprint_auto_planned: Option<String>,
    /// Next task picked for execution.
    pub next_task_id: Option<String>,
    /// Next task title (for display).
    pub next_task_title: Option<String>,
}

/// Run one orchestrator tick:
///
/// 1. Sprint lifecycle: complete done sprints, activate planned ones, auto-plan if needed
/// 2. Pick next task (highest priority approved)
pub async fn run_once(db: &CreateOsDb) -> Result<OrchestratorResult> {
    let mut result = OrchestratorResult {
        sprints_completed: Vec::new(),
        sprint_activated: None,
        sprint_auto_planned: None,
        next_task_id: None,
        next_task_title: None,
    };

    // --- Phase 1: Complete active sprints where all tasks are done ---
    let active_sprints = db.list_sprints(Some("active")).await?;
    for sprint in &active_sprints {
        let (done, total) = db.count_sprint_tasks(&sprint.id.to_string()).await?;
        if total > 0 && done == total {
            db.update_sprint_status(&sprint.id.to_string(), "completed")
                .await?;
            result.sprints_completed.push(sprint.id.to_string());
        }
    }

    // --- Phase 2: Activate a planned (not_started) sprint if none are active ---
    let still_active = db.list_sprints(Some("active")).await?;
    if still_active.is_empty() {
        let planned = db.list_sprints(Some("not_started")).await?;
        if let Some(next_sprint) = planned.first() {
            db.update_sprint_status(&next_sprint.id.to_string(), "active")
                .await?;
            result.sprint_activated = Some(next_sprint.id.to_string());
        }
    }

    // --- Phase 3: Auto-plan a sprint if there are approved tasks with no sprint ---
    let still_active = db.list_sprints(Some("active")).await?;
    if still_active.is_empty() {
        let approved_tasks = db.list_tasks(Some("approved"), None).await?;
        let unassigned: Vec<_> = approved_tasks
            .iter()
            .filter(|t| t.sprint_id.is_none() || t.sprint_id.as_deref() == Some(""))
            .collect();

        if !unassigned.is_empty() {
            let now = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let sprint = db
                .create_sprint(NewSprint {
                    name: format!("Auto-sprint {now}"),
                    objectives: format!("{} approved tasks queued", unassigned.len()),
                    domain_id: None,
                    epic_id: None,
                    start_date: Some(now),
                    end_date: None,
                })
                .await?;

            // Activate the auto-created sprint
            db.update_sprint_status(&sprint.id.to_string(), "active")
                .await?;

            // Assign unassigned approved tasks to this sprint
            let sprint_id_str = sprint.id.to_string();
            for task in &unassigned {
                let update = super::models::TaskUpdate {
                    sprint_id: Some(sprint_id_str.clone()),
                    ..Default::default()
                };
                db.update_task(&task.id.to_string(), update).await?;
            }

            result.sprint_auto_planned = Some(sprint_id_str);
        }
    }

    // --- Phase 4: Pick next task ---
    if let Some(task) = db.pick_next_task().await? {
        result.next_task_id = Some(task.id.to_string());
        result.next_task_title = Some(task.title.clone());
    }

    Ok(result)
}
