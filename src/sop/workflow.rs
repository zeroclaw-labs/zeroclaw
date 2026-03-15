//! Workflow DAG engine for structured multi-step agent workflows.
//!
//! Extends the SOP system with fan-out, sequential, conditional, and loop
//! execution patterns. Each [`WorkflowStep`] is a node in a directed acyclic
//! graph that the [`WorkflowExecutor`] traverses at runtime.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, warn};

// ── Step types ──────────────────────────────────────────────────

/// A single node in a workflow DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowStep {
    /// Execute a single agent prompt.
    Task {
        name: String,
        prompt: String,
        #[serde(default)]
        tools: Option<Vec<String>>,
        #[serde(default)]
        model: Option<String>,
    },
    /// Execute steps one after another, passing output forward.
    Sequential {
        name: String,
        steps: Vec<WorkflowStep>,
    },
    /// Execute steps in parallel, collect all results.
    FanOut {
        name: String,
        steps: Vec<WorkflowStep>,
    },
    /// Execute one branch based on a condition variable.
    Conditional {
        name: String,
        /// Variable name to evaluate (from a previous step's output).
        condition: String,
        /// Branch to execute when the condition is truthy (non-empty string).
        then_step: Box<WorkflowStep>,
        /// Branch to execute otherwise.
        #[serde(default)]
        else_step: Option<Box<WorkflowStep>>,
    },
    /// Repeat a step for each item in a list variable.
    Loop {
        name: String,
        /// Variable name containing a newline-separated list of items.
        items_var: String,
        /// Step template to execute per item. The current item is injected
        /// as `{item}` in the step's prompt context.
        body: Box<WorkflowStep>,
    },
}

impl WorkflowStep {
    /// Return the step's name regardless of variant.
    pub fn name(&self) -> &str {
        match self {
            Self::Task { name, .. }
            | Self::Sequential { name, .. }
            | Self::FanOut { name, .. }
            | Self::Conditional { name, .. }
            | Self::Loop { name, .. } => name,
        }
    }
}

// ── Result types ────────────────────────────────────────────────

/// Outcome of a complete workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub name: String,
    pub status: WorkflowStatus,
    pub outputs: HashMap<String, String>,
    pub duration_ms: u64,
}

/// Terminal status of a workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowStatus {
    Completed,
    Failed {
        step: String,
        error: String,
    },
    PartiallyCompleted {
        completed: Vec<String>,
        failed: Vec<String>,
    },
}

// ── Step handler ────────────────────────────────────────────────

/// Trait for executing a single task step. Implementations connect to the
/// agent runtime or provide test stubs.
#[async_trait::async_trait]
pub trait StepHandler: Send + Sync {
    /// Execute a task step and return its output string.
    async fn execute(
        &self,
        name: &str,
        prompt: &str,
        tools: Option<&[String]>,
        model: Option<&str>,
        context: &HashMap<String, String>,
    ) -> Result<String, String>;
}

// ── Executor ────────────────────────────────────────────────────

/// Traverses a [`WorkflowStep`] DAG and executes each node according to
/// its type (sequential, fan-out, conditional, loop).
pub struct WorkflowExecutor<H: StepHandler> {
    handler: Arc<H>,
}

