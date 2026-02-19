use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::hooks::traits::HookHandler;
use crate::tools::traits::ToolResult;

/// Logs tool calls for auditing.
pub struct CommandLoggerHook {
    log: Arc<Mutex<Vec<String>>>,
}

impl CommandLoggerHook {
    pub fn new() -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[cfg(test)]
    pub fn entries(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait]
impl HookHandler for CommandLoggerHook {
    fn name(&self) -> &str {
        "command-logger"
    }

    fn priority(&self) -> i32 {
        -50
    }

    async fn on_after_tool_call(&self, tool: &str, result: &ToolResult, duration: Duration) {
        let entry = format!(
            "[{}] {} ({}ms) success={}",
            chrono::Utc::now().format("%H:%M:%S"),
            tool,
            duration.as_millis(),
            result.success,
        );
        tracing::info!(hook = "command-logger", "{}", entry);
        self.log.lock().unwrap().push(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn logs_tool_calls() {
        let hook = CommandLoggerHook::new();
        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };
        hook.on_after_tool_call("shell", &result, Duration::from_millis(42))
            .await;
        let entries = hook.entries();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].contains("shell"));
        assert!(entries[0].contains("42ms"));
        assert!(entries[0].contains("success=true"));
    }
}
