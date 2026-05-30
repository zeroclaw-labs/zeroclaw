//! Streaming tool executor: dispatches tools as they arrive mid-stream.
//!
//! When the LLM streams a response containing tool calls, each complete
//! tool_call event triggers immediate execution rather than waiting for
//! the full response. Concurrency-safe tools run in parallel; non-safe
//! tools form exclusive barriers.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::observability::Observer;
use crate::tools::Tool;
use daemonclaw_providers::ToolCall;

use super::tool_execution::{ToolExecutionOutcome, execute_one_tool};

struct TrackedTool {
    call: ToolCall,
    is_safe: bool,
    handle: Option<JoinHandle<anyhow::Result<ToolExecutionOutcome>>>,
    result: Option<ToolExecutionOutcome>,
}

/// Executes tools as they stream in, respecting concurrency safety.
pub struct StreamingToolExecutor {
    tools: Vec<TrackedTool>,
    tools_registry: Arc<Vec<Box<dyn Tool>>>,
    observer: Arc<dyn Observer>,
    cancellation_token: Option<CancellationToken>,
    receipt_generator: Option<Arc<super::tool_receipts::ReceiptGenerator>>,
    executing_count: Arc<Mutex<usize>>,
    has_exclusive: Arc<Mutex<bool>>,
}

impl StreamingToolExecutor {
    pub fn new(
        tools_registry: Arc<Vec<Box<dyn Tool>>>,
        observer: Arc<dyn Observer>,
        cancellation_token: Option<CancellationToken>,
        receipt_generator: Option<Arc<super::tool_receipts::ReceiptGenerator>>,
    ) -> Self {
        Self {
            tools: Vec::new(),
            tools_registry,
            observer,
            cancellation_token,
            receipt_generator,
            executing_count: Arc::new(Mutex::new(0)),
            has_exclusive: Arc::new(Mutex::new(false)),
        }
    }

    /// Add a tool call received from the stream. Starts execution immediately
    /// if concurrency conditions allow, otherwise queues for later.
    pub fn add_tool(&mut self, call: ToolCall) {
        let is_safe = self.lookup_safety(&call);
        self.tools.push(TrackedTool {
            call,
            is_safe,
            handle: None,
            result: None,
        });
        self.try_dispatch_queued();
    }

    /// After the stream ends, wait for all in-flight tools and return results
    /// in the order they were received.
    pub async fn finish(mut self) -> Vec<(ToolCall, ToolExecutionOutcome)> {
        // Dispatch any remaining queued tools
        self.try_dispatch_queued();

        // Wait for all handles
        for tracked in &mut self.tools {
            if let Some(handle) = tracked.handle.take() {
                match handle.await {
                    Ok(Ok(outcome)) => tracked.result = Some(outcome),
                    Ok(Err(e)) => {
                        tracked.result = Some(ToolExecutionOutcome {
                            output: format!("Error: {e}"),
                            success: false,
                            error_reason: Some(e.to_string()),
                            duration: Duration::ZERO,
                            receipt: None,
                        });
                    }
                    Err(e) => {
                        tracked.result = Some(ToolExecutionOutcome {
                            output: format!("Task panicked: {e}"),
                            success: false,
                            error_reason: Some("task panicked".to_string()),
                            duration: Duration::ZERO,
                            receipt: None,
                        });
                    }
                }
            }
        }

        self.tools
            .into_iter()
            .filter_map(|t| {
                t.result
                    .map(|r| (t.call, r))
            })
            .collect()
    }

    /// Returns true if any tools have been added.
    pub fn has_tools(&self) -> bool {
        !self.tools.is_empty()
    }

    fn lookup_safety(&self, call: &ToolCall) -> bool {
        if call.name == "tool_search" {
            return false;
        }
        let args: serde_json::Value = serde_json::from_str(&call.arguments)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        self.tools_registry
            .iter()
            .find(|t| t.name() == call.name)
            .map(|t| t.is_concurrency_safe(&args))
            .unwrap_or(false)
    }

    fn try_dispatch_queued(&mut self) {
        for i in 0..self.tools.len() {
            if self.tools[i].handle.is_some() || self.tools[i].result.is_some() {
                continue;
            }
            let can_run = if self.tools[i].is_safe {
                // Safe tools can run if no exclusive tool is running
                !*self.has_exclusive.blocking_lock()
            } else {
                // Exclusive tools can run only if nothing else is executing
                *self.executing_count.blocking_lock() == 0
            };

            if !can_run {
                if !self.tools[i].is_safe {
                    break;
                }
                continue;
            }

            let registry = Arc::clone(&self.tools_registry);
            let observer = Arc::clone(&self.observer);
            let cancel = self.cancellation_token.clone();
            let receipt_gen = self.receipt_generator.clone();
            let exec_count = Arc::clone(&self.executing_count);
            let has_excl = Arc::clone(&self.has_exclusive);
            let is_safe = self.tools[i].is_safe;

            let call_name = self.tools[i].call.name.clone();
            let call_args: serde_json::Value =
                serde_json::from_str(&self.tools[i].call.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            let handle = tokio::spawn(async move {
                {
                    let mut count = exec_count.lock().await;
                    *count += 1;
                    if !is_safe {
                        *has_excl.lock().await = true;
                    }
                }
                let result = execute_one_tool(
                    &call_name,
                    call_args,
                    &registry,
                    None,
                    observer.as_ref(),
                    cancel.as_ref(),
                    receipt_gen.as_deref(),
                )
                .await;
                {
                    let mut count = exec_count.lock().await;
                    *count -= 1;
                    if !is_safe {
                        *has_excl.lock().await = false;
                    }
                }
                result
            });

            self.tools[i].handle = Some(handle);
        }
    }
}