impl<H: StepHandler + 'static> WorkflowExecutor<H> {
    pub fn new(handler: H) -> Self {
        Self {
            handler: Arc::new(handler),
        }
    }

    /// Execute the top-level workflow step and return the result.
    pub async fn run(&self, step: &WorkflowStep) -> WorkflowResult {
        let start = Instant::now();
        let vars = Arc::new(Mutex::new(HashMap::<String, String>::new()));

        let status = self.execute_step(step, &vars).await;
        let outputs = match Arc::try_unwrap(vars) {
            Ok(mutex) => mutex.into_inner(),
            Err(arc) => arc.lock().await.clone(),
        };

        WorkflowResult {
            name: step.name().to_string(),
            status,
            outputs,
            duration_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }

    /// Recursively execute a single step node.
    fn execute_step<'a>(
        &'a self,
        step: &'a WorkflowStep,
        vars: &'a Arc<Mutex<HashMap<String, String>>>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = WorkflowStatus> + Send + 'a>> {
        Box::pin(async move {
            match step {
                WorkflowStep::Task {
                    name,
                    prompt,
                    tools,
                    model,
                } => {
                    debug!(step = %name, "executing task step");
                    let ctx = vars.lock().await.clone();
                    let tool_refs = tools.as_deref();
                    match self
                        .handler
                        .execute(name, prompt, tool_refs, model.as_deref(), &ctx)
                        .await
                    {
                        Ok(output) => {
                            vars.lock().await.insert(name.clone(), output);
                            WorkflowStatus::Completed
                        }
                        Err(e) => {
                            warn!(step = %name, error = %e, "task step failed");
                            WorkflowStatus::Failed {
                                step: name.clone(),
                                error: e,
                            }
                        }
                    }
                }

                WorkflowStep::Sequential { name, steps } => {
                    debug!(step = %name, count = steps.len(), "executing sequential block");
                    for child in steps {
                        let status = self.execute_step(child, vars).await;
                        if status != WorkflowStatus::Completed {
                            return status;
                        }
                    }
                    WorkflowStatus::Completed
                }

                WorkflowStep::FanOut { name, steps } => {
                    debug!(step = %name, count = steps.len(), "executing fan-out block");
                    let mut handles = Vec::with_capacity(steps.len());

                    for child in steps {
                        let handler = Arc::clone(&self.handler);
                        let child = child.clone();
                        let vars = Arc::clone(vars);
                        handles.push(tokio::spawn(async move {
                            let executor = WorkflowExecutor {
                                handler: Arc::clone(&handler),
                            };
                            let status = executor.execute_step(&child, &vars).await;
                            (child.name().to_string(), status)
                        }));
                    }

                    let mut completed = Vec::new();
                    let mut failed = Vec::new();
                    for handle in handles {
                        match handle.await {
                            Ok((child_name, WorkflowStatus::Completed)) => {
                                completed.push(child_name);
                            }
                            Ok((child_name, _)) => {
                                failed.push(child_name);
                            }
                            Err(e) => {
                                failed.push(format!("<join-error: {e}>"));
                            }
                        }
                    }

                    if failed.is_empty() {
                        WorkflowStatus::Completed
                    } else {
                        WorkflowStatus::PartiallyCompleted { completed, failed }
                    }
                }

                WorkflowStep::Conditional {
                    name,
                    condition,
                    then_step,
                    else_step,
                } => {
                    debug!(step = %name, condition = %condition, "evaluating conditional");
                    let ctx = vars.lock().await;
                    let value = ctx.get(condition).cloned().unwrap_or_default();
                    drop(ctx);

                    let truthy =
                        !value.is_empty() && value != "false" && value != "0" && value != "null";

                    if truthy {
                        self.execute_step(then_step, vars).await
                    } else if let Some(else_s) = else_step {
                        self.execute_step(else_s, vars).await
                    } else {
                        WorkflowStatus::Completed
                    }
                }

                WorkflowStep::Loop {
                    name,
                    items_var,
                    body,
                } => {
                    debug!(step = %name, items_var = %items_var, "executing loop");
                    let items_raw = {
                        let ctx = vars.lock().await;
                        ctx.get(items_var).cloned().unwrap_or_default()
                    };

                    let items: Vec<String> = items_raw
                        .lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect();

                    if items.is_empty() {
                        debug!(step = %name, "loop has zero items — skipping");
                        return WorkflowStatus::Completed;
                    }

                    for (i, item) in items.iter().enumerate() {
                        let iter_name = format!("{name}__item_{i}");
                        {
                            let mut guard = vars.lock().await;
                            guard.insert("item".to_string(), item.clone());
                            guard.insert("item_index".to_string(), i.to_string());
                        }

                        let status = self.execute_step(body, vars).await;

                        // Store per-iteration output under an indexed key.
                        {
                            let mut guard = vars.lock().await;
                            if let Some(output) = guard.get(body.name()).cloned() {
                                guard.insert(iter_name, output);
                            }
                        }

                        if status != WorkflowStatus::Completed {
                            return status;
                        }
                    }
                    WorkflowStatus::Completed
                }
            }
        })
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A simple test handler that echoes the prompt with context vars interpolated.
    struct EchoHandler;

    #[async_trait::async_trait]
    impl StepHandler for EchoHandler {
        async fn execute(
            &self,
            name: &str,
            prompt: &str,
            _tools: Option<&[String]>,
            _model: Option<&str>,
            context: &HashMap<String, String>,
        ) -> Result<String, String> {
            let mut output = prompt.to_string();
            for (k, v) in context {
                output = output.replace(&format!("{{{k}}}"), v);
            }
            Ok(format!("[{name}] {output}"))
        }
    }

    /// A handler that fails on steps whose name starts with "fail_".
    struct FailingHandler;

    #[async_trait::async_trait]
    impl StepHandler for FailingHandler {
        async fn execute(
            &self,
            name: &str,
            prompt: &str,
            _tools: Option<&[String]>,
            _model: Option<&str>,
            context: &HashMap<String, String>,
        ) -> Result<String, String> {
            if name.starts_with("fail_") {
                return Err(format!("intentional failure in {name}"));
            }
            let mut output = prompt.to_string();
            for (k, v) in context {
                output = output.replace(&format!("{{{k}}}"), v);
            }
            Ok(format!("[{name}] {output}"))
        }
    }

    #[tokio::test]
    async fn sequential_passes_outputs_forward() {
        let executor = WorkflowExecutor::new(EchoHandler);

        let step = WorkflowStep::Sequential {
            name: "seq".into(),
            steps: vec![
                WorkflowStep::Task {
                    name: "step_a".into(),
                    prompt: "hello".into(),
                    tools: None,
                    model: None,
                },
                WorkflowStep::Task {
                    name: "step_b".into(),
                    prompt: "got {step_a}".into(),
                    tools: None,
                    model: None,
                },
                WorkflowStep::Task {
                    name: "step_c".into(),
                    prompt: "got {step_b}".into(),
                    tools: None,
                    model: None,
                },
            ],
        };

        let result = executor.run(&step).await;
        assert_eq!(result.status, WorkflowStatus::Completed);
        assert_eq!(result.outputs.len(), 3);
        assert_eq!(result.outputs["step_a"], "[step_a] hello");
        assert!(result.outputs["step_b"].contains("[step_a] hello"));
        assert!(result.outputs["step_c"].contains("[step_b]"));
    }

    #[tokio::test]
    async fn fanout_runs_all_branches() {
        let executor = WorkflowExecutor::new(EchoHandler);

        let step = WorkflowStep::FanOut {
            name: "parallel".into(),
            steps: vec![
                WorkflowStep::Task {
                    name: "branch_1".into(),
                    prompt: "one".into(),
                    tools: None,
                    model: None,
                },
                WorkflowStep::Task {
                    name: "branch_2".into(),
                    prompt: "two".into(),
                    tools: None,
                    model: None,
                },
                WorkflowStep::Task {
                    name: "branch_3".into(),
                    prompt: "three".into(),
                    tools: None,
                    model: None,
                },
            ],
        };

        let result = executor.run(&step).await;
        assert_eq!(result.status, WorkflowStatus::Completed);
        assert!(result.outputs.contains_key("branch_1"));
        assert!(result.outputs.contains_key("branch_2"));
        assert!(result.outputs.contains_key("branch_3"));
    }

    #[tokio::test]
    async fn conditional_truthy_branch() {
        let executor = WorkflowExecutor::new(EchoHandler);

        let step = WorkflowStep::Sequential {
            name: "cond_test".into(),
            steps: vec![
                WorkflowStep::Task {
                    name: "setup".into(),
                    prompt: "yes".into(),
                    tools: None,
                    model: None,
                },
                WorkflowStep::Conditional {
                    name: "branch".into(),
                    condition: "setup".into(),
                    then_step: Box::new(WorkflowStep::Task {
                        name: "then_result".into(),
                        prompt: "took then".into(),
                        tools: None,
                        model: None,
                    }),
                    else_step: Some(Box::new(WorkflowStep::Task {
                        name: "else_result".into(),
                        prompt: "took else".into(),
                        tools: None,
                        model: None,
                    })),
                },
            ],
        };

        let result = executor.run(&step).await;
        assert_eq!(result.status, WorkflowStatus::Completed);
        assert!(result.outputs.contains_key("then_result"));
        assert!(!result.outputs.contains_key("else_result"));
    }

    #[tokio::test]
    async fn conditional_falsy_branch() {
        let executor = WorkflowExecutor::new(EchoHandler);

        let step = WorkflowStep::Sequential {
            name: "cond_test".into(),
            steps: vec![
                // "setup" is not set — condition is empty → falsy
                WorkflowStep::Conditional {
                    name: "branch".into(),
                    condition: "setup".into(),
                    then_step: Box::new(WorkflowStep::Task {
                        name: "then_result".into(),
                        prompt: "took then".into(),
                        tools: None,
                        model: None,
                    }),
                    else_step: Some(Box::new(WorkflowStep::Task {
                        name: "else_result".into(),
                        prompt: "took else".into(),
                        tools: None,
                        model: None,
                    })),
                },
            ],
        };

        let result = executor.run(&step).await;
        assert_eq!(result.status, WorkflowStatus::Completed);
        assert!(!result.outputs.contains_key("then_result"));
        assert!(result.outputs.contains_key("else_result"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_iterates_items() {
        let executor = WorkflowExecutor::new(EchoHandler);

        // Pre-populate the items variable directly via the executor, using a
        // simple Sequential: first a task that produces the list, then the loop.
        let step = WorkflowStep::Sequential {
            name: "loop_test".into(),
            steps: vec![
                WorkflowStep::Task {
                    name: "items".into(),
                    prompt: "alpha\nbeta\ngamma".into(),
                    tools: None,
                    model: None,
                },
                WorkflowStep::Loop {
                    name: "foreach".into(),
                    items_var: "items".into(),
                    body: Box::new(WorkflowStep::Task {
                        name: "process".into(),
                        prompt: "processing {item}".into(),
                        tools: None,
                        model: None,
                    }),
                },
            ],
        };

        let result = tokio::time::timeout(std::time::Duration::from_secs(10), executor.run(&step))
            .await
            .expect("loop test timed out — likely deadlock");
        assert_eq!(result.status, WorkflowStatus::Completed);
        // Should have per-iteration outputs.
        assert!(result.outputs.contains_key("foreach__item_0"));
        assert!(result.outputs.contains_key("foreach__item_1"));
        assert!(result.outputs.contains_key("foreach__item_2"));
    }

    #[tokio::test]
    async fn fanout_partial_failure() {
        let executor = WorkflowExecutor::new(FailingHandler);

        let step = WorkflowStep::FanOut {
            name: "partial".into(),
            steps: vec![
                WorkflowStep::Task {
                    name: "ok_1".into(),
                    prompt: "fine".into(),
                    tools: None,
                    model: None,
                },
                WorkflowStep::Task {
                    name: "fail_bad".into(),
                    prompt: "boom".into(),
                    tools: None,
                    model: None,
                },
                WorkflowStep::Task {
                    name: "ok_2".into(),
                    prompt: "also fine".into(),
                    tools: None,
                    model: None,
                },
            ],
        };

        let result = executor.run(&step).await;
        match &result.status {
            WorkflowStatus::PartiallyCompleted { completed, failed } => {
                assert!(completed.contains(&"ok_1".to_string()));
                assert!(completed.contains(&"ok_2".to_string()));
                assert!(failed.contains(&"fail_bad".to_string()));
            }
            other => panic!("expected PartiallyCompleted, got {other:?}"),
        }
    }
}
