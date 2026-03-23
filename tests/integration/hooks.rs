use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use zeroclaw::hooks::{HookHandler, HookResult, HookRunner};
use zeroclaw::tools::ToolResult;

struct CounterHook {
    gateway_starts: Arc<AtomicUsize>,
    tool_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl HookHandler for CounterHook {
    fn name(&self) -> &str {
        "counter"
    }

    async fn on_gateway_start(&self, _host: &str, _port: u16) {
        self.gateway_starts.fetch_add(1, Ordering::SeqCst);
    }

    async fn on_after_tool_call(&self, _tool: &str, _result: &ToolResult, _duration: Duration) {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
    }
}

struct ToolBlocker {
    blocked_tools: Vec<String>,
}

#[async_trait]
impl HookHandler for ToolBlocker {
    fn name(&self) -> &str {
        "tool-blocker"
    }

    fn priority(&self) -> i32 {
        100
    }

    async fn before_tool_call(
        &self,
        name: String,
        args: serde_json::Value,
    ) -> HookResult<(String, serde_json::Value)> {
        if self.blocked_tools.contains(&name) {
            HookResult::Cancel(format!("{name} is blocked"))
        } else {
            HookResult::Continue((name, args))
        }
    }
}

#[tokio::test]
async fn hook_runner_full_pipeline() {
    let gateway_starts = Arc::new(AtomicUsize::new(0));
    let tool_calls = Arc::new(AtomicUsize::new(0));

    let mut runner = HookRunner::new();
    runner.register(Box::new(CounterHook {
        gateway_starts: gateway_starts.clone(),
        tool_calls: tool_calls.clone(),
    }));
    runner.register(Box::new(ToolBlocker {
        blocked_tools: vec!["dangerous".into()],
    }));

    // Void hook: fire gateway start
    runner.fire_gateway_start("127.0.0.1", 8080).await;
    assert_eq!(gateway_starts.load(Ordering::SeqCst), 1);

    // Modifying hook: safe tool passes through
    let result = runner
        .run_before_tool_call("safe_tool".into(), serde_json::json!({}))
        .await;
    assert!(!result.is_cancel());

    // Modifying hook: dangerous tool is blocked
    let result = runner
        .run_before_tool_call("dangerous".into(), serde_json::json!({}))
        .await;
    assert!(result.is_cancel());

    // Void hook: fire after tool call increments counter
    let tool_result = ToolResult {
        success: true,
        output: "ok".into(),
        error: None,
    };
    runner
        .fire_after_tool_call("safe_tool", &tool_result, Duration::from_millis(10))
        .await;
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
}
