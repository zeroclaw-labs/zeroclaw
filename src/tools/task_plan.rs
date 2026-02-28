//! Session-scoped task checklist for tracking multi-step work.
//!
//! Provides a `task_plan` tool that lets the agent break complex work into
//! steps and track progress within a single session. The task list lives in
//! memory (`Arc<RwLock<Vec<TaskItem>>>`) and is discarded when the session
//! ends — it is intentionally not persisted via the Memory trait.

use crate::security::{policy::ToolOperation, SecurityPolicy};
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::fmt;
use std::sync::{Arc, RwLock};

// ── Data Structures ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Completed => write!(f, "completed"),
        }
    }
}

impl TaskStatus {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(TaskStatus::Pending),
            "in_progress" => Some(TaskStatus::InProgress),
            "completed" => Some(TaskStatus::Completed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct TaskItem {
    id: usize,
    title: String,
    status: TaskStatus,
}

// ── Tool ─────────────────────────────────────────────────────────────────

pub struct TaskPlanTool {
    security: Arc<SecurityPolicy>,
    tasks: Arc<RwLock<Vec<TaskItem>>>,
    next_id: Arc<RwLock<usize>>,
}

impl TaskPlanTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            tasks: Arc::new(RwLock::new(Vec::new())),
            next_id: Arc::new(RwLock::new(1)),
        }
    }

    /// Enforce mutation permission (autonomy + rate limit).
    fn enforce_mutation(&self) -> Result<(), ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Act, "task_plan")
            .map_err(|msg| ToolResult {
                success: false,
                output: String::new(),
                error: Some(msg),
            })
    }

    fn handle_create(&self, tasks_val: &serde_json::Value) -> ToolResult {
        let arr = match tasks_val.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Parameter 'tasks' must be a non-empty array of {title, status?}".into(),
                    ),
                };
            }
        };

        let mut items = Vec::with_capacity(arr.len());
        let mut id = 1usize;
        for entry in arr {
            let title = match entry.get("title").and_then(|v| v.as_str()) {
                Some(t) if !t.is_empty() => t.to_string(),
                _ => {
                    return ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Each task must have a non-empty 'title' string".into()),
                    };
                }
            };
            let status = entry
                .get("status")
                .and_then(|v| v.as_str())
                .and_then(TaskStatus::from_str)
                .unwrap_or(TaskStatus::Pending);
            items.push(TaskItem { id, title, status });
            id += 1;
        }

        let count = items.len();
        *self.tasks.write().unwrap() = items;
        *self.next_id.write().unwrap() = id;

        ToolResult {
            success: true,
            output: format!("Created {count} task(s)."),
            error: None,
        }
    }

    fn handle_add(&self, title: &str) -> ToolResult {
        if title.is_empty() {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some("Parameter 'title' must be a non-empty string".into()),
            };
        }

        let mut next_id = self.next_id.write().unwrap();
        let id = *next_id;
        *next_id += 1;

        self.tasks.write().unwrap().push(TaskItem {
            id,
            title: title.to_string(),
            status: TaskStatus::Pending,
        });

        ToolResult {
            success: true,
            output: format!("Added task [{id}] \"{title}\"."),
            error: None,
        }
    }

    fn handle_update(&self, id: usize, status_str: &str) -> ToolResult {
        let status = match TaskStatus::from_str(status_str) {
            Some(s) => s,
            None => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid status '{status_str}'. Must be: pending, in_progress, completed"
                    )),
                };
            }
        };

        let mut tasks = self.tasks.write().unwrap();
        match tasks.iter_mut().find(|t| t.id == id) {
            Some(task) => {
                task.status = status;
                ToolResult {
                    success: true,
                    output: format!("Task [{id}] updated to {status}."),
                    error: None,
                }
            }
            None => ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Task with id {id} not found")),
            },
        }
    }

    fn handle_list(&self) -> ToolResult {
        let tasks = self.tasks.read().unwrap();
        if tasks.is_empty() {
            return ToolResult {
                success: true,
                output: "No tasks.".into(),
                error: None,
            };
        }

        let completed = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .count();
        let total = tasks.len();

        let mut lines = vec![format!("Tasks ({completed}/{total} completed):")];
        for t in tasks.iter() {
            lines.push(format!("- [{}] [{}] {}", t.id, t.status, t.title));
        }

        ToolResult {
            success: true,
            output: lines.join("\n"),
            error: None,
        }
    }

    fn handle_delete(&self) -> ToolResult {
        self.tasks.write().unwrap().clear();
        *self.next_id.write().unwrap() = 1;

        ToolResult {
            success: true,
            output: "Task list cleared.".into(),
            error: None,
        }
    }
}

#[async_trait]
impl Tool for TaskPlanTool {
    fn name(&self) -> &str {
        "task_plan"
    }

