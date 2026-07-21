use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::schema::PipelineConfig;

/// Errors specific to pipeline execution.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
pub enum PipelineError {
    #[error("Unknown tool '{0}' is not on the allowed list")]
    UnknownTool(String),
    #[error("Pipeline exceeds maximum of {0} steps")]
    TooManySteps(usize),
    #[error("Invalid template reference: {0}")]
    InvalidTemplate(String),
    #[error("Step {index} ({tool}) failed: {message}")]
    StepFailed {
        index: usize,
        tool: String,
        message: String,
    },
}

/// A single step in a pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    pub tool: String,
    pub args: serde_json::Value,
}

/// The pipeline request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRequest {
    pub steps: Vec<PipelineStep>,
    #[serde(default)]
    pub parallel: bool,
    /// What to include in the tool output. Defaults to every step's result.
    #[serde(default)]
    pub result: PipelineResultMode,
}

/// Controls what `execute_pipeline` returns to the caller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineResultMode {
    /// Return every step's result as a JSON array (default; backward compatible).
    #[default]
    All,
    /// Return only the final step's raw output. Use when earlier steps produce
    /// large intermediate blobs (e.g. base64) that must not flow back into the
    /// model context.
    Last,
}

/// Result of a single pipeline step.
#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub index: usize,
    pub tool: String,
    pub success: bool,
    pub output: String,
}

/// The execute_pipeline tool that runs multi-step tool chains.
pub struct PipelineTool {
    config: PipelineConfig,
    tools: Vec<Arc<dyn Tool>>,
    allowed_set: HashSet<String>,
}

impl PipelineTool {
    pub const NAME: &'static str = "execute_pipeline";

    pub fn new(config: PipelineConfig, tools: Vec<Arc<dyn Tool>>) -> Self {
        let allowed_set: HashSet<String> = config.allowed_tools.iter().cloned().collect();
        Self {
            config,
            tools,
            allowed_set,
        }
    }

