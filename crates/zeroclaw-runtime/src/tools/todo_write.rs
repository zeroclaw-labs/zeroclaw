use async_trait::async_trait;
use serde_json::{Value, json};
use zeroclaw_api::plan::{PlanEntry, PlanPriority, PlanStatus};
use zeroclaw_api::tool::{Tool, ToolResult};

/// Live task tracker tool. Models call this with the COMPLETE current
/// todo list on every invocation (whole-list replace). The tool only
/// validates and normalizes; the tool-execution layer emits the
/// resulting `TurnEvent::Plan`.
pub struct TodoWriteTool;

impl TodoWriteTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TodoWriteTool {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parse_entries(args: &Value) -> anyhow::Result<Vec<PlanEntry>> {
    let todos = args
        .get("todos")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::Error::msg("`todos` must be an array"))?;

    let mut out = Vec::with_capacity(todos.len());
    for (i, item) in todos.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| anyhow::Error::msg(format!("todos[{i}] must be an object")))?;

        let content = obj
            .get("content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::Error::msg(format!("todos[{i}].content is required")))?
            .to_string();

        let status = match obj.get("status").and_then(Value::as_str) {
            Some("pending") => PlanStatus::Pending,
            Some("in_progress") => PlanStatus::InProgress,
            Some("completed") => PlanStatus::Completed,
            other => {
                return Err(anyhow::Error::msg(format!(
                    "todos[{i}].status must be pending|in_progress|completed, got {other:?}"
                )));
            }
        };

        let priority = match obj.get("priority").and_then(Value::as_str) {
            Some("high") => PlanPriority::High,
            Some("low") => PlanPriority::Low,
            _ => PlanPriority::Medium,
        };

        let active_form = obj
            .get("activeForm")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        out.push(PlanEntry {
            content,
            status,
            priority,
            active_form,
        });
    }
    Ok(out)
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn description(&self) -> &str {
        "Render a live task tracker for the current work. Call this with the COMPLETE \
         current todo list every time — the new list wholly replaces the previous one. \
         Each todo has `content` (imperative description), `status` (pending, in_progress, \
         or completed), and optionally `priority` (high, medium, low) and `activeForm` \
         (present-continuous label shown while in_progress). Keep exactly one item \
         in_progress at a time. Pass an empty list to clear the tracker."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete current todo list (whole-list replace).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Imperative task description" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "priority": { "type": "string", "enum": ["high", "medium", "low"] },
                            "activeForm": { "type": "string", "description": "Present-continuous label shown while in_progress" }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        match parse_entries(&args) {
            Ok(entries) => {
                let total = entries.len();
                let done = entries
                    .iter()
                    .filter(|e| e.status == PlanStatus::Completed)
                    .count();
                Ok(ToolResult {
                    success: true,
                    output: format!("{total} todos tracked ({done} done)").into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(e.to_string()),
            }),
        }
    }
}

zeroclaw_api::tool_attribution!(TodoWriteTool, zeroclaw_api::attribution::ToolKind::Plugin);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zeroclaw_api::plan::{PlanPriority, PlanStatus};
    use zeroclaw_api::tool::Tool;

    #[test]
    fn parses_valid_entries_with_all_fields() {
        let args = json!({
            "todos": [
                { "content": "A", "status": "completed", "priority": "high", "activeForm": "Doing A" },
                { "content": "B", "status": "in_progress" }
            ]
        });
        let entries = parse_entries(&args).expect("should parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].status, PlanStatus::Completed);
        assert_eq!(entries[0].priority, PlanPriority::High);
        assert_eq!(entries[0].active_form.as_deref(), Some("Doing A"));
        assert_eq!(entries[1].priority, PlanPriority::Medium);
        assert_eq!(entries[1].active_form, None);
    }

    #[test]
    fn empty_list_is_valid_clear() {
        let args = json!({ "todos": [] });
        let entries = parse_entries(&args).expect("empty is valid");
        assert!(entries.is_empty());
    }

    #[test]
    fn rejects_missing_content() {
        let args = json!({ "todos": [ { "status": "pending" } ] });
        assert!(parse_entries(&args).is_err());
    }

    #[test]
    fn rejects_invalid_status() {
        let args = json!({ "todos": [ { "content": "A", "status": "bogus" } ] });
        assert!(parse_entries(&args).is_err());
    }

    #[test]
    fn coerces_unknown_priority_to_medium() {
        let args =
            json!({ "todos": [ { "content": "A", "status": "pending", "priority": "urgent" } ] });
        let entries = parse_entries(&args).expect("unknown priority coerced");
        assert_eq!(entries[0].priority, PlanPriority::Medium);
    }

    #[test]
    fn rejects_missing_todos_key() {
        let args = json!({});
        assert!(parse_entries(&args).is_err());
    }

    #[tokio::test]
    async fn execute_returns_success_summary() {
        let tool = TodoWriteTool::new();
        let args = json!({ "todos": [
            { "content": "A", "status": "completed" },
            { "content": "B", "status": "pending" }
        ]});
        let res = tool.execute(args).await.unwrap();
        assert!(res.success);
        assert!(res.output.contains("2"));
        assert!(res.output.contains("1"));
    }

    #[tokio::test]
    async fn execute_rejects_bad_args() {
        let tool = TodoWriteTool::new();
        let args = json!({ "todos": [ { "status": "pending" } ] });
        let res = tool.execute(args).await.unwrap();
        assert!(!res.success);
        assert!(res.error.is_some());
    }

    #[test]
    fn tool_advertises_model_facing_name() {
        assert_eq!(TodoWriteTool::new().name(), "TodoWrite");
    }
}