    fn description(&self) -> &str {
        "Manage a task checklist for the current session. Use to break complex work into steps and track progress.\n\
         Actions: create (batch), add (single), update (change status), list (view all), delete (clear all)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "add", "update", "list", "delete"],
                    "description": "Operation to perform"
                },
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            }
                        },
                        "required": ["title"]
                    },
                    "description": "For 'create': list of tasks to create (replaces existing list)"
                },
                "title": {
                    "type": "string",
                    "description": "For 'add': title of the new task"
                },
                "id": {
                    "type": "integer",
                    "description": "For 'update': ID of the task to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "For 'update': new status"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        match action {
            "create" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                let tasks_val = args.get("tasks").cloned().unwrap_or(json!([]));
                Ok(self.handle_create(&tasks_val))
            }
            "add" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                let title = args
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                Ok(self.handle_add(title))
            }
            "update" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                #[allow(clippy::cast_possible_truncation)]
                let id = args.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let status = args
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if id == 0 {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Parameter 'id' is required for update".into()),
                    });
                }
                if status.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Parameter 'status' is required for update".into()),
                    });
                }
                Ok(self.handle_update(id, status))
            }
            "list" => Ok(self.handle_list()),
            "delete" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                Ok(self.handle_delete())
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid: create, add, update, list, delete"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn default_tool() -> TaskPlanTool {
        TaskPlanTool::new(Arc::new(SecurityPolicy::default()))
    }

    fn readonly_tool() -> TaskPlanTool {
        TaskPlanTool::new(Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        }))
    }

    #[test]
    fn tool_name_and_schema() {
        let tool = default_tool();
        assert_eq!(tool.name(), "task_plan");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["tasks"].is_object());
        assert!(schema["properties"]["id"].is_object());
        assert!(schema["properties"]["status"].is_object());
    }

    #[tokio::test]
    async fn create_and_list() {
        let tool = default_tool();

        let r = tool
            .execute(json!({
                "action": "create",
                "tasks": [
                    { "title": "step one" },
                    { "title": "step two" },
                    { "title": "step three", "status": "completed" }
                ]
            }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("3 task(s)"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("1/3 completed"));
        assert!(r.output.contains("[1] [pending] step one"));
        assert!(r.output.contains("[2] [pending] step two"));
        assert!(r.output.contains("[3] [completed] step three"));
    }

    #[tokio::test]
    async fn add_task() {
        let tool = default_tool();

        // Create initial list
        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "first" }]
        }))
        .await
        .unwrap();

        // Add a task — should get id=2
        let r = tool
            .execute(json!({ "action": "add", "title": "second" }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("[2]"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.output.contains("[1] [pending] first"));
        assert!(r.output.contains("[2] [pending] second"));
    }

    #[tokio::test]
    async fn update_status() {
        let tool = default_tool();

        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "do thing" }]
        }))
        .await
        .unwrap();

        let r = tool
            .execute(json!({ "action": "update", "id": 1, "status": "in_progress" }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("in_progress"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.output.contains("[in_progress]"));
    }

    #[tokio::test]
    async fn update_nonexistent_id() {
        let tool = default_tool();

        let r = tool
            .execute(json!({ "action": "update", "id": 999, "status": "completed" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn create_replaces_existing() {
        let tool = default_tool();

        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "old task" }]
        }))
        .await
        .unwrap();

        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "new task" }]
        }))
        .await
        .unwrap();

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(!r.output.contains("old task"));
        assert!(r.output.contains("new task"));
        // ID should reset to 1
        assert!(r.output.contains("[1]"));
    }

    #[tokio::test]
    async fn delete_clears_all() {
        let tool = default_tool();

        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "will be deleted" }]
        }))
        .await
        .unwrap();

        let r = tool.execute(json!({ "action": "delete" })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("cleared"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.output.contains("No tasks"));
    }

    #[tokio::test]
    async fn readonly_blocks_mutations() {
        let tool = readonly_tool();

        for action in &["create", "add", "update", "delete"] {
            let mut args = json!({ "action": action });
            if *action == "create" {
                args["tasks"] = json!([{ "title": "t" }]);
            }
            if *action == "add" {
                args["title"] = json!("t");
            }
            if *action == "update" {
                args["id"] = json!(1);
                args["status"] = json!("completed");
            }
            let r = tool.execute(args).await.unwrap();
            assert!(
                !r.success,
                "action '{action}' should be blocked in read-only"
            );
            assert!(r.error.unwrap().contains("read-only"));
        }
    }

    #[tokio::test]
    async fn list_works_in_readonly() {
        let tool = readonly_tool();
        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.success);
    }

    #[tokio::test]
    async fn unknown_action_returns_failure() {
        let tool = default_tool();
        let r = tool.execute(json!({ "action": "nope" })).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn create_with_empty_tasks_fails() {
        let tool = default_tool();
        let r = tool
            .execute(json!({ "action": "create", "tasks": [] }))
            .await
            .unwrap();
        assert!(!r.success);
    }

    #[tokio::test]
    async fn update_missing_params_fails() {
        let tool = default_tool();

        // Missing id
        let r = tool
            .execute(json!({ "action": "update", "status": "completed" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("id"));

        // Missing status
        let r = tool
            .execute(json!({ "action": "update", "id": 1 }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("status"));
    }

    #[tokio::test]
    async fn invalid_status_value_fails() {
        let tool = default_tool();
        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "t" }]
        }))
        .await
        .unwrap();

        let r = tool
            .execute(json!({ "action": "update", "id": 1, "status": "invalid" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Invalid status"));
    }

    #[tokio::test]
    async fn add_empty_title_fails() {
        let tool = default_tool();
        let r = tool
            .execute(json!({ "action": "add", "title": "" }))
            .await
            .unwrap();
        assert!(!r.success);
    }

    #[tokio::test]
    async fn list_empty_shows_no_tasks() {
        let tool = default_tool();
        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("No tasks"));
    }
}
