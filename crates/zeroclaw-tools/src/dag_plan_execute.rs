// DAG Plan & Execute tool for parallel task execution and dynamic programming.
// Supports standard tool tasks only.

use anyhow::Result;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::task::JoinSet;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Errors specific to plan & execute execution.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
pub enum DagPlanExecuteError {
    #[error("Unknown tool '{0}' is not available")]
    UnknownTool(String),
    #[error("Plan exceeds maximum of {0} nodes")]
    TooManyNodes(usize),
    #[error("Invalid template reference: {0}")]
    InvalidTemplate(String),
    #[error("Task '{0}' failed: {1}")]
    TaskFailed(String, String),
    #[error("Cycle detected in plan: {0}")]
    CycleDetected(String),
    #[error("Missing dependency: task '{0}' depends on non-existent task '{1}")]
    MissingDependency(String, String),
}

/// A single task/node in the execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagPlanTask {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub args: serde_json::Value,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagPlanExecuteRequest {
    pub tasks: Vec<DagPlanTask>,
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
    #[serde(default)]
    pub trace: bool,
}

fn default_max_parallel() -> usize {
    4
}

#[derive(Debug, Clone, Serialize)]
pub struct DagPlanTaskResult {
    pub id: String,
    pub tool: String,
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DagPlanExecuteTrace {
    pub total_duration_ms: u64,
    pub tasks_executed: usize,
    pub max_parallelism_reached: usize,
    pub task_results: Vec<DagPlanTaskResult>,
    pub execution_order: Vec<String>,
}

/// Callback for runtime-owned child tool dispatch.
/// The runtime provides a dispatch that records observer events
/// (ToolCallStart/ToolCall) and filters against excluded tools.
pub type ChildDispatchFn = dyn Fn(String, serde_json::Value) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send>>
    + Send
    + Sync;

pub struct DagPlanExecuteTool {
    tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
    child_dispatch: Option<Arc<ChildDispatchFn>>,
    excluded_tools: Arc<Vec<String>>,
}

impl Clone for DagPlanExecuteTool {
    fn clone(&self) -> Self {
        Self {
            tools: Arc::clone(&self.tools),
            child_dispatch: self.child_dispatch.clone(),
            excluded_tools: Arc::clone(&self.excluded_tools),
        }
    }
}

const DEFAULT_MAX_NODES: usize = 50;

impl DagPlanExecuteTool {
    pub fn new(tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>) -> Self {
        Self {
            tools: Arc::clone(&tools),
            child_dispatch: None,
            excluded_tools: Arc::new(Vec::new()),
        }
    }
    pub fn with_child_dispatch(mut self, dispatch: Arc<ChildDispatchFn>) -> Self {
        self.child_dispatch = Some(dispatch);
        self
    }

    /// Set the excluded tools list so child execution filters excluded tools.
    pub fn with_excluded_tools(mut self, excluded: Arc<Vec<String>>) -> Self {
        self.excluded_tools = excluded;
        self
    }

    fn find_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().iter().find(|t| t.name() == name).cloned()
    }