    /// Find a tool by name in the registry.
    fn find_tool(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    /// Validate the pipeline request before execution.
    fn validate(&self, request: &PipelineRequest) -> std::result::Result<(), PipelineError> {
        if request.steps.len() > self.config.max_steps {
            return Err(PipelineError::TooManySteps(self.config.max_steps));
        }

        // Check all tools are on the allowlist before executing any.
        for step in &request.steps {
            if !self.allowed_set.contains(&step.tool) {
                return Err(PipelineError::UnknownTool(step.tool.clone()));
            }
        }

        Ok(())
    }

    /// Execute steps sequentially, interpolating results.
    async fn execute_sequential(
        &self,
        steps: &[PipelineStep],
    ) -> std::result::Result<Vec<StepResult>, PipelineError> {
        let mut results: Vec<StepResult> = Vec::with_capacity(steps.len());

        for (i, step) in steps.iter().enumerate() {
            let tool = self
                .find_tool(&step.tool)
                .ok_or_else(|| PipelineError::UnknownTool(step.tool.clone()))?;

            // Interpolate previous step results into args.
            let interpolated_args = interpolate_args(&step.args, &results);

            let tool_result =
                tool.execute(interpolated_args)
                    .await
                    .map_err(|e| PipelineError::StepFailed {
                        index: i,
                        tool: step.tool.clone(),
                        message: e.to_string(),
                    })?;

            if !tool_result.success {
                return Err(PipelineError::StepFailed {
                    index: i,
                    tool: step.tool.clone(),
                    message: tool_result
                        .error
                        .unwrap_or_else(|| tool_result.output.clone().into_string()),
                });
            }

            results.push(StepResult {
                index: i,
                tool: step.tool.clone(),
                success: true,
                output: tool_result.output.into_string(),
            });
        }

        Ok(results)
    }

    /// Execute independent steps in parallel (no interpolation between them).
    async fn execute_parallel(
        &self,
        steps: &[PipelineStep],
    ) -> std::result::Result<Vec<StepResult>, PipelineError> {
        use tokio::task::JoinSet;

        let mut join_set = JoinSet::new();

        for (i, step) in steps.iter().enumerate() {
            let tool = self
                .find_tool(&step.tool)
                .ok_or_else(|| PipelineError::UnknownTool(step.tool.clone()))?;

            // Clone what we need for the spawned task.
            let tool_name = step.tool.clone();
            let args = step.args.clone();

            // We need a reference that lives long enough — use Arc.
            let tool_arc = self.tools.iter().find(|t| t.name() == tool.name()).cloned();

            if let Some(tool_arc) = tool_arc {
                join_set.spawn(async move {
                    let result = tool_arc.execute(args).await;
                    (i, tool_name, result)
                });
            }
        }

        let mut results: Vec<StepResult> = Vec::with_capacity(steps.len());

        while let Some(join_result) = join_set.join_next().await {
            let (index, tool_name, tool_result) =
                join_result.map_err(|e| PipelineError::StepFailed {
                    index: 0,
                    tool: "unknown".to_string(),
                    message: format!("Task join error: {e}"),
                })?;

            let tool_result = tool_result.map_err(|e| PipelineError::StepFailed {
                index,
                tool: tool_name.clone(),
                message: e.to_string(),
            })?;

            if !tool_result.success {
                return Err(PipelineError::StepFailed {
                    index,
                    tool: tool_name,
                    message: tool_result
                        .error
                        .unwrap_or_else(|| tool_result.output.clone().into_string()),
                });
            }

            results.push(StepResult {
                index,
                tool: tool_name,
                success: true,
                output: tool_result.output.into_string(),
            });
        }

        // Sort by index for deterministic output.
        results.sort_by_key(|r| r.index);
        Ok(results)
    }
}

#[async_trait]
impl Tool for PipelineTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Execute a multi-step tool pipeline in a single call. Steps run sequentially by default \
         with result interpolation (use {{step[N].result}} to reference prior outputs), \
         or in parallel when 'parallel: true' is set. Set 'result: \"last\"' to return only the \
         final step's output (recommended when an earlier step yields a large blob, e.g. base64, \
         that should not flow back into the context); the default 'all' returns every step's result."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "description": "Ordered list of tool invocations",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tool": {
                                "type": "string",
                                "description": "Name of the tool to invoke"
                            },
                            "args": {
                                "type": "object",
                                "description": "Arguments to pass to the tool. Use {{step[N].result}} to interpolate prior step outputs."
                            }
                        },
                        "required": ["tool", "args"]
                    }
                },
                "parallel": {
                    "type": "boolean",
                    "description": "Run steps in parallel (no interpolation). Default: false",
                    "default": false
                },
                "result": {
                    "type": "string",
                    "enum": ["all", "last"],
                    "description": "What to return: 'all' (default) = every step's result as JSON; 'last' = only the final step's raw output. Use 'last' to keep large intermediate blobs (e.g. base64) out of the context.",
                    "default": "all"
                }
            },
            "required": ["steps"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let request: PipelineRequest = serde_json::from_value(args).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "pipeline: invalid request"
            );
            anyhow::Error::msg(format!("Invalid pipeline request: {e}"))
        })?;

        // Validate before execution.
        if let Err(e) = self.validate(&request) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(e.to_string()),
            });
        }

        let results = if request.parallel {
            self.execute_parallel(&request.steps).await
        } else {
            self.execute_sequential(&request.steps).await
        };

        match results {
            Ok(step_results) => {
                let output = match request.result {
                    PipelineResultMode::Last => step_results
                        .last()
                        .map(|s| s.output.clone())
                        .unwrap_or_default(),
                    PipelineResultMode::All => serde_json::to_string_pretty(&step_results)
                        .unwrap_or_else(|_| "Pipeline completed".to_string()),
                };
                Ok(ToolResult {
                    success: true,
                    output: output.into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(e.to_string()),
            }),
        }
    }
}

/// Interpolate `{{step[N].result}}` references in tool arguments.
/// Single-pass replacement: values containing `{{` after substitution are stripped
/// to prevent injection.
pub fn interpolate_args(
    args: &serde_json::Value,
    prior_results: &[StepResult],
) -> serde_json::Value {
    match args {
        serde_json::Value::String(s) => {
            let interpolated = interpolate_string(s, prior_results);
            serde_json::Value::String(interpolated)
        }
        serde_json::Value::Object(map) => {
            let new_map: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), interpolate_args(v, prior_results)))
                .collect();
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            let new_arr: Vec<serde_json::Value> = arr
                .iter()
                .map(|v| interpolate_args(v, prior_results))
                .collect();
            serde_json::Value::Array(new_arr)
        }
        other => other.clone(),
    }
}

/// Perform single-pass interpolation of `{{step[N].result}}` in a string.
fn interpolate_string(s: &str, prior_results: &[StepResult]) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if c == '{'
            && let Some(&(_, '{')) = chars.peek()
        {
            // Found `{{` — try to match `{{step[N].result}}`
            let rest = &s[i..];
            if let Some(end) = find_template_end(rest) {
                let template = &rest[2..end]; // strip {{ and }}
                if let Some(value) = resolve_template(template, prior_results) {
                    // Strip any `{{` in the resolved value to prevent injection.
                    result.push_str(&value.replace("{{", ""));
                    // Skip past the closing `}}`
                    let skip_to = i + end + 2;
                    while chars.peek().is_some_and(|&(idx, _)| idx < skip_to) {
                        chars.next();
                    }
                    continue;
                }
            }
        }
        result.push(c);
    }

    result
}

