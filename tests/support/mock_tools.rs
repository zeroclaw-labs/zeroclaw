//! Shared mock tool implementations for integration tests.

use anyhow::Result;
use async_trait::async_trait;
use jhedaiclaw::tools::{Tool, ToolResult};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// Simple tool that echoes its input argument.
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes the input message"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": {"type": "string"}
            }
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let msg = args
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("(empty)")
            .to_string();
        Ok(ToolResult {
            success: true,
            output: msg,
            error: None,
        })
    }
}

/// Tool that tracks invocation count for verifying dispatch.
pub struct CountingTool {
    count: Arc<Mutex<usize>>,
}

impl CountingTool {
    pub fn new() -> (Self, Arc<Mutex<usize>>) {
        let count = Arc::new(Mutex::new(0));
        (
            Self {
                count: count.clone(),
            },
            count,
        )
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "counter"
    }
    fn description(&self) -> &str {
        "Counts invocations"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        let mut c = self.count.lock().unwrap();
        *c += 1;
        Ok(ToolResult {
            success: true,
            output: format!("call #{}", *c),
            error: None,
        })
    }
}

/// Tool that always fails, simulating a broken external service.
pub struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "failing_tool"
    }
    fn description(&self) -> &str {
        "Always fails"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Service unavailable: connection timeout".into()),
        })
    }
}

/// Tool that captures all arguments for assertion.
pub struct RecordingTool {
    name: String,
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl RecordingTool {
    pub fn new(name: &str) -> (Self, Arc<Mutex<Vec<serde_json::Value>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                name: name.to_string(),
                calls: calls.clone(),
            },
            calls,
        )
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Records all arguments for assertion"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "input": {"type": "string"}
            }
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        self.calls.lock().unwrap().push(args.clone());
        let output = args
            .get("input")
            .and_then(|v| v.as_str())
            .unwrap_or("recorded")
            .to_string();
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}