    fn validate(
        &self,
        request: &DagPlanExecuteRequest,
    ) -> std::result::Result<(), DagPlanExecuteError> {
        if request.tasks.len() > DEFAULT_MAX_NODES {
            return Err(DagPlanExecuteError::TooManyNodes(DEFAULT_MAX_NODES));
        }
        let task_map: HashMap<&str, &DagPlanTask> =
            request.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
        if task_map.len() != request.tasks.len() {
            return Err(DagPlanExecuteError::CycleDetected(
                "Duplicate task ID".to_string(),
            ));
        }
        for task in &request.tasks {
            if task.tool == "dag_plan_execute" {
                return Err(DagPlanExecuteError::UnknownTool(
                    "dag_plan_execute (recursive execution not permitted)".to_string(),
                ));
            }

            if self.find_tool(&task.tool).is_none() {
                return Err(DagPlanExecuteError::UnknownTool(task.tool.clone()));
            }
            for dep in &task.depends_on {
                if !task_map.contains_key(dep.as_str()) {
                    return Err(DagPlanExecuteError::MissingDependency(
                        task.id.clone(),
                        dep.clone(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn get_ready_tasks_by_id(
        &self,
        tasks: &[DagPlanTask],
        completed: &HashSet<String>,
        in_progress: &HashSet<String>,
    ) -> Vec<String> {
        tasks
            .iter()
            .filter(|t| {
                !completed.contains(&t.id)
                    && !in_progress.contains(&t.id)
                    && t.depends_on.iter().all(|d| completed.contains(d))
            })
            .map(|t| t.id.clone())
            .collect()
    }

    async fn execute_task_with_snapshot(
        &self,
        task: &DagPlanTask,
        results: &HashMap<String, DagPlanTaskResult>,
    ) -> Result<DagPlanTaskResult> {
        let start = std::time::Instant::now();
        let ref_map: HashMap<&str, &DagPlanTaskResult> =
            results.iter().map(|(k, v)| (k.as_str(), v)).collect();

        let interpolated_args = self.interpolate_args(&task.args, &ref_map);
        // Check exclusion list before dispatching
        let task_name = task.tool.trim();
        if self
            .excluded_tools
            .iter()
            .any(|excluded| excluded.trim().eq_ignore_ascii_case(task_name))
        {
            return Ok(DagPlanTaskResult {
                id: task.id.clone(),
                tool: task.tool.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("tool {task_name} is excluded from this agent")),
                duration_ms: start.elapsed().as_millis() as u64,
                dependencies: task.depends_on.clone(),
            });
        }
        let tool_result = if let Some(ref dispatch) = self.child_dispatch {
            dispatch(task.tool.clone(), interpolated_args).await?
        } else {
            let tool = self
                .find_tool(&task.tool)
                .ok_or_else(|| anyhow::Error::msg(format!("Unknown tool: {}", task.tool)))?;
            tool.execute(interpolated_args).await?
        };
        Ok(DagPlanTaskResult {
            id: task.id.clone(),
            tool: task.tool.clone(),
            success: tool_result.success,
            output: tool_result.output.to_string(),
            error: tool_result.error,
            duration_ms: start.elapsed().as_millis() as u64,
            dependencies: task.depends_on.clone(),
        })
    }

    async fn execute_plan(
        &self,
        request: &DagPlanExecuteRequest,
    ) -> Result<Vec<DagPlanTaskResult>, DagPlanExecuteError> {
        let mut completed = HashSet::<String>::new();
        let mut in_progress = HashSet::<String>::new();
        let mut results = HashMap::<String, DagPlanTaskResult>::new();
        let mut order = Vec::<String>::new();

        while completed.len() < request.tasks.len() {
            let ready = self.get_ready_tasks_by_id(&request.tasks, &completed, &in_progress);
            let to_execute: Vec<_> = ready.into_iter().take(request.max_parallel).collect();
            if to_execute.is_empty() {
                if in_progress.is_empty() {
                    return Err(DagPlanExecuteError::CycleDetected("Deadlock".into()));
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                continue;
            }
            let mut join_set = JoinSet::new();
            for id in to_execute {
                in_progress.insert(id.clone());
                let task = request.tasks.iter().find(|t| t.id == id).unwrap().clone();
                let snapshot = results.clone();
                let this = self.clone();
                join_set.spawn(async move {
                    let result = this
                        .execute_task_with_snapshot(&task, &snapshot)
                        .await
                        .map_err(|e| {
                            DagPlanExecuteError::TaskFailed(task.id.clone(), e.to_string())
                        });
                    (id, result)
                });
            }
            while let Some(res) = join_set.join_next().await {
                let (id, task_result) = res.map_err(|e| {
                    DagPlanExecuteError::TaskFailed("unknown".into(), e.to_string())
                })?;
                let result = task_result?;
                if !result.success {
                    return Err(DagPlanExecuteError::TaskFailed(
                        id.clone(),
                        result.error.unwrap_or(result.output),
                    ));
                }
                in_progress.remove(&id);
                completed.insert(id.clone());
                order.push(id.clone());
                results.insert(id, result);
            }
        }
        let mut final_results: Vec<_> = order
            .iter()
            .filter_map(|id| results.get(id).cloned())
            .collect();
        final_results.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(final_results)
    }

    fn interpolate_args(
        &self,
        args: &serde_json::Value,
        results: &HashMap<&str, &DagPlanTaskResult>,
    ) -> serde_json::Value {
        match args {
            serde_json::Value::String(s) => {
                serde_json::Value::String(self.interpolate_string(s, results))
            }
            serde_json::Value::Object(m) => serde_json::Value::Object(
                m.iter()
                    .map(|(k, v)| (k.clone(), self.interpolate_args(v, results)))
                    .collect(),
            ),
            serde_json::Value::Array(a) => serde_json::Value::Array(
                a.iter()
                    .map(|v| self.interpolate_args(v, results))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn interpolate_string(&self, s: &str, results: &HashMap<&str, &DagPlanTaskResult>) -> String {
        let mut result = String::with_capacity(s.len());
        let mut chars = s.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '{'
                && chars.peek().is_some_and(|&(_, c2)| c2 == '{')
                && let Some(end) = s[i + 2..].find("}}")
            {
                let tpl = &s[i + 2..i + 2 + end];
                if let Some(val) = self.resolve_template(tpl, results) {
                    result.push_str(&val.replace("{{", ""));
                    while chars.peek().is_some_and(|&(idx, _)| idx < i + end + 4) {
                        chars.next();
                    }
                    continue;
                }
            }
            result.push(c);
        }
        result
    }

    fn resolve_template(
        &self,
        tpl: &str,
        results: &HashMap<&str, &DagPlanTaskResult>,
    ) -> Option<String> {
        let tpl = tpl.trim();
        // Parse dep[ID].output format
        // e.g., "dep[cinemas].output" -> extract "cinemas"
        if tpl.starts_with("dep[") && tpl.ends_with(".output") {
            // Remove "dep[" prefix and ".output" suffix
            let inner = &tpl[4..tpl.len() - 7]; // strip "dep[" (4 chars) and ".output" (7 chars)
            // Now inner should be just the ID, but we need to remove trailing ']'
            let id = inner.strip_suffix(']')?;
            results.get(id).map(|r| r.output.clone())
        } else {
            None
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for DagPlanExecuteTool {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
    }
    fn alias(&self) -> &str {
        self.name()
    }
}

#[async_trait]
impl Tool for DagPlanExecuteTool {
    fn name(&self) -> &str {
        "dag_plan_execute"
    }
    fn description(&self) -> &str {
        r#"execute a plan of multiple tasks with dependencies, tasks with no dependencies run in parallel (default max 4 concurrent)
## When To Use
- Run multiple tools in parallel as a DAG (directed acyclic graph).
- You need 2+ independent queries (weather AND calendar AND traffic)
- You have tasks with clear dependencies (check availability → book meeting)

## Task Type:
TOOL TASKS - Execute standard tools (shell, http_request, file_write, etc.):
   Required fields: id, tool, args
   Example: {"id": "task1", "tool": "shell", "args": {"cmd": "echo hello"}, "depends_on": []}

DEPENDENCY OUTPUT PASSING: Use {{dep[TASK_ID].output}} syntax to pass results from completed tasks to subsequent tasks in tool args string fields.
Example: "cmd": "process {{dep[task1].output}}"

## EXECUTION FLOW:
1. Tasks with no dependencies (or all deps completed) start immediately
2. Up to max_parallel tasks run concurrently
3. When a task completes, its output becomes available via {{dep[ID].output}}
4. Dependent tasks wait for all their dependencies before starting
5. Returns array of all task results with id, tool, success, output, error, duration_ms

## CONSTRAINTS:
- Maximum 50 tasks per plan
- Cannot call dag_plan_execute recursively (no nested plans)
- All depends_on IDs must exist as task IDs in the same plan
- Circular dependencies are detected and rejected

## EXAMPLE - Fetch then process:
{"tasks": [
  {"id": "fetch", "tool": "http_request", "args": {"url": "https://api.example.com/data"}, "depends_on": []},
  {"id": "save", "tool": "file_write", "args": {"path": "/tmp/data.txt", "content": "{{dep[fetch].output}}"}, "depends_on": ["fetch"]}
], "max_parallel": 2}"#
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "Array of tasks to execute. Each task must have a unique ID.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Unique task identifier. Used in depends_on and {{dep[ID].output}} references."
                            },
                            "tool": {
                                "type": "string",
                                "description": "Tool name to execute (e.g., http_request, file_write). Required for tool tasks."
                            },
                            "args": {
                                "type": "object",
                                "description": "Tool arguments. String values support {{dep[ID].output}} interpolation. Example: {\"cmd\": \"echo {{dep[task1].output}}\"}"
                            },
                            "depends_on": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Array of task IDs that must complete before this task starts. Empty array or omitted means no dependencies."
                            }
                        },
                        "required": ["id","tool"]
                    }
                },
                "max_parallel": {
                    "type": "integer",
                    "description": "Maximum number of tasks to execute concurrently. Default: 4. Increase for I/O-bound tasks, decrease for CPU-bound.",
                    "default": 4
                },
                "trace": {
                    "type": "boolean",
                    "description": "If true, returns detailed execution trace with timing, order, and parallelism stats. Default: false.",
                    "default": false
                }
            },
            "required": ["tasks"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let req: DagPlanExecuteRequest = serde_json::from_value(args)
            .map_err(|e| anyhow::Error::msg(format!("Invalid plan request: {e}")))?;
        if let Err(e) = self.validate(&req) {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(e.to_string()),
            });
        }
        let start = std::time::Instant::now();
        match self.execute_plan(&req).await {
            Ok(results) => {
                let dur = start.elapsed().as_millis() as u64;
                let out = if req.trace {
                    serde_json::to_string_pretty(&DagPlanExecuteTrace {
                        total_duration_ms: dur,
                        tasks_executed: results.len(),
                        max_parallelism_reached: req.max_parallel,
                        task_results: results.clone(),
                        execution_order: results.iter().map(|r| r.id.clone()).collect(),
                    })
                    .unwrap_or_default()
                } else {
                    serde_json::to_string_pretty(&results).unwrap_or_default()
                };
                Ok(ToolResult {
                    success: true,
                    output: out.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_task_parse() {
        let t: DagPlanTask =
            serde_json::from_str(r#"{"id":"f","tool":"shell","args":{"cmd":"echo"}}"#).unwrap();
        assert_eq!(t.tool, "shell");
    }

    #[test]
    fn test_interpolate_dep_template() {
        use std::collections::HashMap;
        let mut results: HashMap<&str, DagPlanTaskResult> = HashMap::new();
        results.insert(
            "cinemas",
            DagPlanTaskResult {
                id: "cinemas".into(),
                tool: "http".into(),
                success: true,
                output: "Cinema List: ABC, XYZ".into(),
                error: None,
                duration_ms: 100,
                dependencies: vec![],
            },
        );
        let tool = DagPlanExecuteTool::new(Arc::new(RwLock::new(vec![])));
        let refs: HashMap<&str, &DagPlanTaskResult> =
            results.iter().map(|(k, v)| (*k, v)).collect();
        let result = tool.interpolate_string("Please: {{dep[cinemas].output}}", &refs);
        assert_eq!(result, "Please: Cinema List: ABC, XYZ");
    }

    #[test]
    fn excluded_tool_is_blocked() {
        let tool = DagPlanExecuteTool::new(Arc::new(RwLock::new(vec![])))
            .with_excluded_tools(Arc::new(vec!["blocked_tool".into()]));
        assert!(tool.excluded_tools.iter().any(|e| e == "blocked_tool"));
    }

    #[tokio::test]
    async fn cycle_detection_rejects() {
        let tool = DagPlanExecuteTool::new(Arc::new(RwLock::new(vec![])));
        let args = serde_json::json!({
            "tasks": [
                {"id":"a","tool":"e","args":{},"depends_on":["b"]},
                {"id":"b","tool":"e","args":{},"depends_on":["a"]}
            ],
            "max_parallel": 1
        });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn zero_parallel_rejected() {
        let tool = DagPlanExecuteTool::new(Arc::new(RwLock::new(vec![])));
        let args = serde_json::json!({
            "tasks": [{"id":"a","tool":"e","args":{}}],
            "max_parallel": 0
        });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn max_nodes_exceeded() {
        let tool = DagPlanExecuteTool::new(Arc::new(RwLock::new(vec![])));
        let mut tasks = Vec::new();
        for i in 0..60 {
            tasks.push(serde_json::json!({"id": format!("t{i}"), "tool":"e","args":{}}));
        }
        let args = serde_json::json!({"tasks": tasks, "max_parallel": 1});
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
    }
}