/// Find the position of `}}` in a string starting with `{{`.
fn find_template_end(s: &str) -> Option<usize> {
    s[2..].find("}}").map(|pos| pos + 2)
}

/// Resolve a template reference like `step[0].result`.
fn resolve_template(template: &str, prior_results: &[StepResult]) -> Option<String> {
    let template = template.trim();
    if !template.starts_with("step[") || !template.ends_with(".result") {
        return None;
    }

    let bracket_end = template.find(']')?;
    let index_str = &template[5..bracket_end];
    let index: usize = index_str.parse().ok()?;

    prior_results
        .iter()
        .find(|r| r.index == index)
        .map(|r| r.output.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Interpolation ──────────────────────────────────────

    #[test]
    fn interpolate_simple_reference() {
        let results = vec![StepResult {
            index: 0,
            tool: "web_search".to_string(),
            success: true,
            output: "search results here".to_string(),
        }];

        let args = serde_json::json!({"text": "Summarize: {{step[0].result}}"});
        let interpolated = interpolate_args(&args, &results);
        assert_eq!(
            interpolated["text"].as_str().unwrap(),
            "Summarize: search results here"
        );
    }

    #[test]
    fn interpolate_multiple_references() {
        let results = vec![
            StepResult {
                index: 0,
                tool: "a".to_string(),
                success: true,
                output: "first".to_string(),
            },
            StepResult {
                index: 1,
                tool: "b".to_string(),
                success: true,
                output: "second".to_string(),
            },
        ];

        let args = serde_json::json!({"text": "{{step[0].result}} and {{step[1].result}}"});
        let interpolated = interpolate_args(&args, &results);
        assert_eq!(interpolated["text"].as_str().unwrap(), "first and second");
    }

    #[test]
    fn interpolate_no_match_passes_through() {
        let args = serde_json::json!({"text": "no templates here"});
        let interpolated = interpolate_args(&args, &[]);
        assert_eq!(interpolated["text"].as_str().unwrap(), "no templates here");
    }

    #[test]
    fn interpolate_invalid_index_passes_through() {
        let args = serde_json::json!({"text": "{{step[99].result}}"});
        let interpolated = interpolate_args(&args, &[]);
        // Invalid reference is left as-is.
        assert_eq!(
            interpolated["text"].as_str().unwrap(),
            "{{step[99].result}}"
        );
    }

    #[test]
    fn interpolate_strips_injection() {
        let results = vec![StepResult {
            index: 0,
            tool: "a".to_string(),
            success: true,
            output: "value with {{step[1].result}} injection".to_string(),
        }];

        let args = serde_json::json!({"text": "{{step[0].result}}"});
        let interpolated = interpolate_args(&args, &results);
        // The `{{` in the resolved value should be stripped.
        let text = interpolated["text"].as_str().unwrap();
        assert!(!text.contains("{{"));
        assert!(text.contains("step[1].result}} injection"));
    }

    #[test]
    fn interpolate_nested_objects() {
        let results = vec![StepResult {
            index: 0,
            tool: "a".to_string(),
            success: true,
            output: "data".to_string(),
        }];

        let args = serde_json::json!({
            "outer": {
                "inner": "prefix {{step[0].result}} suffix"
            }
        });
        let interpolated = interpolate_args(&args, &results);
        assert_eq!(
            interpolated["outer"]["inner"].as_str().unwrap(),
            "prefix data suffix"
        );
    }

    #[test]
    fn interpolate_array_values() {
        let results = vec![StepResult {
            index: 0,
            tool: "a".to_string(),
            success: true,
            output: "item".to_string(),
        }];

        let args = serde_json::json!(["{{step[0].result}}", "static"]);
        let interpolated = interpolate_args(&args, &results);
        assert_eq!(interpolated[0].as_str().unwrap(), "item");
        assert_eq!(interpolated[1].as_str().unwrap(), "static");
    }

    // ── Validation ─────────────────────────────────────────

    #[test]
    fn validate_too_many_steps() {
        let config = PipelineConfig {
            enabled: true,
            max_steps: 2,
            allowed_tools: vec!["shell".to_string()],
        };
        let tool = PipelineTool::new(config, vec![]);

        let request = PipelineRequest {
            steps: vec![
                PipelineStep {
                    tool: "shell".into(),
                    args: serde_json::json!({}),
                },
                PipelineStep {
                    tool: "shell".into(),
                    args: serde_json::json!({}),
                },
                PipelineStep {
                    tool: "shell".into(),
                    args: serde_json::json!({}),
                },
            ],
            parallel: false,
            result: PipelineResultMode::default(),
        };

        let err = tool.validate(&request).unwrap_err();
        assert!(matches!(err, PipelineError::TooManySteps(2)));
    }

    #[test]
    fn validate_unknown_tool() {
        let config = PipelineConfig {
            enabled: true,
            max_steps: 20,
            allowed_tools: vec!["shell".to_string()],
        };
        let tool = PipelineTool::new(config, vec![]);

        let request = PipelineRequest {
            steps: vec![PipelineStep {
                tool: "forbidden_tool".into(),
                args: serde_json::json!({}),
            }],
            parallel: false,
            result: PipelineResultMode::default(),
        };

        let err = tool.validate(&request).unwrap_err();
        assert!(matches!(err, PipelineError::UnknownTool(_)));
    }

    #[test]
    fn validate_valid_request() {
        let config = PipelineConfig {
            enabled: true,
            max_steps: 20,
            allowed_tools: vec!["shell".to_string(), "file_read".to_string()],
        };
        let tool = PipelineTool::new(config, vec![]);

        let request = PipelineRequest {
            steps: vec![
                PipelineStep {
                    tool: "shell".into(),
                    args: serde_json::json!({}),
                },
                PipelineStep {
                    tool: "file_read".into(),
                    args: serde_json::json!({}),
                },
            ],
            parallel: false,
            result: PipelineResultMode::default(),
        };

        assert!(tool.validate(&request).is_ok());
    }

    #[test]
    fn validate_empty_pipeline() {
        let config = PipelineConfig {
            enabled: true,
            max_steps: 20,
            allowed_tools: vec![],
        };
        let tool = PipelineTool::new(config, vec![]);

        let request = PipelineRequest {
            steps: vec![],
            parallel: false,
            result: PipelineResultMode::default(),
        };

        assert!(tool.validate(&request).is_ok());
    }

    // ── Template resolution ────────────────────────────────

    #[test]
    fn resolve_valid_template() {
        let results = vec![StepResult {
            index: 0,
            tool: "a".to_string(),
            success: true,
            output: "hello".to_string(),
        }];
        assert_eq!(
            resolve_template("step[0].result", &results),
            Some("hello".to_string())
        );
    }

    #[test]
    fn resolve_invalid_template_format() {
        assert_eq!(resolve_template("invalid", &[]), None);
        assert_eq!(resolve_template("step.result", &[]), None);
        assert_eq!(resolve_template("step[abc].result", &[]), None);
    }

    #[test]
    fn resolve_out_of_range_index() {
        assert_eq!(resolve_template("step[5].result", &[]), None);
    }

    // ── Result mode ────────────────────────────────────────

    struct EchoTool {
        name: String,
        output: String,
    }

    zeroclaw_api::mock_tool_attribution!(EchoTool);

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: self.output.clone().into(),
                error: None,
            })
        }
    }

    fn echo_pipeline() -> PipelineTool {
        let config = PipelineConfig {
            enabled: true,
            max_steps: 20,
            allowed_tools: vec!["a".to_string(), "b".to_string()],
        };
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(EchoTool {
                name: "a".into(),
                output: "FIRST_BIG_BLOB".into(),
            }),
            Arc::new(EchoTool {
                name: "b".into(),
                output: "final answer".into(),
            }),
        ];
        PipelineTool::new(config, tools)
    }

    #[tokio::test]
    async fn result_last_returns_only_final_output() {
        let args = serde_json::json!({
            "steps": [
                {"tool": "a", "args": {}},
                {"tool": "b", "args": {}}
            ],
            "result": "last"
        });
        let res = echo_pipeline().execute(args).await.unwrap();
        assert!(res.success);
        assert_eq!(res.output, "final answer");
        assert!(!res.output.contains("FIRST_BIG_BLOB"));
    }

    #[tokio::test]
    async fn result_all_is_default_and_includes_every_step() {
        let args = serde_json::json!({
            "steps": [
                {"tool": "a", "args": {}},
                {"tool": "b", "args": {}}
            ]
        });
        let res = echo_pipeline().execute(args).await.unwrap();
        assert!(res.success);
        assert!(res.output.contains("FIRST_BIG_BLOB"));
        assert!(res.output.contains("final answer"));
    }
}
